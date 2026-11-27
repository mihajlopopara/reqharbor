package db

import (
	"context"
	"database/sql"
	"embed"
	"errors"
	"fmt"
	"io/fs"
	"regexp"
	"sort"
	"strconv"
	"strings"
	"time"
)

//go:embed migrations/*.sql
var migrationFS embed.FS

// Migration is one embedded NNNN_name.sql file.
type Migration struct {
	Version int
	Name    string
	SQL     string
	// ForeignKeysOff is set by a first line of `-- reqharbor:foreign-keys-off`: the migration runs
	// with foreign keys disabled (set outside the transaction) so a table rebuild can never
	// cascade-delete rows. foreign_key_check still runs before commit.
	ForeignKeysOff bool
}

var migrationName = regexp.MustCompile(`^(\d{4})_([a-z0-9_]+)\.sql$`)

const fkOffMarker = "-- reqharbor:foreign-keys-off"

// Migrations returns the embedded migrations in ascending order.
func Migrations() ([]Migration, error) { return loadMigrations(migrationFS) }

func loadMigrations(fsys fs.FS) ([]Migration, error) {
	entries, err := fs.ReadDir(fsys, "migrations")
	if err != nil {
		return nil, err
	}
	var out []Migration
	for _, e := range entries {
		m := migrationName.FindStringSubmatch(e.Name())
		if m == nil {
			return nil, fmt.Errorf("migration file %q is not named NNNN_name.sql", e.Name())
		}
		b, err := fs.ReadFile(fsys, "migrations/"+e.Name())
		if err != nil {
			return nil, err
		}
		v, _ := strconv.Atoi(m[1])
		text := string(b)
		first, _, _ := strings.Cut(text, "\n")
		out = append(out, Migration{Version: v, Name: m[2], SQL: text, ForeignKeysOff: strings.TrimSpace(first) == fkOffMarker})
	}
	sort.Slice(out, func(i, j int) bool { return out[i].Version < out[j].Version })
	for i, m := range out {
		if m.Version != i+1 {
			return nil, fmt.Errorf("migrations must be numbered 0001, 0002, … without gaps; found %04d at position %d", m.Version, i+1)
		}
	}
	return out, nil
}

// LatestVersion is the newest embedded migration.
func LatestVersion() int {
	ms, err := Migrations()
	if err != nil || len(ms) == 0 {
		return 0
	}
	return ms[len(ms)-1].Version
}

// SchemaTooNewError is the downgrade guard.
type SchemaTooNewError struct{ DB, Binary int }

func (e *SchemaTooNewError) Error() string {
	return fmt.Sprintf("database schema v%d is newer than this reqharbor-server (max v%d); use a newer image", e.DB, e.Binary)
}

const createBookkeeping = `CREATE TABLE IF NOT EXISTS schema_migrations (
  version    INTEGER PRIMARY KEY,
  name       TEXT    NOT NULL,
  applied_at INTEGER NOT NULL
)`

// CurrentVersion returns the highest applied migration (0 for a new database).
func CurrentVersion(ctx context.Context, q interface {
	QueryRowContext(context.Context, string, ...any) *sql.Row
}) (int, error) {
	var exists int
	if err := q.QueryRowContext(ctx, "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='schema_migrations'").Scan(&exists); err != nil {
		return 0, err
	}
	if exists == 0 {
		return 0, nil
	}
	var v sql.NullInt64
	if err := q.QueryRowContext(ctx, "SELECT MAX(version) FROM schema_migrations").Scan(&v); err != nil {
		return 0, err
	}
	return int(v.Int64), nil
}

// Status describes the migration state.
type Status struct {
	Current int
	Latest  int
	Pending []Migration
}

