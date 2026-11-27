package db

import (
	"context"
	"database/sql"
	"errors"
	"net/url"
	"os"
	"path/filepath"
	"reflect"
	"strconv"
	"strings"
	"testing"
	"time"
)

var restoreNow = time.Date(2026, 11, 27, 12, 30, 0, 0, time.UTC)

func restoreOpts(revBase int64) RestoreOptions {
	return RestoreOptions{RevBase: revBase, Now: func() time.Time { return restoreNow }}
}

// liveDB opens dataDir (migrated to `to`, 0 = latest) for seeding; the caller closes it.
func liveDB(t *testing.T, dataDir string, to int) *DB {
	t.Helper()
	d, err := Open(dataDir, Options{})
	if err != nil {
		t.Fatal(err)
	}
	if _, err := d.Migrate(context.Background(), to, time.Now); err != nil {
		d.Close()
		t.Fatal(err)
	}
	return d
}

func exec(t *testing.T, d *DB, q string, args ...any) {
	t.Helper()
	if _, err := d.Writer.Exec(q, args...); err != nil {
		t.Fatalf("%s: %v", q, err)
	}
}

// addEntities appends n entities to workspace ws with the next revs and moves its head.
func addEntities(t *testing.T, d *DB, ws string, n int) {
	t.Helper()
	var head int64
	if err := d.Writer.QueryRow("SELECT rev FROM workspaces WHERE id = ?", ws).Scan(&head); err != nil {
		t.Fatal(err)
	}
	for i := 0; i < n; i++ {
		head++
		id := ws + "-e" + strconv.FormatInt(head, 10)
		exec(t, d, "INSERT INTO entities (workspace_id, type, id, rev, updated_at, deleted, data, device_id, size) VALUES (?, 'request', ?, ?, ?, 0, '{}', 'dev', 2)", ws, id, head, 1000+head)
	}
	exec(t, d, "UPDATE workspaces SET rev = ? WHERE id = ?", head, ws)
}

func addWorkspace(t *testing.T, d *DB, ws string, entities int) {
	t.Helper()
	exec(t, d, "INSERT INTO workspaces (id, name, token_hash, rev, created_at) VALUES (?, ?, ?, 0, 1)", ws, "ws "+ws, "hash-"+ws)
	addEntities(t, d, ws, entities)
}

type revRow struct {
	ID  string
	Rev int64
}

func entityRevs(t *testing.T, d *DB, ws string) []revRow {
	t.Helper()
	rows, err := d.Reader.Query("SELECT id, rev FROM entities WHERE workspace_id = ? ORDER BY rev", ws)
	if err != nil {
		t.Fatal(err)
	}
	defer rows.Close()
	var out []revRow
	for rows.Next() {
		var r revRow
		if err := rows.Scan(&r.ID, &r.Rev); err != nil {
			t.Fatal(err)
		}
		out = append(out, r)
	}
	return out
}

func state(t *testing.T, d *DB, key string) string {
	t.Helper()
	v, err := serverState(context.Background(), d.Reader, key)
	if err != nil {
		t.Fatal(err)
	}
	return v
}

func wsRev(t *testing.T, d *DB, ws string) int64 {
	t.Helper()
	var rev int64
	if err := d.Reader.QueryRow("SELECT rev FROM workspaces WHERE id = ?", ws).Scan(&rev); err != nil {
		t.Fatalf("workspace %s: %v", ws, err)
	}
	return rev
}

func backupOf(t *testing.T, d *DB, name string) string {
	t.Helper()
	path := filepath.Join(t.TempDir(), name)
	if err := d.BackupTo(context.Background(), path); err != nil {
		t.Fatal(err)
	}
	return path
}

func noLeftovers(t *testing.T, dir string) {
	t.Helper()
	for _, s := range []string{restoreTmpSuffix, restoreTmpSuffix + "-wal", restoreTmpSuffix + "-shm"} {
		if _, err := os.Stat(filepath.Join(dir, FileName+s)); !errors.Is(err, os.ErrNotExist) {
			t.Errorf("leftover %s (%v)", FileName+s, err)
		}
	}
}

