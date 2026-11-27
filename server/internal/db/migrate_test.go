package db

import (
	"context"
	"errors"
	"flag"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"testing/fstest"
	"time"
)

var update = flag.Bool("update", false, "rewrite testdata golden files")

func open(t *testing.T) *DB {
	t.Helper()
	d, err := Open(t.TempDir(), Options{})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { d.Close() })
	return d
}

// schemaDump is the golden form of sqlite_master.
func schemaDump(t *testing.T, d *DB) string {
	t.Helper()
	rows, err := d.Writer.Query("SELECT type, name, tbl_name, COALESCE(sql, '') FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' ORDER BY type, name")
	if err != nil {
		t.Fatal(err)
	}
	defer rows.Close()
	var b strings.Builder
	for rows.Next() {
		var typ, name, tbl, sql string
		if err := rows.Scan(&typ, &name, &tbl, &sql); err != nil {
			t.Fatal(err)
		}
		fmt.Fprintf(&b, "-- %s %s on %s\n%s;\n\n", typ, name, tbl, sql)
	}
	if err := rows.Err(); err != nil {
		t.Fatal(err)
	}
	return b.String()
}

func golden(t *testing.T, name, got string) {
	t.Helper()
	path := filepath.Join("testdata", name)
	if *update {
		if err := os.WriteFile(path, []byte(got), 0o644); err != nil {
			t.Fatal(err)
		}
		return
	}
	want, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("%v (run go test ./internal/db -update)", err)
	}
	if string(want) != got {
		t.Fatalf("schema differs from %s (run go test ./internal/db -update after reviewing):\n%s", path, got)
	}
}

func TestMigrate_FreshDatabaseGoldenSchemas(t *testing.T) {
	ctx := context.Background()
	d := open(t)
	if _, err := d.Migrate(ctx, 2, time.Now); err != nil {
		t.Fatal(err)
	}
	golden(t, "schema_v2.sql", schemaDump(t, d))
	applied, err := d.Migrate(ctx, 3, time.Now)
	if err != nil {
		t.Fatal(err)
	}
	if len(applied) != 1 || applied[0].Version != 3 {
		t.Fatalf("applied %v", applied)
	}
	golden(t, "schema_v3.sql", schemaDump(t, d))
	applied, err = d.Migrate(ctx, 0, time.Now)
	if err != nil {
		t.Fatal(err)
	}
	if len(applied) != 1 || applied[0].Version != 4 {
		t.Fatalf("applied %v", applied)
	}
	golden(t, "schema_v4.sql", schemaDump(t, d))

	var av int
	if err := d.Writer.QueryRow("PRAGMA auto_vacuum").Scan(&av); err != nil || av != 2 {
		t.Fatalf("auto_vacuum = %d (%v), want 2 (INCREMENTAL)", av, err)
	}
	var mode string
	if err := d.Writer.QueryRow("PRAGMA journal_mode").Scan(&mode); err != nil || mode != "wal" {
		t.Fatalf("journal_mode %q", mode)
	}
	var fk int
	if err := d.Writer.QueryRow("PRAGMA foreign_keys").Scan(&fk); err != nil || fk != 1 {
		t.Fatalf("foreign_keys %d", fk)
	}
	// Idempotent.
	if again, err := d.Migrate(ctx, 0, time.Now); err != nil || len(again) != 0 {
		t.Fatalf("re-run applied %v (%v)", again, err)
	}
	st, err := d.MigrationStatus(ctx)
	if err != nil || st.Current != 4 || st.Latest != 4 || len(st.Pending) != 0 {
		t.Fatalf("status %+v %v", st, err)
	}
}

func TestMigrate_CopiesOfWorkerMigrationsAreByteIdentical(t *testing.T) {
	for _, name := range []string{"0001_init.sql", "0002_device_id_and_quota.sql"} {
		ours, err := os.ReadFile(filepath.Join("migrations", name))
		if err != nil {
			t.Fatal(err)
		}
		theirs, err := os.ReadFile(filepath.Join("..", "..", "..", "worker", "migrations", name))
		if errors.Is(err, os.ErrNotExist) {
			t.Skip("worker/ removed (S8)")
		}
		if err != nil {
			t.Fatal(err)
		}
		if string(ours) != string(theirs) {
			t.Fatalf("%s differs from worker/migrations", name)
		}
	}
}

