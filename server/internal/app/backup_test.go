package app_test

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"sync"
	"testing"
	"time"

	"github.com/mihajlopopara/reqharbor/server/internal/db"
	tu "github.com/mihajlopopara/reqharbor/server/internal/testutil"
)

// populated returns a harness with legacy workspaces, entities, shares, accounts with sessions
// and personal-space data, and an invite: every kind of row a backup must carry.
func populated(t *testing.T) (*tu.Harness, tu.Workspace) {
	t.Helper()
	h := tu.New(t)
	ws := h.NewWorkspace(t, "backup")
	for batch := 0; batch < 3; batch++ {
		var cs []map[string]any
		for i := 0; i < 50; i++ {
			id := fmt.Sprintf("r%03d", batch*50+i)
			cs = append(cs, tu.Change("request", id, 1795435200000+int64(i), nil))
		}
		if res := h.Call(t, "/v1/sync", tu.Req{Token: ws.Token, JSON: tu.Changes(cs...)}); res.StatusCode != 200 {
			t.Fatalf("push: %d %s", res.StatusCode, res.Body)
		}
	}
	h.Call(t, "/v1/sync", tu.Req{Token: ws.Token, JSON: tu.Changes(tu.Tombstone("request", "r000", 1795435300000))})
	createShare(t, h, ws.Token, nil, "")
	revoked := createShare(t, h, ws.Token, nil, "")
	if res := h.Call(t, "/v1/shares/"+revoked["shareId"].(string), tu.Req{Method: "DELETE", Token: ws.Token}); res.StatusCode != 200 {
		t.Fatalf("revoke: %d", res.StatusCode)
	}
	admin := h.CreateAdmin(t, "admin@example.test")
	space := h.PersonalSpace(t, admin.Access)
	if res := h.SpacePush(t, admin.Access, space, tu.Change("request", "p1", h.Clock.Now().UnixMilli(), nil)); res.StatusCode != 200 {
		t.Fatalf("space push: %d %s", res.StatusCode, res.Body)
	}
	h.CreateUser(t, "user@example.test")
	h.Invite(t, "", 3)
	return h, ws
}

func tableCounts(t *testing.T, d *db.DB) map[string]int {
	t.Helper()
	rows, err := d.Reader.Query("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
	if err != nil {
		t.Fatal(err)
	}
	var names []string
	for rows.Next() {
		var n string
		if err := rows.Scan(&n); err != nil {
			t.Fatal(err)
		}
		names = append(names, n)
	}
	rows.Close()
	out := map[string]int{}
	for _, n := range names {
		var c int
		if err := d.Reader.QueryRow("SELECT COUNT(*) FROM " + n).Scan(&c); err != nil {
			t.Fatal(err)
		}
		out[n] = c
	}
	return out
}

// openBackup opens a backup file as the database of a scratch data directory.
func openBackup(t *testing.T, path string) *db.DB {
	t.Helper()
	dir := t.TempDir()
	b, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, db.FileName), b, 0o600); err != nil {
		t.Fatal(err)
	}
	d, err := db.Open(dir, db.Options{})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { d.Close() })
	return d
}

func TestBackup_OpensAndMatchesTheRowCounts(t *testing.T) {
	h, _ := populated(t)
	path := filepath.Join(t.TempDir(), "b.db")
	if err := db.BackupFile(context.Background(), h.Config.DataDir, path); err != nil {
		t.Fatal(err)
	}
	live := tableCounts(t, h.DB)
	got := tableCounts(t, openBackup(t, path))
	if len(got) != len(live) {
		t.Fatalf("tables %v, want %v", got, live)
	}
	for table, n := range live {
		if got[table] != n {
			t.Errorf("%s: %d rows in the backup, %d live", table, got[table], n)
		}
	}
	for _, table := range []string{"entities", "shares", "users", "sessions", "teams", "workspaces", "one_time_tokens", "server_state"} {
		if live[table] == 0 {
			t.Errorf("fixture has no %s rows", table)
		}
	}
	// Same content too, not only the same number of rows.
	sum := func(d *db.DB) string {
		var s string
		if err := d.Reader.QueryRow("SELECT group_concat(workspace_id || type || id || rev || deleted || COALESCE(data, '~'), '|') FROM (SELECT * FROM entities ORDER BY workspace_id, rev)").Scan(&s); err != nil {
			t.Fatal(err)
		}
		return s
	}
	if sum(h.DB) != sum(openBackup(t, path)) {
		t.Error("backup entities differ from the live ones")
	}
}