func TestRestore_RefusesWhileServeHoldsTheLock(t *testing.T) {
	dir := t.TempDir()
	d := liveDB(t, dir, 0)
	addWorkspace(t, d, "W1", 2)
	backup := backupOf(t, d, "b.db")
	d.Close()

	serving, err := Open(dir, Options{Lock: true})
	if err != nil {
		t.Fatal(err)
	}
	if _, err := Restore(context.Background(), dir, backup, restoreOpts(0)); !errors.Is(err, ErrLocked) {
		t.Fatalf("restore while serving: %v", err)
	}
	serving.Close()
	noLeftovers(t, dir)
	// Once serve stopped, the same restore works, and a second serve is refused while a restore
	// holds the lock (the lock is the same file).
	lock, err := AcquireLock(dir)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := Open(dir, Options{Lock: true}); !errors.Is(err, ErrLocked) {
		t.Fatalf("serve during restore: %v", err)
	}
	lock.Release()
	if _, err := Restore(context.Background(), dir, backup, restoreOpts(0)); err != nil {
		t.Fatal(err)
	}
}

func TestRestore_ShiftsRevsAboveTheHeadRotatesTheEpochAndKeepsTheInstance(t *testing.T) {
	ctx := context.Background()
	dir := t.TempDir()
	d := liveDB(t, dir, 0)
	exec(t, d, "INSERT INTO server_state (key, value) VALUES ('instance_id', 'INSTANCE'), ('sync_epoch', 'EPOCH1'), ('throttle_key', 'K')")
	addWorkspace(t, d, "W1", 3) // revs 1..3
	addWorkspace(t, d, "W2", 1) // rev 1
	addWorkspace(t, d, "W0", 0) // rev 0, no entities
	backup := backupOf(t, d, "nightly.db")
	// After the backup: W1 moves to head 5, W2 gets 40 more (head 41), W3 is created.
	addEntities(t, d, "W1", 2)
	addEntities(t, d, "W2", 40)
	addWorkspace(t, d, "W3", 1)
	d.Close()

	res, err := Restore(ctx, dir, backup, restoreOpts(0))
	if err != nil {
		t.Fatal(err)
	}
	r, err := Open(dir, Options{})
	if err != nil {
		t.Fatal(err)
	}
	defer r.Close()

	// base_w = max(preHead_w, restoredHead_w) + 1, added to every rev: order is preserved and every
	// restored entity is above any cursor a client could hold.
	want := map[string][]revRow{
		"W1": {{"W1-e1", 7}, {"W1-e2", 8}, {"W1-e3", 9}},
		"W2": {{"W2-e1", 43}},
		"W0": nil,
	}
	for ws, rows := range want {
		if got := entityRevs(t, r, ws); !reflect.DeepEqual(got, rows) {
			t.Errorf("%s revs %v, want %v", ws, got, rows)
		}
	}
	for ws, head := range map[string]int64{"W1": 9, "W2": 43, "W0": 1} {
		if got := wsRev(t, r, ws); got != head {
			t.Errorf("%s head %d, want %d", ws, got, head)
		}
	}
	var n int
	if err := r.Reader.QueryRow("SELECT COUNT(*) FROM workspaces WHERE id = 'W3'").Scan(&n); err != nil || n != 0 {
		t.Errorf("W3, created after the backup, still exists (%v)", err)
	}
	// The epoch rotates; instance_id and the throttle key stay; the restore is recorded.
	epoch := state(t, r, "sync_epoch")
	if epoch == "EPOCH1" || len(epoch) != 26 || epoch != res.NewEpoch || res.PreviousEpoch != "EPOCH1" {
		t.Errorf("epoch %q, result %+v", epoch, res)
	}
	if got := state(t, r, "instance_id"); got != "INSTANCE" || res.InstanceID != "INSTANCE" || res.InstanceReplaced {
		t.Errorf("instance_id %q, result %+v", got, res)
	}
	if got := state(t, r, "throttle_key"); got != "K" {
		t.Errorf("throttle_key %q", got)
	}
	if got := state(t, r, "restored_at"); got != strconv.FormatInt(restoreNow.UnixMilli(), 10) {
		t.Errorf("restored_at %q", got)
	}
	if got := state(t, r, "restored_from"); got != "nightly.db" {
		t.Errorf("restored_from %q", got)
	}
	if res.Workspaces != 3 || res.MinBase != 1 || res.MaxBase != 42 || res.Unrecorded != 0 || res.BackupVersion != 5 || res.Version != 5 {
		t.Errorf("result %+v", res)
	}
	// The previous database is kept, complete (its WAL was checkpointed), and has W3.
	wantPre := filepath.Join(dir, FileName+".pre-restore-20261127-123000")
	if res.PreRestorePath != wantPre {
		t.Fatalf("pre-restore path %q, want %q", res.PreRestorePath, wantPre)
	}
	pre := t.TempDir()
	b, err := os.ReadFile(wantPre)
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(pre, FileName), b, 0o600); err != nil {
		t.Fatal(err)
	}
	p, err := Open(pre, Options{})
	if err != nil {
		t.Fatal(err)
	}
	defer p.Close()
	if got := wsRev(t, p, "W3"); got != 1 {
		t.Errorf("pre-restore W3 head %d", got)
	}
	if got := wsRev(t, p, "W2"); got != 41 {
		t.Errorf("pre-restore W2 head %d", got)
	}
	noLeftovers(t, dir)
	if err := checkIntegrity(ctx, r.Reader, "integrity_check"); err != nil {
		t.Fatal(err)
	}

	// A second restore in the same second keeps both earlier databases.
	r.Close()
	res2, err := Restore(ctx, dir, backup, restoreOpts(0))
	if err != nil {
		t.Fatal(err)
	}
	if res2.PreRestorePath != wantPre+"-2" {
		t.Fatalf("second pre-restore path %q", res2.PreRestorePath)
	}
	if _, err := os.Stat(wantPre); err != nil {
		t.Fatal("first pre-restore file deleted")
	}
	// The pre-restore head is now the shifted one: W1 was at 9, so it moves to 9+3+1 = 13.
	r2, err := Open(dir, Options{})
	if err != nil {
		t.Fatal(err)
	}
	defer r2.Close()
	if got := entityRevs(t, r2, "W1"); got[0].Rev != 11 || wsRev(t, r2, "W1") != 13 {
		t.Errorf("second restore W1 %v head %d", got, wsRev(t, r2, "W1"))
	}
	if state(t, r2, "sync_epoch") == epoch {
		t.Error("second restore kept the epoch")
	}
}

