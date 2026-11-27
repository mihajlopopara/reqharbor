package jobs_test

import (
	"context"
	"errors"
	"log/slog"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/mihajlopopara/reqharbor/server/internal/clock"
	"github.com/mihajlopopara/reqharbor/server/internal/db"
	"github.com/mihajlopopara/reqharbor/server/internal/jobs"
)

func openDB(t *testing.T) *db.DB {
	t.Helper()
	d, err := db.Open(t.TempDir(), db.Options{})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { d.Close() })
	if _, err := d.Migrate(context.Background(), 0, time.Now); err != nil {
		t.Fatal(err)
	}
	return d
}

func names(t *testing.T, dir string) []string {
	t.Helper()
	entries, err := os.ReadDir(dir)
	if err != nil {
		t.Fatal(err)
	}
	var out []string
	for _, e := range entries {
		out = append(out, e.Name())
	}
	sort.Strings(out)
	return out
}

var t0 = time.Date(2026, 11, 27, 12, 0, 0, 0, time.UTC)

func TestScheduledBackup_NamesVerifiesAndPrunesOnlyItsOwnFiles(t *testing.T) {
	d := openDB(t)
	dir := d.BackupsDir()
	if err := os.MkdirAll(dir, 0o700); err != nil {
		t.Fatal(err)
	}
	// Files the job must never touch, some of them older than every scheduled backup.
	others := []string{"manual-20200101-000000.db", "pre-migrate-v4-v5-20200101-000000.db", "reqharbor-latest.db",
		"reqharbor-20200101-000000.db.bak", "notes.txt", "reqharbor-2020-01-01.db"}
	for _, n := range others {
		if err := os.WriteFile(filepath.Join(dir, n), []byte("x"), 0o600); err != nil {
			t.Fatal(err)
		}
	}
	ctx := context.Background()
	var made []string
	for i := 0; i < 4; i++ {
		res, err := jobs.RunScheduledBackup(ctx, d, t0.Add(time.Duration(i)*24*time.Hour), 2)
		if err != nil {
			t.Fatal(err)
		}
		made = append(made, filepath.Base(res.Path))
		if res.Bytes <= 0 {
			t.Fatalf("bytes %d", res.Bytes)
		}
		fi, err := os.Stat(res.Path)
		if err != nil || fi.Mode().Perm() != 0o600 {
			t.Fatalf("%s: %v %v", res.Path, fi.Mode(), err)
		}
		if info, err := db.VerifyBackup(ctx, res.Path); err != nil || info.Version < 5 {
			t.Fatalf("verify: %+v %v", info, err)
		}
		if i >= 2 && (len(res.Pruned) != 1 || res.Pruned[0] != made[i-2]) {
			t.Fatalf("run %d pruned %v", i, res.Pruned)
		}
	}
	if made[0] != "reqharbor-20261127-120000.db" || made[3] != "reqharbor-20261130-120000.db" {
		t.Fatalf("names %v", made)
	}
	want := append(append([]string(nil), others...), made[2], made[3])
	sort.Strings(want)
	if got := names(t, dir); strings.Join(got, ",") != strings.Join(want, ",") {
		t.Fatalf("backups dir\n got %v\nwant %v", got, want)
	}
}

func TestScheduledBackup_SameSecondGetsASuffixAndPrunesInOrder(t *testing.T) {
	d := openDB(t)
	ctx := context.Background()
	var made []string
	for i := 0; i < 3; i++ {
		res, err := jobs.RunScheduledBackup(ctx, d, t0, 10)
		if err != nil {
			t.Fatal(err)
		}
		made = append(made, filepath.Base(res.Path))
	}
	if strings.Join(made, ",") != "reqharbor-20261127-120000.db,reqharbor-20261127-120000-2.db,reqharbor-20261127-120000-3.db" {
		t.Fatalf("names %v", made)
	}
	list, err := jobs.ScheduledBackups(d.BackupsDir())
	if err != nil || len(list) != 3 || list[0].Name != made[0] || list[2].Name != made[2] {
		t.Fatalf("order %+v %v", list, err)
	}
	pruned, err := jobs.PruneScheduledBackups(d.BackupsDir(), 1, "")
	if err != nil || len(pruned) != 2 || pruned[0] != made[0] || pruned[1] != made[1] {
		t.Fatalf("pruned %v %v", pruned, err)
	}
	if got := names(t, d.BackupsDir()); len(got) != 1 || got[0] != made[2] {
		t.Fatalf("left %v", got)
	}
}

