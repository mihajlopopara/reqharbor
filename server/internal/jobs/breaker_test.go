package jobs_test

import (
	"bytes"
	"context"
	"errors"
	"log/slog"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/mihajlopopara/reqharbor/server/internal/jobs"
	"github.com/mihajlopopara/reqharbor/server/internal/store"
)

// fakeSizes is the DB-size and free-disk source of the sampler.
type fakeSizes struct {
	mu            sync.Mutex
	db, free      int64
	dbErr, ffsErr error
}

func (f *fakeSizes) SizeBytes(context.Context) (int64, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.db, f.dbErr
}

func (f *fakeSizes) FreeDiskBytes() (int64, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.free, f.ffsErr
}

func (f *fakeSizes) set(db, free int64) {
	f.mu.Lock()
	f.db, f.free = db, free
	f.mu.Unlock()
}

// fakeStore answers only the two total-storage sums; any other query panics (the embedded nil
// interface), which proves the sampler reads nothing else.
type fakeStore struct {
	mu           sync.Mutex
	legacy, open int64
	err          error
	reads        int
}

type fakeTx struct {
	store.ReadTx
	s *fakeStore
}

func (t fakeTx) LegacyTotalBytes() (int64, error)     { return t.s.legacy, t.s.err }
func (t fakeTx) OpenSignupTotalBytes() (int64, error) { return t.s.open, t.s.err }

func (s *fakeStore) Read(_ context.Context, fn func(store.ReadTx) error) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.reads++
	return fn(fakeTx{s: s})
}

func (s *fakeStore) Write(context.Context, func(store.WriteTx) error) error {
	panic("the breaker never writes")
}

func TestBreaker_SampleTable(t *testing.T) {
	const MiB = 1 << 20
	cases := []struct {
		name                         string
		limits                       jobs.BreakerLimits
		db, free, legacy, open       int64
		wantOpen, wantLegacy, wantOS bool
		wantReads                    int
	}{
		{name: "all off", db: 10 * MiB, free: 1, legacy: 1 << 40, open: 1 << 40},
		{name: "db below max", limits: jobs.BreakerLimits{MaxDBBytes: 10 * MiB}, db: 10*MiB - 1},
		{name: "db at max", limits: jobs.BreakerLimits{MaxDBBytes: 10 * MiB}, db: 10 * MiB, wantOpen: true},
		{name: "free disk above min", limits: jobs.BreakerLimits{MinFreeDiskBytes: 512 * MiB}, free: 512 * MiB},
		{name: "free disk below min", limits: jobs.BreakerLimits{MinFreeDiskBytes: 512 * MiB}, free: 512*MiB - 1, wantOpen: true},
		{name: "legacy below cap", limits: jobs.BreakerLimits{LegacyTotalBytes: 1 << 30}, legacy: 1<<30 - 1, wantReads: 1},
		{name: "legacy at cap", limits: jobs.BreakerLimits{LegacyTotalBytes: 1 << 30}, legacy: 1 << 30, wantLegacy: true, wantReads: 1},
		{name: "open signup at cap", limits: jobs.BreakerLimits{OpenSignupTotalBytes: 5 << 30}, open: 5 << 30, wantOS: true, wantReads: 1},
		{
			name:   "every limit reached",
			limits: jobs.BreakerLimits{MaxDBBytes: 1, MinFreeDiskBytes: 2, LegacyTotalBytes: 3, OpenSignupTotalBytes: 4},
			db:     1, free: 1, legacy: 3, open: 4,
			wantOpen: true, wantLegacy: true, wantOS: true, wantReads: 2,
		},
		{
			name:   "the class caps do not open the global breaker",
			limits: jobs.BreakerLimits{MaxDBBytes: 100, LegacyTotalBytes: 3, OpenSignupTotalBytes: 4},
			db:     99, legacy: 3, open: 4,
			wantLegacy: true, wantOS: true, wantReads: 2,
		},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			sizes := &fakeSizes{db: c.db, free: c.free}
			st := &fakeStore{legacy: c.legacy, open: c.open}
			b := jobs.NewBreaker(c.limits, sizes, st, slog.New(slog.DiscardHandler))
			if err := b.Sample(context.Background()); err != nil {
				t.Fatal(err)
			}
			if b.Open() != c.wantOpen || b.LegacyFull() != c.wantLegacy || b.OpenSignupFull() != c.wantOS {
				t.Fatalf("open %v legacy %v openSignup %v", b.Open(), b.LegacyFull(), b.OpenSignupFull())
			}
			if st.reads != c.wantReads {
				t.Fatalf("%d store reads, want %d (a disabled cap reads nothing)", st.reads, c.wantReads)
			}
		})
	}
}