func TestRestore_MigratesAnOlderBackupAndRefusesANewerOne(t *testing.T) {
	ctx := context.Background()
	// A v2 backup (the Worker's schema): no server_state, so no instance id of its own.
	old := t.TempDir()
	o := liveDB(t, old, 2)
	addWorkspace(t, o, "W1", 2)
	o.Close()
	oldFile := filepath.Join(old, FileName)

	dir := t.TempDir()
	d := liveDB(t, dir, 0)
	exec(t, d, "INSERT INTO server_state (key, value) VALUES ('instance_id', 'INSTANCE'), ('sync_epoch', 'EPOCH1')")
	addWorkspace(t, d, "W1", 4)
	d.Close()

	res, err := Restore(ctx, dir, oldFile, restoreOpts(0))
	if err != nil {
		t.Fatal(err)
	}
	if res.BackupVersion != 2 || res.Version != LatestVersion() {
		t.Fatalf("versions %+v", res)
	}
	r, err := Open(dir, Options{})
	if err != nil {
		t.Fatal(err)
	}
	if v, _ := CurrentVersion(ctx, r.Reader); v != LatestVersion() {
		t.Fatalf("restored at v%d", v)
	}
	if got := state(t, r, "instance_id"); got != "INSTANCE" {
		t.Errorf("instance_id %q: the current one must be kept", got)
	}
	if got := entityRevs(t, r, "W1"); len(got) != 2 || got[0].Rev != 6 || got[1].Rev != 7 {
		t.Errorf("revs %v", got)
	}
	r.Close()
	// The v2 source file is untouched (the copy was migrated, not the backup).
	src, err := Open(old, Options{})
	if err != nil {
		t.Fatal(err)
	}
	if v, _ := CurrentVersion(ctx, src.Reader); v != 2 {
		t.Errorf("backup file migrated in place to v%d", v)
	}
	src.Close()

	before, err := os.ReadFile(filepath.Join(dir, FileName))
	if err != nil {
		t.Fatal(err)
	}
	// A newer backup is refused and leaves the live database alone.
	newer := t.TempDir()
	nd := liveDB(t, newer, 0)
	exec(t, nd, "INSERT INTO schema_migrations (version, name, applied_at) VALUES (99, 'future', 0)")
	nd.Close()
	_, err = Restore(ctx, dir, filepath.Join(newer, FileName), restoreOpts(0))
	var re *RestoreError
	if !errors.As(err, &re) || !strings.Contains(err.Error(), "has schema v99, newer than this reqharbor-server") {
		t.Fatalf("newer backup: %v", err)
	}
	// So are files that are not ReqHarbor databases, and the live database itself.
	junk := filepath.Join(t.TempDir(), "junk.db")
	if err := os.WriteFile(junk, []byte(strings.Repeat("not a database ", 400)), 0o600); err != nil {
		t.Fatal(err)
	}
	empty := t.TempDir()
	e, err := Open(empty, Options{})
	if err != nil {
		t.Fatal(err)
	}
	exec(t, e, "CREATE TABLE other (x)")
	e.Close()
	for file, want := range map[string]string{
		junk:                             "cannot be opened",
		filepath.Join(empty, FileName):   "is not a ReqHarbor database",
		filepath.Join(dir, FileName):     "is the live database",
		filepath.Join(dir, "missing.db"): "cannot read backup",
	} {
		_, err := Restore(ctx, dir, file, restoreOpts(0))
		if !errors.As(err, &re) || !strings.Contains(err.Error(), want) {
			t.Errorf("%s: %v (want %q)", filepath.Base(file), err, want)
		}
	}
	after, err := os.ReadFile(filepath.Join(dir, FileName))
	if err != nil {
		t.Fatal(err)
	}
	if string(before) != string(after) {
		t.Error("a refused restore changed the live database")
	}
	if m, _ := filepath.Glob(filepath.Join(dir, FileName+".pre-restore-*")); len(m) != 1 {
		t.Errorf("pre-restore files %v (only the successful restore keeps one)", m)
	}
	noLeftovers(t, dir)
}