type failingBackuper struct {
	jobs.Backuper
	garbage bool
}

func (f failingBackuper) BackupTo(_ context.Context, path string) error {
	if f.garbage {
		return os.WriteFile(path, []byte("not a database"), 0o600)
	}
	return errors.New("disk I/O error")
}

func TestScheduledBackup_AFailureLeavesNothingAndPrunesNothing(t *testing.T) {
	d := openDB(t)
	ctx := context.Background()
	for i := 0; i < 3; i++ {
		if _, err := jobs.RunScheduledBackup(ctx, d, t0.Add(time.Duration(i)*time.Hour), 3); err != nil {
			t.Fatal(err)
		}
	}
	before := names(t, d.BackupsDir())
	if _, err := jobs.RunScheduledBackup(ctx, failingBackuper{Backuper: d}, t0.Add(5*time.Hour), 1); err == nil {
		t.Fatal("no error")
	}
	if _, err := jobs.RunScheduledBackup(ctx, failingBackuper{Backuper: d, garbage: true}, t0.Add(6*time.Hour), 1); err == nil || !strings.Contains(err.Error(), "does not verify") {
		t.Fatalf("unverifiable backup: %v", err)
	}
	if got := names(t, d.BackupsDir()); strings.Join(got, ",") != strings.Join(before, ",") {
		t.Fatalf("after failures %v, want %v", got, before)
	}
	// A cancelled backup leaves no file under the final name and no temporary directory.
	cctx, cancel := context.WithCancel(ctx)
	cancel()
	if _, err := jobs.RunScheduledBackup(cctx, d, t0.Add(7*time.Hour), 3); err == nil {
		t.Fatal("a cancelled backup succeeded")
	}
	if got := names(t, d.BackupsDir()); strings.Join(got, ",") != strings.Join(before, ",") {
		t.Fatalf("after a cancelled backup %v", got)
	}
}

func TestNextBackup(t *testing.T) {
	day := 24 * time.Hour
	for _, c := range []struct {
		name string
		last time.Time
		ran  bool
		want time.Duration
	}{
		{"never", time.Time{}, false, 0},
		{"just now", t0, true, day},
		{"an hour ago", t0.Add(-time.Hour), true, 23 * time.Hour},
		{"overdue", t0.Add(-3 * day), true, 0},
		{"in the future (clock moved back)", t0.Add(5 * day), true, day},
	} {
		if got := jobs.NextBackup(t0, c.last, c.ran, day); got != c.want {
			t.Errorf("%s: %v, want %v", c.name, got, c.want)
		}
	}
}

// countingBackuper advances the fake clock by step after each backup, as the real time would
// between runs, and can fail its first calls.
type countingBackuper struct {
	*db.DB
	clk   *clock.Fake
	step  time.Duration
	mu    sync.Mutex
	calls int
	fail  int
	done  chan struct{}
}

func (c *countingBackuper) BackupTo(ctx context.Context, path string) error {
	c.mu.Lock()
	c.calls++
	n := c.calls
	c.mu.Unlock()
	defer func() {
		c.clk.Advance(c.step)
		select {
		case c.done <- struct{}{}:
		default:
		}
	}()
	if n <= c.fail {
		return errors.New("disk I/O error")
	}
	return c.DB.BackupTo(ctx, path)
}

func TestRunBackups_TicksPrunesAndRetries(t *testing.T) {
	d := openDB(t)
	clk := clock.NewFake(t0)
	b := &countingBackuper{DB: d, clk: clk, step: time.Hour, fail: 1, done: make(chan struct{}, 16)}
	logs := &syncBuffer{}
	logger := slog.New(slog.NewJSONHandler(logs, nil))
	ctx, cancel := context.WithCancel(context.Background())
	stopped := make(chan struct{})
	go func() { defer close(stopped); jobs.RunBackups(ctx, b, clk, 20*time.Millisecond, 2, logger) }()
	// Wait for whole runs (backup, verification and pruning), as the job logs them: one failure,
	// then four completed backups.
	deadline := time.Now().Add(30 * time.Second)
	for strings.Count(logs.String(), `"msg":"scheduled backup",`) < 4 {
		if time.Now().After(deadline) {
			t.Fatalf("logs %s", logs)
		}
		time.Sleep(5 * time.Millisecond)
	}
	cancel()
	<-stopped
	list, _ := jobs.ScheduledBackups(d.BackupsDir())
	if len(list) != 2 {
		t.Fatalf("kept %d, want 2", len(list))
	}
	l := logs.String()
	if !strings.Contains(l, `"msg":"scheduled backup failed"`) || !strings.Contains(l, `"msg":"scheduled backup"`) || !strings.Contains(l, `"pruned":1`) {
		t.Fatalf("logs %s", l)
	}
}