func TestBreaker_RecoversAndKeepsTheLastStateOnErrors(t *testing.T) {
	sizes := &fakeSizes{db: 200, free: 1 << 40}
	st := &fakeStore{legacy: 50}
	b := jobs.NewBreaker(jobs.BreakerLimits{MaxDBBytes: 100, LegacyTotalBytes: 40}, sizes, st, slog.New(slog.DiscardHandler))
	ctx := context.Background()
	if err := b.Sample(ctx); err != nil || !b.Open() || !b.LegacyFull() {
		t.Fatalf("not open: %v", err)
	}
	// A failing read keeps the last known state and reports the error.
	sizes.dbErr = errors.New("disk I/O")
	st.err = errors.New("busy")
	if err := b.Sample(ctx); err == nil || !b.Open() || !b.LegacyFull() {
		t.Fatalf("state after errors: open %v legacy %v err %v", b.Open(), b.LegacyFull(), err)
	}
	// Space freed: the next good sample closes it again.
	sizes.dbErr, st.err = nil, nil
	sizes.set(99, 1<<40)
	st.legacy = 39
	if err := b.Sample(ctx); err != nil || b.Open() || b.LegacyFull() {
		t.Fatalf("still open: %v", err)
	}
	// SetLimits applies from the next sample; turning a cap off closes it.
	sizes.set(1000, 1<<40)
	st.legacy = 1000
	b.SetLimits(jobs.BreakerLimits{})
	if b.Open() {
		t.Fatal("SetLimits sampled")
	}
	if err := b.Sample(ctx); err != nil || b.Open() || b.LegacyFull() {
		t.Fatal("disabled limits still trip")
	}
	// Without a size source only the class caps apply.
	nb := jobs.NewBreaker(jobs.BreakerLimits{MaxDBBytes: 1, MinFreeDiskBytes: 1}, nil, nil, slog.New(slog.DiscardHandler))
	if err := nb.Sample(ctx); err != nil || nb.Open() {
		t.Fatal("nil sizes")
	}
}

type syncBuffer struct {
	mu sync.Mutex
	b  bytes.Buffer
}

func (s *syncBuffer) Write(p []byte) (int, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.b.Write(p)
}

func (s *syncBuffer) String() string {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.b.String()
}

func TestBreaker_RunSamplesOnItsTickerAndLogsTransitionsOnce(t *testing.T) {
	sizes := &fakeSizes{db: 1, free: 1 << 40}
	logs := &syncBuffer{}
	b := jobs.NewBreaker(jobs.BreakerLimits{MaxDBBytes: 100}, sizes, &fakeStore{}, slog.New(slog.NewTextHandler(logs, nil)))
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan struct{})
	go func() { b.Run(ctx, 2*time.Millisecond); close(done) }()
	wait := func(what string, cond func() bool) {
		t.Helper()
		deadline := time.Now().Add(5 * time.Second)
		for !cond() {
			if time.Now().After(deadline) {
				t.Fatalf("timed out waiting for %s; logs:\n%s", what, logs.String())
			}
			time.Sleep(2 * time.Millisecond)
		}
	}
	sizes.set(100, 1<<40)
	wait("open", b.Open)
	time.Sleep(20 * time.Millisecond) // several more samples while it stays open
	sizes.set(50, 1<<40)
	wait("closed", func() bool { return !b.Open() })
	wait("closed log", func() bool { return strings.Contains(logs.String(), "storage breaker closed") })
	cancel()
	select {
	case <-done:
	case <-time.After(5 * time.Second):
		t.Fatal("Run did not stop")
	}
	if n := strings.Count(logs.String(), "storage breaker open"); n != 1 {
		t.Fatalf("open logged %d times:\n%s", n, logs.String())
	}
}