func TestBackup_APushCompletesWhileTheBackupConnectionHoldsItsSnapshot(t *testing.T) {
	h, ws := populated(t)
	ctx := context.Background()
	// VACUUM INTO reads the whole database inside one read transaction on the backup connection.
	// Hold such a transaction open: WAL readers never block the writer, so a push must still go
	// through at once.
	conn, err := h.DB.Backup.Conn(ctx)
	if err != nil {
		t.Fatal(err)
	}
	defer conn.Close()
	if _, err := conn.ExecContext(ctx, "BEGIN"); err != nil {
		t.Fatal(err)
	}
	var before int
	if err := conn.QueryRowContext(ctx, "SELECT COUNT(*) FROM entities").Scan(&before); err != nil {
		t.Fatal(err)
	}
	done := make(chan *tu.Resp, 1)
	go func() {
		done <- h.Call(t, "/v1/sync", tu.Req{Token: ws.Token, JSON: tu.Changes(tu.Change("request", "during", 1795435400000, nil))})
	}()
	select {
	case res := <-done:
		if res.StatusCode != 200 {
			t.Fatalf("push: %d %s", res.StatusCode, res.Body)
		}
	case <-time.After(3 * time.Second):
		t.Fatal("a push waited for the backup connection's read transaction")
	}
	var during int
	if err := conn.QueryRowContext(ctx, "SELECT COUNT(*) FROM entities").Scan(&during); err != nil || during != before {
		t.Fatalf("the snapshot moved: %d -> %d (%v)", before, during, err)
	}
	if _, err := conn.ExecContext(ctx, "ROLLBACK"); err != nil {
		t.Fatal(err)
	}
	conn.Close() // the backup pool has a single connection

	// And with real backups running concurrently: every push succeeds, and each backup is a
	// consistent snapshot somewhere between the start and the end.
	start := h.Count(t, "entities", "")
	var wg sync.WaitGroup
	errs := make(chan error, 8)
	bdir := t.TempDir()
	var paths []string
	for i := 0; i < 4; i++ {
		paths = append(paths, filepath.Join(bdir, fmt.Sprintf("c%d.db", i)))
	}
	for _, p := range paths {
		wg.Add(1)
		go func() {
			defer wg.Done()
			if err := h.DB.BackupTo(ctx, p); err != nil {
				errs <- err
			}
		}()
	}
	for i := 0; i < 20; i++ {
		if res := h.Call(t, "/v1/sync", tu.Req{Token: ws.Token, JSON: tu.Changes(tu.Change("request", fmt.Sprintf("c%02d", i), 1795435500000, nil))}); res.StatusCode != 200 {
			t.Fatalf("push %d during backups: %d %s", i, res.StatusCode, res.Body)
		}
	}
	wg.Wait()
	close(errs)
	for err := range errs {
		t.Fatalf("concurrent backup: %v", err)
	}
	if end := h.Count(t, "entities", ""); end != start+20 {
		t.Fatalf("entities %d -> %d", start, end)
	}
	for _, p := range paths {
		b := openBackup(t, p)
		var n int
		var head, maxRev int64
		if err := b.Reader.QueryRow("SELECT COUNT(*) FROM entities").Scan(&n); err != nil || n < start || n > start+20 {
			t.Fatalf("%s: %d entities (%v), want %d..%d", filepath.Base(p), n, err, start, start+20)
		}
		// Consistent: the workspace head matches the entities the snapshot holds.
		if err := b.Reader.QueryRow("SELECT w.rev, (SELECT MAX(rev) FROM entities WHERE workspace_id = w.id) FROM workspaces w WHERE w.id = ?", ws.ID).Scan(&head, &maxRev); err != nil || head != maxRev {
			t.Fatalf("%s: head %d, max entity rev %d (%v)", filepath.Base(p), head, maxRev, err)
		}
	}
}