func TestRestore_WithoutACurrentDatabaseAndWithRevBase(t *testing.T) {
	ctx := context.Background()
	src := t.TempDir()
	d := liveDB(t, src, 0)
	exec(t, d, "INSERT INTO server_state (key, value) VALUES ('instance_id', 'INSTANCE'), ('sync_epoch', 'EPOCH1')")
	addWorkspace(t, d, "W1", 3)
	addWorkspace(t, d, "W2", 1)
	backup := backupOf(t, d, "b.db")
	d.Close()

	// Disk loss, default: max(restored revs) + 2^32 for every workspace.
	lost := t.TempDir()
	res, err := Restore(ctx, lost, backup, restoreOpts(0))
	if err != nil {
		t.Fatal(err)
	}
	if res.PreRestorePath != "" || res.Unrecorded != 2 || res.MinBase != 3+DefaultRevOffset || res.InstanceID != "INSTANCE" {
		t.Fatalf("result %+v", res)
	}
	r, err := Open(lost, Options{})
	if err != nil {
		t.Fatal(err)
	}
	if got := entityRevs(t, r, "W1"); got[0].Rev != 4+DefaultRevOffset || got[2].Rev != 6+DefaultRevOffset {
		t.Errorf("W1 %v", got)
	}
	if got := wsRev(t, r, "W2"); got != 4+DefaultRevOffset {
		t.Errorf("W2 head %d", got)
	}
	if state(t, r, "instance_id") != "INSTANCE" || state(t, r, "sync_epoch") == "EPOCH1" {
		t.Error("instance or epoch")
	}
	r.Close()

	// Disk loss with --rev-base N: N is the base of every workspace.
	lost2 := t.TempDir()
	if _, err := Restore(ctx, lost2, backup, restoreOpts(1000)); err != nil {
		t.Fatal(err)
	}
	r2, err := Open(lost2, Options{})
	if err != nil {
		t.Fatal(err)
	}
	if got := entityRevs(t, r2, "W1"); got[0].Rev != 1001 || wsRev(t, r2, "W1") != 1003 || wsRev(t, r2, "W2") != 1001 {
		t.Errorf("W1 %v", got)
	}
	r2.Close()

	// --rev-base must be above every restored head and every recorded pre-restore head.
	var re *RestoreError
	if _, err := Restore(ctx, t.TempDir(), backup, restoreOpts(3)); !errors.As(err, &re) || !strings.Contains(err.Error(), "--rev-base 3 is too low: workspace W1 had rev 3") {
		t.Errorf("rev-base at the restored head: %v", err)
	}
	if _, err := Restore(ctx, lost2, backup, restoreOpts(1003)); !errors.As(err, &re) || !strings.Contains(err.Error(), "workspace W1 had rev 1003") {
		t.Errorf("rev-base at the pre-restore head: %v", err)
	}
	if _, err := Restore(ctx, t.TempDir(), backup, restoreOpts(maxSafeRev-1)); !errors.As(err, &re) || !strings.Contains(err.Error(), "passes 2^53") {
		t.Errorf("rev-base overflow: %v", err)
	}
	if _, err := Restore(ctx, t.TempDir(), backup, restoreOpts(-1)); !errors.As(err, &re) {
		t.Errorf("negative rev-base: %v", err)
	}
	noLeftovers(t, lost2)
}

