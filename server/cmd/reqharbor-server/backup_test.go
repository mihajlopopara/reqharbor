package main

import (
	"context"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/mihajlopopara/reqharbor/server/internal/db"
)

func TestBackupCommand(t *testing.T) {
	env := cliEnvFor(t)
	w := openDB(t, env)
	if _, err := w.Exec("INSERT INTO workspaces (id, name, token_hash, rev, created_at) VALUES ('W1', 'w', 'h', 1, 1)"); err != nil {
		t.Fatal(err)
	}
	out := expect(t, env, 0, "Backup complete", "backup")
	path := strings.SplitN(out, "\n", 2)[0]
	if dir := filepath.Join(dataDir(env), "backups"); filepath.Dir(path) != dir || !strings.HasPrefix(filepath.Base(path), "manual-") || !strings.HasSuffix(path, ".db") {
		t.Fatalf("default path %q", path)
	}
	fi, err := os.Stat(path)
	if err != nil || fi.Mode().Perm() != 0o600 {
		t.Fatalf("backup file %v %v", fi, err)
	}
	info, err := db.VerifyBackup(context.Background(), path)
	if err != nil || info.Version != db.LatestVersion() {
		t.Fatalf("%+v %v", info, err)
	}

	custom := filepath.Join(t.TempDir(), "sub", "b.db")
	if got := expect(t, env, 0, "schema v5", "backup", "--out", custom); !strings.HasPrefix(got, custom+"\n") {
		t.Fatalf("--out printed %q", got)
	}
	expect(t, env, 1, "already exists", "backup", "--out", custom)
	expect(t, env, 2, "usage: reqharbor-server backup", "backup", "extra")
	if m, _ := filepath.Glob(filepath.Join(filepath.Dir(custom), "*.partial")); len(m) != 0 {
		t.Fatalf("partial files left: %v", m)
	}
	// Without a database there is nothing to back up (and none is created).
	empty := []string{"REQHARBOR_DATA_DIR=" + filepath.Join(t.TempDir(), "none")}
	expect(t, empty, 1, "no database at", "backup")
	if ok, _ := db.Exists(dataDir(empty)); ok {
		t.Fatal("backup created a database")
	}
}

func TestRestoreCommand(t *testing.T) {
	env := cliEnvFor(t)
	w := openDB(t, env)
	if _, err := w.Exec("INSERT INTO workspaces (id, name, token_hash, rev, created_at) VALUES ('W1', 'w', 'h', 1, 1)"); err != nil {
		t.Fatal(err)
	}
	if _, err := w.Exec("INSERT INTO entities (workspace_id, type, id, rev, updated_at, data, size) VALUES ('W1', 'request', 'r1', 1, 1, '{}', 2)"); err != nil {
		t.Fatal(err)
	}
	backupPath := filepath.Join(t.TempDir(), "b.db")
	expect(t, env, 0, "Backup complete", "backup", "--out", backupPath)

	expect(t, env, 2, "usage: reqharbor-server restore FILE", "restore")
	expect(t, env, 2, "usage: reqharbor-server restore FILE", "restore", backupPath, "other")

	// Refused while serve holds the data directory.
	serving, err := db.Open(dataDir(env), db.Options{Lock: true})
	if err != nil {
		t.Fatal(err)
	}
	expect(t, env, 1, "the server is running on this data directory; stop it first", "restore", backupPath)
	serving.Close()

	out := expect(t, env, 0, "Restored b.db (schema v5)", "restore", backupPath)
	for _, want := range []string{"Workspaces shifted: 1 (every rev moved up by 2 to 2", "New sync epoch: ", "reqharbor.db.pre-restore-", "must sign in again",
		"Revocations made after this backup are undone", "invite revoke",
		"removed members are members again", "Tell team admins to\n  repeat those removals"} {
		if !strings.Contains(out, want) {
			t.Errorf("summary lacks %q:\n%s", want, out)
		}
	}
	// --rev-base after FILE; too low is refused with the reason.
	expect(t, env, 1, "--rev-base 2 is too low", "restore", backupPath, "--rev-base", "2")
	expect(t, env, 0, "every rev moved up by 500 to 500", "restore", backupPath, "--rev-base", "500")
	expect(t, env, 0, "every rev moved up by 600 to 600", "restore", "--rev-base", "600", backupPath)
	d, err := db.Open(dataDir(env), db.Options{})
	if err != nil {
		t.Fatal(err)
	}
	defer d.Close()
	var rev int64
	if err := d.Reader.QueryRow("SELECT rev FROM entities WHERE id = 'r1'").Scan(&rev); err != nil || rev != 601 {
		t.Fatalf("rev %d %v", rev, err)
	}
}