func TestMigrate_PopulatedV2DatabaseKeepsItsRows(t *testing.T) {
	ctx := context.Background()
	d := open(t)
	if _, err := d.Migrate(ctx, 2, time.Now); err != nil {
		t.Fatal(err)
	}
	seed := `INSERT INTO workspaces (id, name, token_hash, rev, created_at) VALUES ('W1','w','h1',2,1);
INSERT INTO entities (workspace_id, type, id, rev, updated_at, deleted, data, device_id, size) VALUES ('W1','request','r1',1,1,0,'{}','',2),('W1','request','r2',2,1,1,NULL,'',0);
INSERT INTO shares (id, workspace_id, kind, entity_id, snapshot, created_at, updated_at, revoked_at, size) VALUES ('S1','W1','request','r1','{}',1,1,NULL,2);
INSERT INTO rate_limits (key, window_start, count) VALUES ('k', 0, 1);`
	if _, err := d.Writer.Exec(seed); err != nil {
		t.Fatal(err)
	}
	if _, err := d.Migrate(ctx, 0, time.Now); err != nil {
		t.Fatal(err)
	}
	for table, want := range map[string]int{"workspaces": 1, "entities": 2, "shares": 1, "server_state": 0, "users": 0} {
		var n int
		if err := d.Writer.QueryRow("SELECT COUNT(*) FROM " + table).Scan(&n); err != nil || n != want {
			t.Fatalf("%s: %d rows (%v), want %d", table, n, err, want)
		}
	}
	var n int
	if err := d.Writer.QueryRow("SELECT COUNT(*) FROM sqlite_master WHERE name = 'rate_limits'").Scan(&n); err != nil || n != 0 {
		t.Fatal("rate_limits not dropped")
	}
	// 0004 keeps legacy rows legacy: no team, the same token hash, updated_by '' and no share
	// creator or options.
	var team, by, created, opts *string
	var hash string
	if err := d.Writer.QueryRow("SELECT team_id, token_hash FROM workspaces WHERE id = 'W1'").Scan(&team, &hash); err != nil || team != nil || hash != "h1" {
		t.Fatalf("workspace after 0004: team %v hash %q (%v)", team, hash, err)
	}
	if err := d.Writer.QueryRow("SELECT updated_by FROM entities WHERE id = 'r1'").Scan(&by); err != nil || by == nil || *by != "" {
		t.Fatalf("updated_by %v (%v)", by, err)
	}
	if err := d.Writer.QueryRow("SELECT created_by, options FROM shares WHERE id = 'S1'").Scan(&created, &opts); err != nil || created != nil || opts != nil {
		t.Fatalf("share after 0004: %v %v (%v)", created, opts, err)
	}
	rows, err := d.Writer.Query("PRAGMA foreign_key_check")
	if err != nil {
		t.Fatal(err)
	}
	if rows.Next() {
		t.Fatal("foreign_key_check reported violations")
	}
	rows.Close()
}

func TestMigrate_DowngradeGuard(t *testing.T) {
	dir := t.TempDir()
	d, err := Open(dir, Options{})
	if err != nil {
		t.Fatal(err)
	}
	if _, err := d.Migrate(context.Background(), 0, time.Now); err != nil {
		t.Fatal(err)
	}
	if _, err := d.Writer.Exec("INSERT INTO schema_migrations (version, name, applied_at) VALUES (99, 'future', 0)"); err != nil {
		t.Fatal(err)
	}
	d.Close()
	_, err = Open(dir, Options{})
	var tooNew *SchemaTooNewError
	if !errors.As(err, &tooNew) || err.Error() != "database schema v99 is newer than this reqharbor-server (max v4); use a newer image" {
		t.Fatalf("Open: %v", err)
	}
}

func TestMigrate_FailedMigrationRollsBack(t *testing.T) {
	ctx := context.Background()
	d := open(t)
	ms, err := loadMigrations(fstest.MapFS{
		"migrations/0001_a.sql": {Data: []byte("CREATE TABLE a (x INTEGER);")},
		"migrations/0002_b.sql": {Data: []byte("CREATE TABLE b (x INTEGER); INSERT INTO nope VALUES (1);")},
	})
	if err != nil {
		t.Fatal(err)
	}
	applied, err := migrate(ctx, d.Writer, ms, 0, time.Now)
	if err == nil || len(applied) != 1 {
		t.Fatalf("applied %v err %v", applied, err)
	}
	var n int
	_ = d.Writer.QueryRow("SELECT COUNT(*) FROM sqlite_master WHERE name = 'b'").Scan(&n)
	if n != 0 {
		t.Fatal("table b survived the failed migration")
	}
	if v, _ := CurrentVersion(ctx, d.Writer); v != 1 {
		t.Fatalf("version %d", v)
	}
}

func TestMigrate_MultiStatementFilesRunEveryStatement(t *testing.T) {
	ctx := context.Background()
	d := open(t)
	ms, _ := loadMigrations(fstest.MapFS{
		"migrations/0001_a.sql": {Data: []byte("CREATE TABLE a (x INTEGER);\n-- comment; with a semicolon\nINSERT INTO a VALUES (1);\nINSERT INTO a VALUES (2);\n")},
	})
	if _, err := migrate(ctx, d.Writer, ms, 0, time.Now); err != nil {
		t.Fatal(err)
	}
	var n int
	if err := d.Writer.QueryRow("SELECT COUNT(*) FROM a").Scan(&n); err != nil || n != 2 {
		t.Fatalf("%d rows (%v)", n, err)
	}
}

