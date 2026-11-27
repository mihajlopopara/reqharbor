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
	"strconv"
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
	lock   *Lock
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

// Lock is the held single-instance lock on a data directory.
type Lock struct{ f *os.File }

// AcquireLock creates the data directory if needed and takes the exclusive lock on
// reqharbor.lock without opening the database. It returns ErrLocked while another process
// (serve, or a restore) holds it. serve takes it through Open; restore takes it on its own,
// because there may be no database to open.
func AcquireLock(dataDir string) (*Lock, error) {
	if err := os.MkdirAll(dataDir, 0o700); err != nil {
		return nil, fmt.Errorf("create data directory %s: %w", dataDir, err)
	}
	f, err := lockFile(filepath.Join(dataDir, LockName))
	if err != nil {
		return nil, err
	}
	return &Lock{f: f}, nil
}

// Release drops the lock.
func (l *Lock) Release() error {
	if l == nil || l.f == nil {
		return nil
	}
	err := unlockFile(l.f)
	l.f = nil
	return err
}

// Open creates the data directory if needed and opens the pools on its reqharbor.db.
func Open(dataDir string, opts Options) (*DB, error) {
	if err := os.MkdirAll(dataDir, 0o700); err != nil {
		return nil, fmt.Errorf("create data directory %s: %w", dataDir, err)
	}
	var lock *Lock
	if opts.Lock {
		var err error
		if lock, err = AcquireLock(dataDir); err != nil {
			return nil, err
		}
	}
	d, err := openPath(dataDir, filepath.Join(dataDir, FileName))
	if err != nil {
		_ = lock.Release()
		return nil, err
	}
	d.lock = lock
	return d, nil
}

// ensurePrivate creates the database file as 0600 if it does not exist yet, and makes an existing
// one and its -wal and -shm 0600. SQLite creates the -wal and -shm with the database file's
// permissions, so they follow. The database holds password and token hashes, like the backups.
func ensurePrivate(path string) error {
	f, err := os.OpenFile(path, os.O_RDWR|os.O_CREATE, 0o600)
	if err != nil {
		return fmt.Errorf("open %s: %w", path, err)
	}
	f.Close()
	for _, p := range []string{path, path + "-wal", path + "-shm"} {
		if err := os.Chmod(p, 0o600); err != nil && !errors.Is(err, os.ErrNotExist) {
			return fmt.Errorf("make %s private: %w", p, err)
		}
	}
	return nil
}

// Exists reports whether dataDir holds a database file.
func Exists(dataDir string) (bool, error) {
	_, err := os.Stat(filepath.Join(dataDir, FileName))
	if errors.Is(err, os.ErrNotExist) {
		return false, nil
	}
	return err == nil, err
}