func TestRunBackups_ScheduleFollowsTheNewestBackupAcrossRestarts(t *testing.T) {
	d := openDB(t)
	clk := clock.NewFake(t0)
	logs := &syncBuffer{}
	logger := slog.New(slog.NewJSONHandler(logs, nil))
	// A scheduled backup from an hour ago: with a 24 h interval nothing is due yet.
	if _, err := jobs.RunScheduledBackup(context.Background(), d, t0.Add(-time.Hour), 7); err != nil {
		t.Fatal(err)
	}
	b := &countingBackuper{DB: d, clk: clk, done: make(chan struct{}, 16)}
	ctx, cancel := context.WithCancel(context.Background())
	stopped := make(chan struct{})
	go func() { defer close(stopped); jobs.RunBackups(ctx, b, clk, 24*time.Hour, 7, logger) }()
	time.Sleep(200 * time.Millisecond)
	cancel()
	<-stopped
	b.mu.Lock()
	calls := b.calls
	b.mu.Unlock()
	if calls != 0 {
		t.Fatalf("a restart repeated the backup: %d calls", calls)
	}
	// One from yesterday: overdue, so it runs at once.
	clk.Advance(24 * time.Hour)
	ctx, cancel = context.WithCancel(context.Background())
	stopped = make(chan struct{})
	go func() { defer close(stopped); jobs.RunBackups(ctx, b, clk, 24*time.Hour, 7, logger) }()
	// Wait until the job logged the backup (verified and pruned): cancelling earlier would
	// interrupt its verification, which deletes the file.
	deadline := time.Now().Add(10 * time.Second)
	for !strings.Contains(logs.String(), `"msg":"scheduled backup"`) {
		if time.Now().After(deadline) {
			t.Fatal("an overdue backup did not run at start")
		}
		time.Sleep(10 * time.Millisecond)
	}
	cancel()
	<-stopped
	if list, _ := jobs.ScheduledBackups(d.BackupsDir()); len(list) != 2 {
		t.Fatalf("%d backups", len(list))
	}
}

func TestScheduledBackup_TheFileJustWrittenIsNeverPruned(t *testing.T) {
	// Backups written while the clock was two and three days ahead: once it is right again, the
	// new backup's name sorts oldest, but it is the one to keep.
	d := openDB(t)
	ctx := context.Background()
	var ahead []string
	for _, days := range []int{2, 3} {
		res, err := jobs.RunScheduledBackup(ctx, d, t0.Add(time.Duration(days)*24*time.Hour), 2)
		if err != nil {
			t.Fatal(err)
		}
		ahead = append(ahead, filepath.Base(res.Path))
	}
	res, err := jobs.RunScheduledBackup(ctx, d, t0, 2)
	if err != nil {
		t.Fatal(err)
	}
	now := filepath.Base(res.Path)
	if len(res.Pruned) != 1 || res.Pruned[0] != ahead[0] {
		t.Fatalf("pruned %v, want [%s]", res.Pruned, ahead[0])
	}
	if got := names(t, d.BackupsDir()); strings.Join(got, ",") != now+","+ahead[1] {
		t.Fatalf("left %v", got)
	}
}

func TestRunBackups_TheScheduleDoesNotDriftByTheBackupsDuration(t *testing.T) {
	// Each backup "takes" a whole interval on the fake clock. The next one is due an interval
	// after the previous one started, so it runs at once; measuring from its end would wait an
	// hour.
	d := openDB(t)
	clk := clock.NewFake(t0)
	b := &countingBackuper{DB: d, clk: clk, step: time.Hour, done: make(chan struct{}, 16)}
	logs := &syncBuffer{}
	ctx, cancel := context.WithCancel(context.Background())
	stopped := make(chan struct{})
	go func() {
		defer close(stopped)
		jobs.RunBackups(ctx, b, clk, time.Hour, 10, slog.New(slog.NewJSONHandler(logs, nil)))
	}()
	deadline := time.Now().Add(30 * time.Second)
	for strings.Count(logs.String(), `"msg":"scheduled backup",`) < 3 {
		if time.Now().After(deadline) {
			t.Fatalf("the schedule drifted: logs %s", logs)
		}
		time.Sleep(5 * time.Millisecond)
	}
	cancel()
	<-stopped
}