func TestMigrate_ForeignKeysOffMigrationsAndCheck(t *testing.T) {
	ctx := context.Background()
	d := open(t)
	ms, _ := loadMigrations(fstest.MapFS{
		"migrations/0001_a.sql": {Data: []byte("CREATE TABLE p (id INTEGER PRIMARY KEY); CREATE TABLE c (pid INTEGER REFERENCES p(id) ON DELETE CASCADE); INSERT INTO p VALUES (1); INSERT INTO c VALUES (1);")},
		// A rebuild of p with foreign keys off must not cascade-delete c's rows.
		"migrations/0002_rebuild.sql": {Data: []byte(fkOffMarker + "\nCREATE TABLE p2 (id INTEGER PRIMARY KEY); INSERT INTO p2 SELECT id FROM p; DROP TABLE p; ALTER TABLE p2 RENAME TO p;")},
	})
	if !ms[1].ForeignKeysOff || ms[0].ForeignKeysOff {
		t.Fatal("marker not detected")
	}
	if _, err := migrate(ctx, d.Writer, ms, 0, time.Now); err != nil {
		t.Fatal(err)
	}
	var n, fk int
	if err := d.Writer.QueryRow("SELECT COUNT(*) FROM c").Scan(&n); err != nil || n != 1 {
		t.Fatalf("child rows %d (%v)", n, err)
	}
	if err := d.Writer.QueryRow("PRAGMA foreign_keys").Scan(&fk); err != nil || fk != 1 {
		t.Fatal("foreign keys not restored")
	}
	// A migration that leaves a dangling reference is refused by foreign_key_check.
	ms2, _ := loadMigrations(fstest.MapFS{
		"migrations/0001_a.sql": {Data: []byte("CREATE TABLE p (id INTEGER PRIMARY KEY); CREATE TABLE c (pid INTEGER REFERENCES p(id));")},
		"migrations/0002_b.sql": {Data: []byte(fkOffMarker + "\nINSERT INTO c VALUES (42);")},
	})
	d2 := open(t)
	if _, err := migrate(ctx, d2.Writer, ms2, 0, time.Now); err == nil || !strings.Contains(err.Error(), "foreign_key_check") {
		t.Fatalf("got %v", err)
	}
}

func TestMigrate_FileNamesAndGaps(t *testing.T) {
	if _, err := loadMigrations(fstest.MapFS{"migrations/1_a.sql": {}}); err == nil {
		t.Error("bad name accepted")
	}
	if _, err := loadMigrations(fstest.MapFS{"migrations/0001_a.sql": {}, "migrations/0003_c.sql": {}}); err == nil {
		t.Error("gap accepted")
	}
}

func TestOpen_SingleInstanceLock(t *testing.T) {
	dir := t.TempDir()
	d, err := Open(dir, Options{Lock: true})
	if err != nil {
		t.Fatal(err)
	}
	if _, err := Open(dir, Options{Lock: true}); !errors.Is(err, ErrLocked) {
		t.Fatalf("second lock: %v", err)
	}
	// CLI subcommands do not take the lock.
	cli, err := Open(dir, Options{})
	if err != nil {
		t.Fatal(err)
	}
	cli.Close()
	d.Close()
	d2, err := Open(dir, Options{Lock: true})
	if err != nil {
		t.Fatalf("lock not released: %v", err)
	}
	d2.Close()
}

func TestBackupTo_ProducesAnOpenableCopy(t *testing.T) {
	ctx := context.Background()
	d := open(t)
	if _, err := d.Migrate(ctx, 0, time.Now); err != nil {
		t.Fatal(err)
	}
	if _, err := d.Writer.Exec("INSERT INTO server_state (key, value) VALUES ('k', 'v')"); err != nil {
		t.Fatal(err)
	}
	path := filepath.Join(d.BackupsDir(), "b.db")
	if err := d.BackupTo(ctx, path); err != nil {
		t.Fatal(err)
	}
	if err := d.BackupTo(ctx, path); err == nil {
		t.Fatal("overwrote an existing backup")
	}
	b, err := Open(filepath.Join(t.TempDir(), "restored"), Options{})
	if err != nil {
		t.Fatal(err)
	}
	b.Close()
	bdir := filepath.Dir(path)
	if err := os.Rename(path, filepath.Join(bdir, FileName)); err != nil {
		t.Fatal(err)
	}
	c, err := Open(bdir, Options{})
	if err != nil {
		t.Fatal(err)
	}
	defer c.Close()
	var v string
	if err := c.Reader.QueryRow("SELECT value FROM server_state WHERE key = 'k'").Scan(&v); err != nil || v != "v" {
		t.Fatalf("backup content %q %v", v, err)
	}
	if size, err := d.SizeBytes(ctx); err != nil || size <= 0 {
		t.Fatalf("size %d %v", size, err)
	}
	if free, err := d.FreeDiskBytes(); err != nil || free <= 0 {
		t.Fatalf("free %d %v", free, err)
	}
}

func TestReaderIsQueryOnly(t *testing.T) {
	d := open(t)
	if _, err := d.Migrate(context.Background(), 0, time.Now); err != nil {
		t.Fatal(err)
	}
	if _, err := d.Reader.Exec("INSERT INTO server_state (key, value) VALUES ('x', 'y')"); err == nil {
		t.Fatal("reader pool can write")
	}
}