func TestRestore_AWorkspaceUnknownToTheCurrentDatabaseGoesFarAbove(t *testing.T) {
	// The current database was recreated empty (for example a fresh volume after disk loss): its
	// heads say nothing about the cursors clients hold for the backup's workspaces.
	ctx := context.Background()
	src := t.TempDir()
	d := liveDB(t, src, 0)
	addWorkspace(t, d, "W1", 2)
	backup := backupOf(t, d, "b.db")
	d.Close()
	dir := t.TempDir()
	liveDB(t, dir, 0).Close()
	res, err := Restore(ctx, dir, backup, restoreOpts(0))
	if err != nil {
		t.Fatal(err)
	}
	if res.Unrecorded != 1 || res.MinBase != 2+DefaultRevOffset || res.PreRestorePath == "" {
		t.Fatalf("result %+v", res)
	}
}

// The documented rollback: a newer server migrated the database past this binary's schema, the
// admin runs the previous image and restores the pre-migrate backup. The current database is only
// read for its heads, so its newer schema must not stop the restore.
func TestRestore_OverALiveDatabaseWithANewerSchema(t *testing.T) {
	ctx := context.Background()
	dir := t.TempDir()
	d := liveDB(t, dir, 0)
	addWorkspace(t, d, "W1", 3)
	exec(t, d, "INSERT INTO server_state (key, value) VALUES ('instance_id', 'INSTANCE'), ('sync_epoch', 'EPOCH1') ON CONFLICT (key) DO UPDATE SET value = excluded.value")
	backup := backupOf(t, d, "pre-migrate.db")
	addEntities(t, d, "W1", 4) // head 7, written by the "newer" server
	exec(t, d, "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?, 'future', 0)", LatestVersion()+1)
	d.Close()
	if _, err := Open(dir, Options{}); err == nil {
		t.Fatal("precondition: Open accepted a newer schema")
	}

	res, err := Restore(ctx, dir, backup, restoreOpts(0))
	if err != nil {
		t.Fatalf("Restore over a newer live schema: %v", err)
	}
	if res.PreRestorePath == "" || res.PreviousEpoch != "EPOCH1" || res.InstanceID != "INSTANCE" {
		t.Fatalf("result %+v", res)
	}
	if _, err := os.Stat(res.PreRestorePath); err != nil {
		t.Fatalf("pre-restore file: %v", err)
	}
	r, err := Open(dir, Options{})
	if err != nil {
		t.Fatalf("restored database: %v", err)
	}
	defer r.Close()
	if v, _ := CurrentVersion(ctx, r.Reader); v != LatestVersion() {
		t.Fatalf("restored schema v%d", v)
	}
	// Above the newer server's head of 7: base 8, so revs 9..11 and head 11.
	if got := wsRev(t, r, "W1"); got != 3+8 {
		t.Fatalf("W1 head %d, want 11", got)
	}
}

