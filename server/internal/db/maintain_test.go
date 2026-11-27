package db

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func TestMaintain_IncrementalVacuumReturnsFreePages(t *testing.T) {
	ctx := context.Background()
	d := open(t)
	if _, err := d.Migrate(ctx, 0, time.Now); err != nil {
		t.Fatal(err)
	}
	if _, err := d.Writer.Exec("INSERT INTO workspaces (id, name, token_hash, rev, created_at) VALUES ('W', 'w', 'h', 0, 1)"); err != nil {
		t.Fatal(err)
	}
	big := strings.Repeat("x", 64<<10)
	for i := range 40 {
		if _, err := d.Writer.Exec("INSERT INTO entities (workspace_id, type, id, rev, updated_at, data, size) VALUES ('W', 'request', ?, ?, 1, ?, ?)", i, i+1, big, len(big)); err != nil {
			t.Fatal(err)
		}
	}
	if _, err := d.Writer.Exec("DELETE FROM entities"); err != nil {
		t.Fatal(err)
	}
	if err := d.Checkpoint(ctx); err != nil {
		t.Fatal(err)
	}
	free := func() int {
		var n int
		if err := d.Writer.QueryRow("PRAGMA freelist_count").Scan(&n); err != nil {
			t.Fatal(err)
		}
		return n
	}
	if free() == 0 {
		t.Fatal("no free pages after the delete")
	}
	if err := d.Maintain(ctx); err != nil {
		t.Fatal(err)
	}
	if n := free(); n != 0 {
		t.Fatalf("%d free pages after Maintain", n)
	}
}

func TestVerifyBackup(t *testing.T) {
	ctx := context.Background()
	d := open(t)
	if _, err := d.Migrate(ctx, 0, time.Now); err != nil {
		t.Fatal(err)
	}
	path := filepath.Join(d.BackupsDir(), "manual.db")
	if err := d.BackupTo(ctx, path); err != nil {
		t.Fatal(err)
	}
	info, err := VerifyBackup(ctx, path)
	if err != nil || info.Version != LatestVersion() || info.Bytes <= 0 {
		t.Fatalf("%+v %v", info, err)
	}
	// Verifying opens read-only: no WAL or shm files appear next to the backup.
	for _, s := range []string{"-wal", "-shm"} {
		if _, err := os.Stat(path + s); err == nil {
			t.Errorf("%s created", s)
		}
	}
	junk := filepath.Join(t.TempDir(), "junk.db")
	if err := os.WriteFile(junk, []byte(strings.Repeat("junk", 1024)), 0o600); err != nil {
		t.Fatal(err)
	}
	if _, err := VerifyBackup(ctx, junk); err == nil {
		t.Fatal("junk verified")
	}
}

