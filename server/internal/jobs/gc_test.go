package jobs_test

import (
	"context"
	"io"
	"log/slog"
	"sort"
	"testing"
	"time"

	"github.com/mihajlopopara/reqharbor/server/internal/clock"
	"github.com/mihajlopopara/reqharbor/server/internal/jobs"
	"github.com/mihajlopopara/reqharbor/server/internal/store"
	tu "github.com/mihajlopopara/reqharbor/server/internal/testutil"
)

const (
	day  = 24 * time.Hour
	week = 7 * day
)

// gcRow is one row a GC pass must delete or keep; at is relative to the fake clock's now.
type gcRow struct {
	name string
	keep bool
	seed func(h *tu.Harness, t *testing.T, name string, now time.Time)
}

func ms(t time.Time) int64 { return clock.Millis(t) }

func authRequest(expires time.Duration) func(*tu.Harness, *testing.T, string, time.Time) {
	return func(h *tu.Harness, t *testing.T, name string, now time.Time) {
		h.MustExec(t, `INSERT INTO auth_requests (id, browser_hash, client_id, redirect_uri, code_challenge, client_state, created_at, expires_at)
		  VALUES (?, 'b', 'reqharbor-desktop', 'http://127.0.0.1:1/cb', 'c', 's', ?, ?)`, name, ms(now.Add(-time.Hour)), ms(now.Add(expires)))
	}
}

func token(expires time.Duration, revoked *time.Duration) func(*tu.Harness, *testing.T, string, time.Time) {
	return func(h *tu.Harness, t *testing.T, name string, now time.Time) {
		var rev any
		if revoked != nil {
			rev = ms(now.Add(*revoked))
		}
		h.MustExec(t, `INSERT INTO one_time_tokens (token_hash, id, purpose, created_at, expires_at, revoked_at)
		  VALUES (?, ?, 'signup_invite', ?, ?, ?)`, "hash-"+name, name, ms(now.Add(-30*day)), ms(now.Add(expires)), rev)
	}
}

func session(expires, idle time.Duration, revoked *time.Duration) func(*tu.Harness, *testing.T, string, time.Time) {
	return func(h *tu.Harness, t *testing.T, name string, now time.Time) {
		var rev any
		if revoked != nil {
			rev = ms(now.Add(*revoked))
		}
		h.MustExec(t, `INSERT INTO sessions (id, user_id, client_id, access_hash, access_expires_at, refresh_hash, created_at, last_used_day,
		    idle_expires_at, expires_at, revoked_at)
		  VALUES (?, 'U1', 'reqharbor-desktop', ?, ?, ?, ?, 0, ?, ?, ?)`,
			name, "a-"+name, ms(now.Add(-time.Hour)), "r-"+name, ms(now.Add(-90*day)), ms(now.Add(idle)), ms(now.Add(expires)), rev)
	}
}

func throttleRow(lastFailed time.Duration) func(*tu.Harness, *testing.T, string, time.Time) {
	return func(h *tu.Harness, t *testing.T, name string, now time.Time) {
		h.MustExec(t, `INSERT INTO login_throttle (key_hash, email_key, failed_count, last_failed_at) VALUES (?, 'e', 3, ?)`, name, ms(now.Add(lastFailed)))
	}
}

func ago(d time.Duration) *time.Duration { d = -d; return &d }