// MigrationStatus reports the applied and pending migrations, applying the downgrade guard.
func (d *DB) MigrationStatus(ctx context.Context) (*Status, error) {
	ms, err := Migrations()
	if err != nil {
		return nil, err
	}
	cur, err := CurrentVersion(ctx, d.Writer)
	if err != nil {
		return nil, err
	}
	latest := 0
	if len(ms) > 0 {
		latest = ms[len(ms)-1].Version
	}
	if cur > latest {
		return nil, &SchemaTooNewError{DB: cur, Binary: latest}
	}
	st := &Status{Current: cur, Latest: latest}
	for _, m := range ms {
		if m.Version > cur {
			st.Pending = append(st.Pending, m)
		}
	}
	return st, nil
}

// Migrate applies pending migrations up to version `to` (0 means all). now stamps applied_at.
func (d *DB) Migrate(ctx context.Context, to int, now func() time.Time) (applied []Migration, err error) {
	ms, err := Migrations()
	if err != nil {
		return nil, err
	}
	return migrate(ctx, d.Writer, ms, to, now)
}

func migrate(ctx context.Context, writer *sql.DB, ms []Migration, to int, now func() time.Time) ([]Migration, error) {
	conn, err := writer.Conn(ctx)
	if err != nil {
		return nil, err
	}
	defer conn.Close()

	cur, err := CurrentVersion(ctx, conn)
	if err != nil {
		return nil, err
	}
	latest := 0
	if len(ms) > 0 {
		latest = ms[len(ms)-1].Version
	}
	if cur > latest {
		return nil, &SchemaTooNewError{DB: cur, Binary: latest}
	}
	if to == 0 || to > latest {
		to = latest
	}
	if cur == 0 {
		// New database: incremental auto-vacuum must be set before the first table exists.
		var tables int
		if err := conn.QueryRowContext(ctx, "SELECT COUNT(*) FROM sqlite_master").Scan(&tables); err != nil {
			return nil, err
		}
		if tables == 0 {
			// journal_mode=WAL has already written the header, so the new mode takes effect only
			// after a VACUUM, which is instant on an empty file.
			if _, err := conn.ExecContext(ctx, "PRAGMA auto_vacuum=INCREMENTAL"); err != nil {
				return nil, err
			}
			if _, err := conn.ExecContext(ctx, "VACUUM"); err != nil {
				return nil, err
			}
		}
	}
	if _, err := conn.ExecContext(ctx, createBookkeeping); err != nil {
		return nil, err
	}
	var applied []Migration
	for _, m := range ms {
		if m.Version <= cur || m.Version > to {
			continue
		}
		if err := applyOne(ctx, conn, m, now()); err != nil {
			return applied, fmt.Errorf("migration %04d_%s: %w", m.Version, m.Name, err)
		}
		applied = append(applied, m)
	}
	return applied, nil
}

func applyOne(ctx context.Context, conn *sql.Conn, m Migration, now time.Time) (err error) {
	if m.ForeignKeysOff {
		if _, err := conn.ExecContext(ctx, "PRAGMA foreign_keys=OFF"); err != nil {
			return err
		}
		defer func() {
			if _, e := conn.ExecContext(context.Background(), "PRAGMA foreign_keys=ON"); e != nil && err == nil {
				err = e
			}
		}()
	}
	tx, err := conn.BeginTx(ctx, nil) // BEGIN IMMEDIATE (writer DSN _txlock=immediate)
	if err != nil {
		return err
	}
	defer func() {
		if err != nil {
			_ = tx.Rollback()
		}
	}()
	if _, err = tx.ExecContext(ctx, m.SQL); err != nil {
		return err
	}
	if _, err = tx.ExecContext(ctx, "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?, ?, ?)", m.Version, m.Name, now.UnixMilli()); err != nil {
		return err
	}
	rows, err := tx.QueryContext(ctx, "PRAGMA foreign_key_check")
	if err != nil {
		return err
	}
	bad := rows.Next()
	if err = rows.Close(); err != nil {
		return err
	}
	if bad {
		return errors.New("foreign_key_check reported violations")
	}
	return tx.Commit()
}