// Another process holds a read transaction on the live database, so its WAL cannot be folded into
// the file: the restore is refused and nothing changes (deleting that WAL would lose frames that
// never reached the kept pre-restore file).
func TestRestore_RefusedWhileAnotherProcessHoldsTheDatabaseOpen(t *testing.T) {
	ctx := context.Background()
	dir := t.TempDir()
	d := liveDB(t, dir, 0)
	addWorkspace(t, d, "W1", 2)
	backup := backupOf(t, d, "b.db")

	// A reader (Litestream, a backup, a CLI command) pins a snapshot; then more frames arrive.
	other, err := sqlOpenPlain(filepath.Join(dir, FileName))
	if err != nil {
		t.Fatal(err)
	}
	defer other.Close()
	tx, err := other.BeginTx(ctx, nil)
	if err != nil {
		t.Fatal(err)
	}
	var n int
	if err := tx.QueryRow("SELECT COUNT(*) FROM entities").Scan(&n); err != nil {
		t.Fatal(err)
	}
	addEntities(t, d, "W1", 5)
	d.Close() // serve stopped; the other process still has the file open

	_, err = Restore(ctx, dir, backup, restoreOpts(0))
	var re *RestoreError
	if !errors.As(err, &re) || !strings.Contains(err.Error(), "stop it first") {
		t.Fatalf("Restore with a reader holding the WAL: %v", err)
	}
	if m, _ := filepath.Glob(filepath.Join(dir, FileName+".pre-restore-*")); len(m) != 0 {
		t.Fatalf("pre-restore files %v after a refused restore", m)
	}
	noLeftovers(t, dir)
	_ = tx.Rollback()
	other.Close()

	// Nothing was lost, and once the other process is gone the restore goes through.
	cur, err := Open(dir, Options{})
	if err != nil {
		t.Fatal(err)
	}
	if got := wsRev(t, cur, "W1"); got != 7 {
		t.Fatalf("live head %d after the refused restore, want 7", got)
	}
	cur.Close()
	res, err := Restore(ctx, dir, backup, restoreOpts(0))
	if err != nil {
		t.Fatal(err)
	}
	kept, err := sqlOpenPlain(res.PreRestorePath)
	if err != nil {
		t.Fatal(err)
	}
	defer kept.Close()
	var head int64
	if err := kept.QueryRow("SELECT rev FROM workspaces WHERE id = 'W1'").Scan(&head); err != nil || head != 7 {
		t.Fatalf("pre-restore head %d %v, want 7", head, err)
	}
}

