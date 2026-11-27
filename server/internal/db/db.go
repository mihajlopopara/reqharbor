// Package db opens the SQLite database (writer, reader and backup pools), holds the
// single-instance lock and runs the embedded migrations (06-go-server.md §2.1, §8.1).
package db

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"net/url"
	"os"
	"path/filepath"
	"time"

	_ "modernc.org/sqlite" // registers the "sqlite" driver
)

// FileName is the database file inside the data directory.
const FileName = "reqharbor.db"

// LockName is the single-instance lock file inside the data directory.
const LockName = "reqharbor.lock"

// ErrLocked is returned when another serve process holds the data directory.
var ErrLocked = errors.New("another reqharbor-server is using this data directory")

const basePragmas = "_pragma=foreign_keys(1)&_pragma=busy_timeout(5000)&_pragma=journal_mode(WAL)&_pragma=synchronous(FULL)&_pragma=temp_store(MEMORY)"

// DB holds the three connection pools on one database file.
type DB struct {
	Path    string
	DataDir string
	// Writer is the single writer connection; transactions begin IMMEDIATE.
	Writer *sql.DB
	// Reader is a pool of query_only connections.
	Reader *sql.DB
	// Backup is used only for VACUUM INTO; it never writes the live database.
	Backup *sql.DB
	lock   *os.File
}

// Options for Open.
type Options struct {
	// Lock takes the exclusive single-instance lock (serve does; CLI subcommands do not).
	Lock bool
}

func dsn(path, extra string) string {
	u := url.URL{Scheme: "file", Path: path}
	q := basePragmas
	if extra != "" {
		q += "&" + extra
	}
	return u.String() + "?" + q
}

// Open creates the data directory if needed and opens the pools.
func Open(dataDir string, opts Options) (*DB, error) {
	if err := os.MkdirAll(dataDir, 0o700); err != nil {
		return nil, fmt.Errorf("create data directory %s: %w", dataDir, err)
	}
	d := &DB{DataDir: dataDir, Path: filepath.Join(dataDir, FileName)}
	if opts.Lock {
		f, err := lockFile(filepath.Join(dataDir, LockName))
		if err != nil {
			return nil, err
		}
		d.lock = f
	}
	var err error
	// The writer opens first so journal_mode=WAL is persisted before any reader connects.
	if d.Writer, err = sql.Open("sqlite", dsn(d.Path, "_txlock=immediate")); err != nil {
		d.Close()
		return nil, err
	}
	d.Writer.SetMaxOpenConns(1)
	d.Writer.SetMaxIdleConns(1)
	d.Writer.SetConnMaxLifetime(0)
	if err = d.Writer.Ping(); err != nil {
		d.Close()
		return nil, fmt.Errorf("open %s: %w", d.Path, err)
	}
	if d.Reader, err = sql.Open("sqlite", dsn(d.Path, "_pragma=query_only(1)")); err != nil {
		d.Close()
		return nil, err
	}
	d.Reader.SetMaxOpenConns(8)
	d.Reader.SetMaxIdleConns(8)
	if d.Backup, err = sql.Open("sqlite", dsn(d.Path, "")); err != nil {
		d.Close()
		return nil, err
	}
	d.Backup.SetMaxOpenConns(1)
	// Downgrade guard: never touch a database written by a newer server.
	cur, err := CurrentVersion(context.Background(), d.Writer)
	if err != nil {
		d.Close()
		return nil, fmt.Errorf("read schema version of %s: %w", d.Path, err)
	}
	if latest := LatestVersion(); cur > latest {
		d.Close()
		return nil, &SchemaTooNewError{DB: cur, Binary: latest}
	}
	return d, nil
}

// Close checkpoints the WAL, closes the pools and releases the lock.
func (d *DB) Close() error {
	var errs []error
	if d.Writer != nil {
		ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		_, _ = d.Writer.ExecContext(ctx, "PRAGMA wal_checkpoint(TRUNCATE)")
		cancel()
	}
	for _, p := range []*sql.DB{d.Reader, d.Backup, d.Writer} {
		if p != nil {
			errs = append(errs, p.Close())
		}
	}
	if d.lock != nil {
		errs = append(errs, unlockFile(d.lock))
		d.lock = nil
	}
	return errors.Join(errs...)
}

// Ping runs SELECT 1 on the writer and reader pools.
func (d *DB) Ping(ctx context.Context) error {
	for _, p := range []*sql.DB{d.Writer, d.Reader} {
		var one int
		if err := p.QueryRowContext(ctx, "SELECT 1").Scan(&one); err != nil {
			return err
		}
	}
	return nil
}

// SizeBytes returns page_count * page_size of the live database.
func (d *DB) SizeBytes(ctx context.Context) (int64, error) {
	var pages, size int64
	if err := d.Reader.QueryRowContext(ctx, "PRAGMA page_count").Scan(&pages); err != nil {
		return 0, err
	}
	if err := d.Reader.QueryRowContext(ctx, "PRAGMA page_size").Scan(&size); err != nil {
		return 0, err
	}
	return pages * size, nil
}

// FreeDiskBytes returns the free space available to the server in the data directory.
func (d *DB) FreeDiskBytes() (int64, error) { return freeBytes(d.DataDir) }

// BackupTo writes a consistent snapshot of the live database to path with VACUUM INTO on the
// backup connection. In WAL mode this does not block the writer.
func (d *DB) BackupTo(ctx context.Context, path string) error {
	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		return err
	}
	if _, err := os.Stat(path); err == nil {
		return fmt.Errorf("backup target %s already exists", path)
	}
	_, err := d.Backup.ExecContext(ctx, "VACUUM INTO ?", path)
	return err
}

// BackupsDir is where backups are written.
func (d *DB) BackupsDir() string { return filepath.Join(d.DataDir, "backups") }