func TestGC_RemovesExpiredRowsAndKeepsLiveOnes(t *testing.T) {
	h := tu.New(t)
	now := h.Clock.Now()
	h.MustExec(t, `INSERT INTO users (id, email, signup_via, created_at, updated_at) VALUES ('U1', 'u1@example.test', 'cli', 1, 1)`)
	// The harness's own setup code is a live one_time_token; GC must keep it too.
	setupTokens := h.Count(t, "one_time_tokens", "")

	rows := []struct {
		table string
		gcRow
	}{
		{"auth_requests", gcRow{"ar-expired", false, authRequest(-time.Second)}},
		{"auth_requests", gcRow{"ar-expires-now", false, authRequest(0)}},
		{"auth_requests", gcRow{"ar-live", true, authRequest(time.Minute)}},

		{"one_time_tokens", gcRow{"tok-expired-8d", false, token(-8*day, nil)}},
		{"one_time_tokens", gcRow{"tok-expired-6d", true, token(-6*day, nil)}},
		{"one_time_tokens", gcRow{"tok-revoked-8d", false, token(week, ago(8*day))}},
		{"one_time_tokens", gcRow{"tok-revoked-1h", true, token(week, ago(time.Hour))}},
		{"one_time_tokens", gcRow{"tok-live", true, token(week, nil)}},

		{"sessions", gcRow{"ses-expired-8d", false, session(-8*day, day, nil)}},
		{"sessions", gcRow{"ses-idle-8d", false, session(30*day, -8*day, nil)}},
		{"sessions", gcRow{"ses-revoked-8d", false, session(30*day, day, ago(8*day))}},
		{"sessions", gcRow{"ses-revoked-6d", true, session(30*day, day, ago(6*day))}},
		{"sessions", gcRow{"ses-expired-1h", true, session(-time.Hour, day, nil)}},
		{"sessions", gcRow{"ses-live", true, session(30*day, 14*day, nil)}},

		{"login_throttle", gcRow{"thr-25h", false, throttleRow(-25 * time.Hour)}},
		{"login_throttle", gcRow{"thr-24h", false, throttleRow(-24 * time.Hour)}},
		{"login_throttle", gcRow{"thr-23h", true, throttleRow(-23 * time.Hour)}},
	}
	for _, r := range rows {
		r.seed(h, t, r.name, now)
	}
	stats, err := jobs.GC(context.Background(), h.Store, h.Clock)
	if err != nil {
		t.Fatal(err)
	}
	keyCol := map[string]string{"auth_requests": "id", "one_time_tokens": "id", "sessions": "id", "login_throttle": "key_hash"}
	want := store.GCStats{}
	for _, r := range rows {
		n := h.Count(t, r.table, keyCol[r.table]+" = ?", r.name)
		if r.keep && n != 1 {
			t.Errorf("%s: deleted, want kept", r.name)
		}
		if !r.keep {
			if n != 0 {
				t.Errorf("%s: kept, want deleted", r.name)
			}
			switch r.table {
			case "auth_requests":
				want.AuthRequests++
			case "one_time_tokens":
				want.Tokens++
			case "sessions":
				want.Sessions++
			case "login_throttle":
				want.Throttle++
			}
		}
	}
	if stats != want {
		t.Errorf("stats %+v, want %+v", stats, want)
	}
	if n := h.Count(t, "one_time_tokens", "purpose = 'setup'"); n != setupTokens {
		t.Errorf("setup code deleted (%d left)", n)
	}

	// A second pass deletes nothing; a week later the rows that were only recently dead go too.
	if again, err := jobs.GC(context.Background(), h.Store, h.Clock); err != nil || again != (store.GCStats{}) {
		t.Fatalf("second pass %+v %v", again, err)
	}
	h.Clock.Advance(week + time.Second)
	if _, err := jobs.GC(context.Background(), h.Store, h.Clock); err != nil {
		t.Fatal(err)
	}
	var left []string
	for _, r := range rows {
		if h.Count(t, r.table, keyCol[r.table]+" = ?", r.name) == 1 {
			left = append(left, r.name)
		}
	}
	sort.Strings(left)
	// tok-live expires a week after now (+1 s later it is expired but not yet 7 days old).
	wantLeft := []string{"ses-live", "tok-live"}
	if len(left) != len(wantLeft) || left[0] != wantLeft[0] || left[1] != wantLeft[1] {
		t.Errorf("after a week: %v, want %v", left, wantLeft)
	}
	if h.Count(t, "users", "id = 'U1'") != 1 {
		t.Error("GC deleted a user")
	}
}

func TestRunGC_RunsOnItsTickerUntilCancelled(t *testing.T) {
	h := tu.New(t)
	throttleRow(-48*time.Hour)(h, t, "old", h.Clock.Now())
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan struct{})
	go func() {
		jobs.RunGC(ctx, h.Store, h.Clock, 5*time.Millisecond, slog.New(slog.NewTextHandler(io.Discard, nil)))
		close(done)
	}()
	deadline := time.Now().Add(5 * time.Second)
	for h.Count(t, "login_throttle", "") != 0 {
		if time.Now().After(deadline) {
			t.Fatal("RunGC never ran")
		}
		time.Sleep(5 * time.Millisecond)
	}
	cancel()
	select {
	case <-done:
	case <-time.After(5 * time.Second):
		t.Fatal("RunGC did not stop")
	}
}