// A backup is written inside a private temporary directory and linked into place: the result is
// 0600, no temporary file or directory is left behind, and an existing target (even one that
// appears while the snapshot is written) is never overwritten.
func TestBackupFile_PrivateAndNeverOverwrites(t *testing.T) {
	ctx := context.Background()
	d := open(t)
	if _, err := d.Migrate(ctx, 0, time.Now); err != nil {
		t.Fatal(err)
	}
	out := filepath.Join(t.TempDir(), "backups")
	path := filepath.Join(out, "a.db")
	if err := BackupFile(ctx, d.DataDir, path); err != nil {
		t.Fatal(err)
	}
	fi, err := os.Stat(path)
	if err != nil {
		t.Fatal(err)
	}
	if fi.Mode().Perm() != 0o600 {
		t.Fatalf("backup mode %v, want 0600", fi.Mode().Perm())
	}
	if entries, _ := os.ReadDir(out); len(entries) != 1 {
		t.Fatalf("backups dir holds %v, want only a.db", entries)
	}
	if _, err := VerifyBackup(ctx, path); err != nil {
		t.Fatal(err)
	}

	// An existing target is refused up front, and its content kept.
	existing := filepath.Join(out, "b.db")
	if err := os.WriteFile(existing, []byte("keep me"), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := BackupFile(ctx, d.DataDir, existing); err == nil || !strings.Contains(err.Error(), "already exists") {
		t.Fatalf("backup over an existing file: %v", err)
	}
	// A target that appears after the up-front check (a concurrent writer) is not clobbered
	// either: the final link fails instead of replacing it.
	racy := filepath.Join(out, "c.db")
	conn, err := sqlOpenPlain(d.Path)
	if err != nil {
		t.Fatal(err)
	}
	defer conn.Close()
	if err := os.WriteFile(racy, []byte("arrived meanwhile"), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := snapshotAndLink(ctx, conn, racy); err == nil || !strings.Contains(err.Error(), "already exists") {
		t.Fatalf("final link over a file that appeared meanwhile: %v", err)
	}
	for p, want := range map[string]string{existing: "keep me", racy: "arrived meanwhile"} {
		if b, _ := os.ReadFile(p); string(b) != want {
			t.Fatalf("%s overwritten: %q", p, b)
		}
	}
	if entries, _ := os.ReadDir(out); len(entries) != 3 {
		t.Fatalf("backups dir holds %v (a temporary directory left behind?)", entries)
	}
}

// Checkpoint reports a WAL it could not fold into the file because another connection pins it.
func TestCheckpoint_ReportsABusyWAL(t *testing.T) {
	ctx := context.Background()
	d := open(t)
	if _, err := d.Migrate(ctx, 0, time.Now); err != nil {
		t.Fatal(err)
	}
	if err := d.Checkpoint(ctx); err != nil {
		t.Fatalf("idle checkpoint: %v", err)
	}
	other, err := sqlOpenPlain(d.Path)
	if err != nil {
		t.Fatal(err)
	}
	defer other.Close()
	tx, err := other.BeginTx(ctx, nil)
	if err != nil {
		t.Fatal(err)
	}
	defer tx.Rollback()
	var n int
	if err := tx.QueryRow("SELECT COUNT(*) FROM workspaces").Scan(&n); err != nil {
		t.Fatal(err)
	}
	if _, err := d.Writer.Exec("INSERT INTO workspaces (id, name, token_hash, rev, created_at) VALUES ('W', 'w', 'h', 0, 1)"); err != nil {
		t.Fatal(err)
	}
	short, cancel := context.WithTimeout(ctx, 30*time.Second)
	defer cancel()
	if err := d.Checkpoint(short); !errors.Is(err, ErrCheckpointBusy) {
		t.Fatalf("checkpoint with a pinned WAL: %v", err)
	}
	_ = tx.Rollback()
	if err := d.Checkpoint(ctx); err != nil {
		t.Fatalf("checkpoint after the reader left: %v", err)
	}
}

func TestLastMaintenance_RoundTrip(t *testing.T) {
	ctx := context.Background()
	d := open(t)
	if _, err := d.Migrate(ctx, 0, time.Now); err != nil {
		t.Fatal(err)
	}
	if _, ok, err := d.LastMaintenance(ctx); ok || err != nil {
		t.Fatalf("fresh database: ok=%v err=%v", ok, err)
	}
	at := time.UnixMilli(1_790_000_000_123)
	for _, when := range []time.Time{at, at.Add(time.Hour)} {
		if err := d.SetLastMaintenance(ctx, when); err != nil {
			t.Fatal(err)
		}
		got, ok, err := d.LastMaintenance(ctx)
		if err != nil || !ok || !got.Equal(when) {
			t.Fatalf("LastMaintenance = %v %v %v, want %v", got, ok, err, when)
		}
	}
}

// The database holds password and token hashes: a fresh one, and its -wal and -shm, are 0600
// whatever the umask, and an existing world-readable one is made private on Open.
func TestOpen_DatabaseFilesArePrivate(t *testing.T) {
	dir := t.TempDir()
	d, err := Open(dir, Options{})
	if err != nil {
		t.Fatal(err)
	}
	if _, err := d.Migrate(context.Background(), 0, time.Now); err != nil {
		t.Fatal(err)
	}
	check := func(when string) {
		t.Helper()
		for _, name := range []string{FileName, FileName + "-wal", FileName + "-shm"} {
			fi, err := os.Stat(filepath.Join(dir, name))
			if err != nil {
				t.Fatalf("%s: %s: %v", when, name, err)
			}
			if fi.Mode().Perm() != 0o600 {
				t.Errorf("%s: %s mode %v, want 0600", when, name, fi.Mode().Perm())
			}
		}
	}
	check("fresh")
	for _, name := range []string{FileName, FileName + "-wal", FileName + "-shm"} {
		if err := os.Chmod(filepath.Join(dir, name), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	d2, err := Open(dir, Options{})
	if err != nil {
		t.Fatal(err)
	}
	defer d2.Close()
	check("reopened")
	d.Close()
	// A data directory Open creates is private too.
	sub := filepath.Join(t.TempDir(), "data")
	d3, err := Open(sub, Options{})
	if err != nil {
		t.Fatal(err)
	}
	d3.Close()
	if fi, err := os.Stat(sub); err != nil || fi.Mode().Perm() != 0o700 {
		t.Fatalf("data dir mode %v %v, want 0700", fi.Mode().Perm(), err)
	}
}
