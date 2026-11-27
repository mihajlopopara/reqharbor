package app_test

import (
	"context"
	"fmt"
	"log/slog"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/mihajlopopara/reqharbor/server/internal/db"
	"github.com/mihajlopopara/reqharbor/server/internal/jobs"
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

// timedBackuper records when the scheduled backup's VACUUM INTO ran.
type timedBackuper struct {
	*db.DB
	mu         sync.Mutex
	start, end time.Time
	finished   chan struct{}
}

func (b *timedBackuper) BackupTo(ctx context.Context, path string) error {
	b.mu.Lock()
	b.start = time.Now()
	b.mu.Unlock()
	err := b.DB.BackupTo(ctx, path)
	b.mu.Lock()
	b.end = time.Now()
	b.mu.Unlock()
	close(b.finished)
	return err
}

func TestBackup_APushCompletesDuringAScheduledBackup(t *testing.T) {
	h, ws := populated(t)
	// About 30 MB in another workspace, so the snapshot takes long enough to push meanwhile.
	bulk := h.NewWorkspace(t, "bulk")
	h.MustExec(t, `WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM n WHERE x < 8000)
		INSERT INTO entities (workspace_id, type, id, rev, updated_at, deleted, data)
		SELECT ?, 'request', 'bulk' || x, x, 1795435200000, 0, '{"pad":"' || hex(randomblob(1900)) || '"}' FROM n`, bulk.ID)
	h.MustExec(t, "UPDATE workspaces SET rev = 8000 WHERE id = ?", bulk.ID)

	b := &timedBackuper{DB: h.DB, finished: make(chan struct{})}
	ctx, cancel := context.WithCancel(context.Background())
	stopped := make(chan struct{})
	logs := &tu.Buffer{}
	// The real scheduled job: with no scheduled backup yet, the first one runs at once.
	go func() {
		defer close(stopped)
		jobs.RunBackups(ctx, b, h.Clock, 24*time.Hour, 7, slog.New(slog.NewJSONHandler(logs, nil)))
	}()
	type push struct{ start, end time.Time }
	var pushes []push
	start := h.Count(t, "entities", "workspace_id = ?", ws.ID)
	for i := 0; ; i++ {
		select {
		case <-b.finished:
		default:
			p := push{start: time.Now()}
			res := h.Call(t, "/v1/sync", tu.Req{Token: ws.Token, JSON: tu.Changes(tu.Change("request", fmt.Sprintf("sched%04d", i), 1795435600000, nil))})
			p.end = time.Now()
			if res.StatusCode != 200 {
				t.Fatalf("push %d during the scheduled backup: %d %s", i, res.StatusCode, res.Body)
			}
			pushes = append(pushes, p)
			continue
		}
		break
	}
	// Wait for the job to log the backup, then stop it.
	deadline := time.Now().Add(10 * time.Second)
	for !strings.Contains(logs.String(), `"msg":"scheduled backup"`) && time.Now().Before(deadline) {
		time.Sleep(10 * time.Millisecond)
	}
	cancel()
	<-stopped
	b.mu.Lock()
	bStart, bEnd := b.start, b.end
	b.mu.Unlock()
	during := 0
	for _, p := range pushes {
		if p.start.After(bStart) && p.end.Before(bEnd) {
			during++
		}
	}
	if during == 0 {
		t.Fatalf("no push completed during the %v backup (%d pushes)", bEnd.Sub(bStart), len(pushes))
	}
	list, err := jobs.ScheduledBackups(h.DB.BackupsDir())
	if err != nil || len(list) != 1 || !strings.Contains(logs.String(), `"msg":"scheduled backup"`) {
		t.Fatalf("scheduled backups %v %v: %s", list, err, logs)
	}
	snap := openBackup(t, filepath.Join(h.DB.BackupsDir(), list[0].Name))
	var n int
	var head, maxRev int64
	if err := snap.Reader.QueryRow("SELECT COUNT(*) FROM entities WHERE workspace_id = ?", ws.ID).Scan(&n); err != nil || n < start || n > start+len(pushes) {
		t.Fatalf("snapshot has %d entities (%v), want %d..%d", n, err, start, start+len(pushes))
	}
	if err := snap.Reader.QueryRow("SELECT w.rev, (SELECT MAX(rev) FROM entities WHERE workspace_id = w.id) FROM workspaces w WHERE w.id = ?", ws.ID).Scan(&head, &maxRev); err != nil || head != maxRev {
		t.Fatalf("inconsistent snapshot: head %d, max rev %d (%v)", head, maxRev, err)
	}
	if got := h.Count(t, "entities", "workspace_id = ?", ws.ID); got != start+len(pushes) {
		t.Fatalf("live entities %d, want %d", got, start+len(pushes))
	}
	t.Logf("%d of %d pushes completed during the %v scheduled backup", during, len(pushes), bEnd.Sub(bStart))
}