// openPath opens the three pools on path (no lock) and applies the downgrade guard.
func openPath(dataDir, path string) (*DB, error) {
	if err := ensurePrivate(path); err != nil {
		return nil, err
	}
	d := &DB{DataDir: dataDir, Path: path}
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

// ErrCheckpointBusy is returned when PRAGMA wal_checkpoint(TRUNCATE) could not copy the whole
// WAL into the database file, because another connection (another process, such as a backup, a
// CLI command or a replication sidecar) kept a read or write transaction open.
var ErrCheckpointBusy = errors.New("the WAL checkpoint could not complete: another process has the database open")

// Checkpoint copies the WAL into the database file and truncates it (graceful shutdown, §6).
// It returns ErrCheckpointBusy unless every WAL frame reached the database file.
func (d *DB) Checkpoint(ctx context.Context) error {
	return checkpointTruncate(ctx, d.Writer)
}

// checkpointTruncate runs PRAGMA wal_checkpoint(TRUNCATE) and checks its row (busy, log,
// checkpointed): it succeeded only when busy is 0 and every frame of the log was checkpointed.
// A database that is not in WAL mode answers log = checkpointed = -1, which is fine.
func checkpointTruncate(ctx context.Context, q interface {
	QueryRowContext(context.Context, string, ...any) *sql.Row
}) error {
	var busy, log, done int64
	if err := q.QueryRowContext(ctx, "PRAGMA wal_checkpoint(TRUNCATE)").Scan(&busy, &log, &done); err != nil {
		return err
	}
	if busy != 0 || log != done {
		return fmt.Errorf("%w (busy=%d, log=%d, checkpointed=%d)", ErrCheckpointBusy, busy, log, done)
	}
	return nil
}

// WaitWriterIdle waits until no connection of the writer pool is in use (a write transaction of
// a request that outlived the shutdown budget), or until ctx ends. It reports whether the writer
// went idle.
func (d *DB) WaitWriterIdle(ctx context.Context) bool {
	t := time.NewTicker(10 * time.Millisecond)
	defer t.Stop()
	for d.Writer.Stats().InUse > 0 {
		select {
		case <-ctx.Done():
			return false
		case <-t.C:
		}
	}
	return true
}

// Maintain is the daily maintenance (§6 Background jobs): PRAGMA optimize refreshes the query
// planner statistics, and PRAGMA incremental_vacuum returns free pages to the filesystem
// (databases are created with auto_vacuum=INCREMENTAL).
func (d *DB) Maintain(ctx context.Context) error {
	if _, err := d.Writer.ExecContext(ctx, "PRAGMA optimize"); err != nil {
		return err
	}
	_, err := d.Writer.ExecContext(ctx, "PRAGMA incremental_vacuum")
	return err
}

// MaintenanceStateKey is the server_state key holding the time of the last daily maintenance
// (Unix ms), so the schedule survives restarts.
const MaintenanceStateKey = "maintenance_at"

// LastMaintenance returns when Maintain last completed; ok is false when it never has.
func (d *DB) LastMaintenance(ctx context.Context) (at time.Time, ok bool, err error) {
	var v int64
	err = d.Reader.QueryRowContext(ctx, "SELECT CAST(value AS INTEGER) FROM server_state WHERE key = ?", MaintenanceStateKey).Scan(&v)
	if errors.Is(err, sql.ErrNoRows) {
		return time.Time{}, false, nil
	}
	if err != nil {
		return time.Time{}, false, err
	}
	return time.UnixMilli(v), true, nil
}

// SetLastMaintenance records when Maintain completed.
func (d *DB) SetLastMaintenance(ctx context.Context, at time.Time) error {
	_, err := d.Writer.ExecContext(ctx, "INSERT INTO server_state (key, value) VALUES (?, ?) ON CONFLICT (key) DO UPDATE SET value = excluded.value",
		MaintenanceStateKey, strconv.FormatInt(at.UnixMilli(), 10))
	return err
}

// Close checkpoints the WAL, closes the pools and releases the lock.
func (d *DB) Close() error {
	var errs []error
	if d.Writer != nil {
		ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		_ = d.Checkpoint(ctx)
		cancel()
	}
	for _, p := range []*sql.DB{d.Reader, d.Backup, d.Writer} {
		if p != nil {
			errs = append(errs, p.Close())
		}
	}
	if d.lock != nil {
		errs = append(errs, d.lock.Release())
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
	return backupInto(ctx, d.Backup, path)
}

// BackupFile is the `backup` command: a VACUUM INTO snapshot of dataDir's database taken from a
// separate process next to a running serve. It uses one plain connection and never checkpoints,
// so it does not hold up the server's writer.
func BackupFile(ctx context.Context, dataDir, path string) error {
	ok, err := Exists(dataDir)
	if err != nil {
		return err
	}
	if !ok {
		return fmt.Errorf("no database at %s (check REQHARBOR_DATA_DIR)", filepath.Join(dataDir, FileName))
	}
	conn, err := sql.Open("sqlite", dsn(filepath.Join(dataDir, FileName), ""))
	if err != nil {
		return err
	}
	defer conn.Close()
	conn.SetMaxOpenConns(1)
	return backupInto(ctx, conn, path)
}

// backupInto runs VACUUM INTO a file inside a fresh private (0700) directory next to path, makes
// it 0600 and hard-links it to path, which fails rather than overwrite an existing file. So a
// failed or interrupted backup never leaves a partial file under the final name, nobody else can
// read the snapshot while it is written (backups hold password and token hashes), and a file
// that appears at path meanwhile is never clobbered.
func backupInto(ctx context.Context, conn *sql.DB, path string) error {
	dir := filepath.Dir(path)
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return err
	}
	if _, err := os.Lstat(path); err == nil {
		return fmt.Errorf("backup target %s already exists", path)
	}
	return snapshotAndLink(ctx, conn, path)
}

// snapshotAndLink is backupInto after its up-front existence check.
func snapshotAndLink(ctx context.Context, conn *sql.DB, path string) error {
	dir := filepath.Dir(path)
	work, err := os.MkdirTemp(dir, ".backup-*") // 0700
	if err != nil {
		return err
	}
	defer os.RemoveAll(work)
	tmp := filepath.Join(work, "backup.db")
	if _, err := conn.ExecContext(ctx, "VACUUM INTO ?", tmp); err != nil {
		return err
	}
	if err := os.Chmod(tmp, 0o600); err != nil {
		return err
	}
	if err := os.Link(tmp, path); err != nil {
		if errors.Is(err, os.ErrExist) {
			return fmt.Errorf("backup target %s already exists", path)
		}
		return err
	}
	syncDir(dir)
	return nil
}

// BackupsDir is where backups are written.
func (d *DB) BackupsDir() string { return filepath.Join(d.DataDir, "backups") }

// BackupInfo describes a verified backup file.
type BackupInfo struct {
	Path    string
	Bytes   int64
	Version int
}

// VerifyBackup opens path read-only, runs PRAGMA quick_check and reads its schema version.
func VerifyBackup(ctx context.Context, path string) (*BackupInfo, error) {
	fi, err := os.Stat(path)
	if err != nil {
		return nil, err
	}
	u := url.URL{Scheme: "file", Path: path}
	conn, err := sql.Open("sqlite", u.String()+"?mode=ro&_pragma=query_only(1)&_pragma=busy_timeout(5000)")
	if err != nil {
		return nil, err
	}
	defer conn.Close()
	if err := checkIntegrity(ctx, conn, "quick_check"); err != nil {
		return nil, fmt.Errorf("%s: %w", path, err)
	}
	v, err := CurrentVersion(ctx, conn)
	if err != nil {
		return nil, fmt.Errorf("%s: %w", path, err)
	}
	return &BackupInfo{Path: path, Bytes: fi.Size(), Version: v}, nil
}

// checkIntegrity runs PRAGMA integrity_check or quick_check; anything but a single "ok" row is
// an error naming the first problem.
func checkIntegrity(ctx context.Context, q interface {
	QueryContext(context.Context, string, ...any) (*sql.Rows, error)
}, pragma string) error {
	rows, err := q.QueryContext(ctx, "PRAGMA "+pragma)
	if err != nil {
		return err
	}
	defer rows.Close()
	var first string
	n := 0
	for rows.Next() {
		var s string
		if err := rows.Scan(&s); err != nil {
			return err
		}
		if n == 0 {
			first = s
		}
		n++
	}
	if err := rows.Err(); err != nil {
		return err
	}
	if n != 1 || first != "ok" {
		return fmt.Errorf("%s failed: %s", pragma, first)
	}
	return nil
}
