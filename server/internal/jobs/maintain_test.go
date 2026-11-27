package jobs_test

import (
	"context"
	"errors"
	"log/slog"
	"sync"
	"testing"
	"time"

	"github.com/mihajlopopara/reqharbor/server/internal/clock"
	"github.com/mihajlopopara/reqharbor/server/internal/jobs"
)

type fakeMaintainer struct {
	mu   sync.Mutex
	n    int
	last time.Time
	ran  bool
	fail bool
}

func (m *fakeMaintainer) Maintain(context.Context) error {
	m.mu.Lock()
	defer m.mu.Unlock()
	m.n++
	if m.fail {
		return errors.New("disk I/O error")
	}
	return nil
}

func (m *fakeMaintainer) LastMaintenance(context.Context) (time.Time, bool, error) {
	m.mu.Lock()
	defer m.mu.Unlock()
	return m.last, m.ran, nil
}

func (m *fakeMaintainer) SetLastMaintenance(_ context.Context, at time.Time) error {
	m.mu.Lock()
	defer m.mu.Unlock()
	m.last, m.ran = at, true
	return nil
}

func (m *fakeMaintainer) runs() (int, time.Time, bool) {
	m.mu.Lock()
	defer m.mu.Unlock()
	return m.n, m.last, m.ran
}

func TestNextMaintenance(t *testing.T) {
	now := time.Date(2026, 11, 27, 12, 0, 0, 0, time.UTC)
	day, hour := jobs.MaintenanceInterval, jobs.MaintenanceFirstDelay
	for _, c := range []struct {
		name string
		last time.Time
		ran  bool
		want time.Duration
	}{
		{"never ran", time.Time{}, false, hour},
		{"ran 2 h ago", now.Add(-2 * time.Hour), true, 22 * time.Hour},
		{"due exactly now", now.Add(-day), true, 0},
		{"overdue after restarts", now.Add(-3 * day), true, 0},
		{"last run in the future", now.Add(5 * day), true, day},
	} {
		if got := jobs.NextMaintenance(now, c.last, c.ran, day, hour); got != c.want {
			t.Errorf("%s: %v, want %v", c.name, got, c.want)
		}
	}
	if jobs.MaintenanceInterval != 24*time.Hour || jobs.GCInterval != 10*time.Minute || jobs.SampleInterval != 10*time.Second {
		t.Fatal("job intervals differ from 06-go-server.md §6")
	}
}

func waitFor(t *testing.T, what string, cond func() bool) {
	t.Helper()
	deadline := time.Now().Add(5 * time.Second)
	for !cond() {
		if time.Now().After(deadline) {
			t.Fatal(what)
		}
		time.Sleep(2 * time.Millisecond)
	}
}

func run(m jobs.Maintainer, interval, first time.Duration) (stop func()) {
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan struct{})
	go func() {
		jobs.RunMaintenance(ctx, m, clock.System{}, interval, first, slog.New(slog.DiscardHandler))
		close(done)
	}()
	return func() { cancel(); <-done }
}

// A server restarted more often than the interval still runs the maintenance: an overdue run
// (recorded before the restart) happens at once, and each completed run is recorded.
func TestRunMaintenance_OverdueRunsAtOnceAndIsRecorded(t *testing.T) {
	m := &fakeMaintainer{last: time.Now().Add(-48 * time.Hour), ran: true}
	start := time.Now()
	stop := run(m, time.Hour, time.Hour)
	defer stop()
	waitFor(t, "overdue maintenance never ran", func() bool { n, _, _ := m.runs(); return n == 1 })
	waitFor(t, "run not recorded", func() bool { _, last, _ := m.runs(); return !last.Before(start) })
}

// A recent run (before a restart) is not repeated: nothing runs until the interval has passed.
func TestRunMaintenance_RecentRunIsNotRepeatedAfterARestart(t *testing.T) {
	m := &fakeMaintainer{last: time.Now(), ran: true}
	stop := run(m, time.Hour, time.Millisecond)
	time.Sleep(100 * time.Millisecond)
	stop()
	if n, _, _ := m.runs(); n != 0 {
		t.Fatalf("ran %d times right after a restart", n)
	}
}

// Never ran: the first run comes firstDelay after start, then every interval; a failure is
// retried after firstDelay and not recorded.
func TestRunMaintenance_FirstRunAfterTheDelayThenOnItsInterval(t *testing.T) {
	m := &fakeMaintainer{}
	stop := run(m, 2*time.Millisecond, 2*time.Millisecond)
	waitFor(t, "maintenance never repeated", func() bool { n, _, _ := m.runs(); return n >= 3 })
	stop()
	if _, _, ran := m.runs(); !ran {
		t.Fatal("runs not recorded")
	}

	f := &fakeMaintainer{fail: true}
	stop = run(f, time.Hour, 2*time.Millisecond)
	waitFor(t, "failed maintenance not retried", func() bool { n, _, _ := f.runs(); return n >= 2 })
	stop()
	if _, _, ran := f.runs(); ran {
		t.Fatal("a failed run was recorded")
	}
}