// The -wal of the live database moves along with it (step 5), so a frame that reached the WAL
// after the checkpoint (a CLI command writing next to a stopped server) stays part of the kept
// pre-restore database instead of being deleted; the -shm is dropped.
func TestRestore_SwapKeepsTheWALNextToThePreRestoreFile(t *testing.T) {
	dir := t.TempDir()
	live := filepath.Join(dir, FileName)
	tmp := live + restoreTmpSuffix
	for name, body := range map[string]string{live: "old-db", live + "-wal": "old-wal", live + "-shm": "old-shm", tmp: "restored"} {
		if err := os.WriteFile(name, []byte(body), 0o600); err != nil {
			t.Fatal(err)
		}
	}
	pre := preRestorePath(live, restoreNow)
	if err := swapIn(live, tmp, pre); err != nil {
		t.Fatal(err)
	}
	for name, want := range map[string]string{live: "restored", pre: "old-db", pre + "-wal": "old-wal"} {
		if got, err := os.ReadFile(name); err != nil || string(got) != want {
			t.Errorf("%s = %q, %v; want %q", filepath.Base(name), got, err, want)
		}
	}
	for _, gone := range []string{live + "-wal", live + "-shm", tmp} {
		if _, err := os.Stat(gone); !errors.Is(err, os.ErrNotExist) {
			t.Errorf("%s still exists (%v)", filepath.Base(gone), err)
		}
	}
	// Without a current database there is nothing to keep.
	if err := os.WriteFile(tmp, []byte("again"), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := os.Remove(live); err != nil {
		t.Fatal(err)
	}
	if err := swapIn(live, tmp, ""); err != nil {
		t.Fatal(err)
	}
	if got, _ := os.ReadFile(live); string(got) != "again" {
		t.Fatalf("live = %q", got)
	}
}

// sqlOpenPlain opens path the way another process would: a plain connection, no pool pragmas.
func sqlOpenPlain(path string) (*sql.DB, error) {
	u := url.URL{Scheme: "file", Path: path}
	c, err := sql.Open("sqlite", u.String()+"?_pragma=busy_timeout(5000)")
	if err != nil {
		return nil, err
	}
	c.SetMaxOpenConns(1)
	return c, nil
}

// swapIn's rollback branches, with injected rename failures: a failed step puts the current
// database (and its -wal) back; when that also fails, the error names where it now is.
func TestRestore_SwapRollbackOnRenameFailures(t *testing.T) {
	type step struct{ from, to string }
	cases := []struct {
		name     string
		failOn   func(s step, live, tmp, pre string) bool
		restored bool   // live holds the old database again
		where    bool   // the error names the pre-restore path
		errHas   string // part of the error
	}{
		{"wal move fails, put back", func(s step, live, _, _ string) bool { return s.from == live+"-wal" }, true, false, "keep the current database's WAL"},
		{"wal move and put back fail", func(s step, live, _, pre string) bool {
			return s.from == live+"-wal" || s.from == pre
		}, false, true, "keep the current database's WAL"},
		{"restored move fails, put back", func(s step, _, tmp, _ string) bool { return s.from == tmp }, true, false, "move the restored database into place"},
		{"restored move and put back fail", func(s step, _, tmp, pre string) bool {
			return s.from == tmp || s.from == pre
		}, false, true, "move the restored database into place"},
	}
	defer func() { rename = os.Rename }()
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			dir := t.TempDir()
			live := filepath.Join(dir, FileName)
			tmp := live + restoreTmpSuffix
			pre := preRestorePath(live, restoreNow)
			for name, body := range map[string]string{live: "old-db", live + "-wal": "old-wal", tmp: "restored"} {
				if err := os.WriteFile(name, []byte(body), 0o600); err != nil {
					t.Fatal(err)
				}
			}
			rename = func(from, to string) error {
				if c.failOn(step{from, to}, live, tmp, pre) {
					return &os.LinkError{Op: "rename", Old: from, New: to, Err: errors.New("injected")}
				}
				return os.Rename(from, to)
			}
			err := swapIn(live, tmp, pre)
			rename = os.Rename
			if err == nil || !strings.Contains(err.Error(), c.errHas) {
				t.Fatalf("swapIn: %v", err)
			}
			if got := strings.Contains(err.Error(), "it is now "+pre); got != c.where {
				t.Fatalf("error names the pre-restore path: %v, want %v: %v", got, c.where, err)
			}
			if c.restored {
				for name, want := range map[string]string{live: "old-db", live + "-wal": "old-wal", tmp: "restored"} {
					if b, err := os.ReadFile(name); err != nil || string(b) != want {
						t.Errorf("%s = %q %v, want %q", filepath.Base(name), b, err, want)
					}
				}
			} else if b, err := os.ReadFile(pre); err != nil || string(b) != "old-db" {
				t.Errorf("the current database is not at the named path: %q %v", b, err)
			}
		})
	}
}
