package app_test

// S4: the password throttle (06-go-server.md §5.9): per (email, IP bucket), persisted; the
// account-wide proof of work; identical behaviour for known and unknown emails; clearing and GC.

import (
	"context"
	"net/url"
	"regexp"
	"strconv"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/mihajlopopara/reqharbor/server/internal/auth/throttle"
	"github.com/mihajlopopara/reqharbor/server/internal/jobs"
	"github.com/mihajlopopara/reqharbor/server/internal/store"

	tu "github.com/mihajlopopara/reqharbor/server/internal/testutil"
)

const victim = "victim@example.test"

func ipFor(i int, known bool) string {
	n := i*2 + 1
	if !known {
		n++
	}
	return "192.0.2." + strconv.Itoa(n)
}

func throttleHarness(t *testing.T) *tu.Harness {
	t.Helper()
	h := tu.New(t)
	h.CreateAdmin(t, "admin@example.test")
	h.CreateUser(t, victim)
	return h
}

// fromIP returns a browser on a fixed client IP with a fresh auth request.
func fromIP(t *testing.T, h *tu.Harness, ip string) (*tu.Browser, string) {
	t.Helper()
	b := h.Browser()
	b.IP = ip
	return b, b.Authorize(t, tu.NewPKCE())
}

func TestThrottle_PairLockDoesNotLockOtherIPs(t *testing.T) {
	h := throttleHarness(t)
	b, ar := fromIP(t, h, "203.0.113.7")
	for i := range 5 {
		r := b.Login(t, ar, victim, "wrong password!")
		if r.StatusCode != 400 || !strings.Contains(string(r.Body), "Email or password is incorrect.") || strings.Contains(string(r.Body), "Try again in") {
			t.Fatalf("failure %d: %d %s", i+1, r.StatusCode, r.Body)
		}
	}
	// (email, A) is throttled now: even the right password gets the throttle message, and no
	// argon2 runs.
	before := h.App.Hasher.Verifies.Load()
	r := b.Login(t, ar, victim, tu.DefaultPassword)
	if r.StatusCode != 400 || !strings.Contains(string(r.Body), "Email or password is incorrect. Try again in 1 minute.") {
		t.Fatalf("throttled: %d %s", r.StatusCode, r.Body)
	}
	if h.App.Hasher.Verifies.Load() != before {
		t.Fatal("argon2 ran for a throttled pair")
	}
	// The owner on another network signs in at once.
	ob := h.Browser()
	ob.IP = "198.51.100.9"
	before = h.App.Hasher.Verifies.Load()
	p := tu.NewPKCE()
	oar := ob.Authorize(t, p)
	tu.LoopbackCode(t, ob.Login(t, oar, victim, tu.DefaultPassword), p)
	if h.App.Hasher.Verifies.Load() != before+1 {
		t.Fatal("expected one argon2 verification")
	}
	// The attacker's row is not lifted by the owner's success.
	if h.Count(t, "login_throttle", "failed_count = 5") != 1 {
		t.Fatal("the attacker's throttle row was cleared")
	}
	// The lock grows: after it lapses, another failure locks for 2 minutes.
	h.Clock.Advance(time.Minute + time.Second)
	b2, ar2 := fromIP(t, h, "203.0.113.7")
	b2.Login(t, ar2, victim, "wrong password!")
	if r := b2.Login(t, ar2, victim, tu.DefaultPassword); !strings.Contains(string(r.Body), "Try again in 2 minutes.") {
		t.Fatalf("second lock: %s", r.Body)
	}
	// A /64 is one bucket for IPv6.
	b6, ar6 := fromIP(t, h, "2001:db8:1:2::1")
	for range 5 {
		b6.Login(t, ar6, "v6@example.test", "wrong password!")
	}
	b6b, ar6b := fromIP(t, h, "2001:db8:1:2:ffff::9")
	if r := b6b.Login(t, ar6b, "v6@example.test", "wrong password!"); !strings.Contains(string(r.Body), "Try again in") {
		t.Fatal("another address in the same /64 is not throttled")
	}
}

var (
	arValueRe = regexp.MustCompile(`[A-Za-z0-9_-]{22}`)
	challRe   = regexp.MustCompile(`data-challenge="([A-Za-z0-9_-]{22})"`)
)

// normal makes two pages comparable: the auth request id, the challenge and the email differ.
func normal(body []byte, email string) string {
	s := strings.ReplaceAll(string(body), email, "EMAIL")
	s = challRe.ReplaceAllString(s, `data-challenge="C"`)
	return arValueRe.ReplaceAllString(s, "AR")
}

func TestThrottle_KnownAndUnknownEmailsAreIdentical(t *testing.T) {
	h := throttleHarness(t)
	const unknown = "nobody@example.test" // same length as victim@example.test
	if len(unknown) != len(victim) {
		t.Fatal("pick emails of equal length")
	}
	kb, kar := fromIP(t, h, "203.0.113.10")
	ub, uar := fromIP(t, h, "203.0.113.11")
	step := func(name string, kpw, upw string) {
		t.Helper()
		k0 := h.App.Hasher.Verifies.Load()
		kr := kb.Login(t, kar, victim, kpw)
		k1 := h.App.Hasher.Verifies.Load()
		ur := ub.Login(t, uar, unknown, upw)
		u1 := h.App.Hasher.Verifies.Load()
		if kr.StatusCode != ur.StatusCode || normal(kr.Body, victim) != normal(ur.Body, unknown) {
			t.Fatalf("%s: known %d / unknown %d differ:\n%s\n---\n%s", name, kr.StatusCode, ur.StatusCode, kr.Body, ur.Body)
		}
		if k1-k0 != u1-k1 {
			t.Fatalf("%s: argon2 ran %d times for the known email and %d for the unknown", name, k1-k0, u1-k1)
		}
	}
	for range 5 {
		step("failure", "wrong password!", "wrong password!")
	}
	step("throttled", tu.DefaultPassword, tu.DefaultPassword) // the right password for the known one
	// Account-wide: 20 failures across IPs require a proof of work, for both.
	for i := range 4 {
		for _, e := range []string{victim, unknown} {
			b, ar := fromIP(t, h, ipFor(i, e == victim))
			for range 4 {
				b.Login(t, ar, e, "wrong password!")
			}
		}
	}
	kb, kar = fromIP(t, h, "198.51.100.20")
	ub, uar = fromIP(t, h, "198.51.100.21")
	step("proof of work", "wrong password!", "wrong password!")
}

func TestThrottle_ProofOfWorkAfter20FailuresAcrossIPs(t *testing.T) {
	h := throttleHarness(t)
	for i := range 5 {
		b, ar := fromIP(t, h, "192.0.2."+string(rune('1'+i)))
		for range 4 {
			if r := b.Login(t, ar, victim, "wrong password!"); r.StatusCode != 400 || strings.Contains(string(r.Body), "data-challenge") {
				t.Fatalf("failure before the threshold: %d", r.StatusCode)
			}
		}
	}
	p := tu.NewPKCE()
	b := h.Browser()
	b.IP = "198.51.100.30"
	ar := b.Authorize(t, p)
	before := h.App.Hasher.Verifies.Load()
	r := b.Login(t, ar, victim, tu.DefaultPassword)
	m := challRe.FindSubmatch(r.Body)
	if r.StatusCode != 400 || m == nil || !strings.Contains(string(r.Body), `data-difficulty="18"`) ||
		!strings.Contains(string(r.Body), "Enable JavaScript to continue signing in.") || !strings.Contains(string(r.Body), "<script>") {
		t.Fatalf("proof of work page: %d %s", r.StatusCode, r.Body)
	}
	if h.App.Hasher.Verifies.Load() != before {
		t.Fatal("argon2 ran before the proof of work")
	}
	login := func(pow string) *tu.Resp {
		return b.Post(t, "/auth/password/login", url.Values{"ar": {ar}, "email": {victim}, "password": {tu.DefaultPassword}, "pow": {pow}})
	}
	// An invalid proof fails (and burns the challenge); so does a solution for another challenge.
	if r := login(string(m[1]) + ".1"); !challRe.Match(r.Body) {
		t.Fatalf("invalid proof accepted: %d", r.StatusCode)
	}
	r = login("")
	m = challRe.FindSubmatch(r.Body)
	proof := throttle.Solve(string(m[1]), 18)
	tu.LoopbackCode(t, login(proof), p)
	// A reused proof fails.
	b2 := h.Browser()
	b2.IP = "198.51.100.31"
	ar2 := b2.Authorize(t, tu.NewPKCE())
	if r := b2.Post(t, "/auth/password/login", url.Values{"ar": {ar2}, "email": {victim}, "password": {tu.DefaultPassword}, "pow": {proof}}); !challRe.Match(r.Body) {
		t.Fatalf("reused proof accepted: %d", r.StatusCode)
	}
	// After the hour the slow-down lapses.
	h.Clock.Advance(time.Hour + time.Second)
	h.SignIn(t, victim, tu.DefaultPassword)
}

func TestThrottle_RowsSurviveARestartAndAreClearedByUnlockAndGC(t *testing.T) {
	h := throttleHarness(t)
	b, ar := fromIP(t, h, "203.0.113.40")
	for range 5 {
		b.Login(t, ar, victim, "wrong password!")
	}
	h.Restart(t)
	b, ar = fromIP(t, h, "203.0.113.40")
	if r := b.Login(t, ar, victim, tu.DefaultPassword); !strings.Contains(string(r.Body), "Try again in") {
		t.Fatalf("throttle forgotten by a restart: %s", r.Body)
	}
	// `user unlock` (another process) deletes every row for the email, whatever the IP, and the
	// in-memory slow-down with them.
	for i := range 5 {
		ob, oar := fromIP(t, h, "192.0.2.1"+string(rune('0'+i)))
		for range 4 {
			ob.Login(t, oar, victim, "wrong password!")
		}
	}
	err := h.Store.Write(context.Background(), func(tx store.WriteTx) error {
		n, err := throttle.ClearEmail(tx, victim)
		if n != 6 {
			t.Errorf("unlock cleared %d rows, want 6", n)
		}
		return err
	})
	if err != nil {
		t.Fatal(err)
	}
	h.SignIn(t, victim, tu.DefaultPassword) // neither throttled nor slowed down
	b = h.Browser()
	b.IP = "203.0.113.40"
	p := tu.NewPKCE()
	tu.LoopbackCode(t, b.Login(t, b.Authorize(t, p), victim, tu.DefaultPassword), p)
}

func TestThrottle_GCRemovesRowsAfter24Hours(t *testing.T) {
	h := throttleHarness(t)
	b, ar := fromIP(t, h, "203.0.113.50")
	b.Login(t, ar, victim, "wrong password!")
	h.Clock.Advance(12 * time.Hour)
	b2, ar2 := fromIP(t, h, "203.0.113.51")
	b2.Login(t, ar2, victim, "wrong password!")
	h.Clock.Advance(12*time.Hour + time.Millisecond)
	stats, err := jobs.GC(context.Background(), h.Store, h.Clock)
	if err != nil {
		t.Fatal(err)
	}
	if stats.Throttle != 1 || h.Count(t, "login_throttle", "") != 1 {
		t.Fatalf("gc %+v, %d rows left", stats, h.Count(t, "login_throttle", ""))
	}
	// Expired auth requests go too; live sessions stay.
	if stats.AuthRequests == 0 || h.Count(t, "sessions", "revoked_at IS NULL") == 0 {
		t.Fatalf("gc %+v", stats)
	}
}

// Concurrent wrong passwords for one (email, IP bucket) cannot all slip past the pair check
// before any failure is recorded: the lock decision is atomic with counting the attempt, so at
// most PairFreeFailures argon2 verifications run before the lock, for known and unknown emails.
func TestThrottle_ConcurrentAttemptsCannotBypassThePairLock(t *testing.T) {
	for _, email := range []string{victim, "nobody@example.test"} {
		t.Run(email, func(t *testing.T) {
			h := throttleHarness(t)
			const ip, perAR, ars = "203.0.113.60", 5, 4 // 20 attempts, under the 30/min login limit
			type job struct {
				b  *tu.Browser
				ar string
			}
			var jobs []job
			for range ars {
				b, ar := fromIP(t, h, ip)
				for range perAR {
					jobs = append(jobs, job{b.Clone(), ar})
				}
			}
			before := h.App.Hasher.Verifies.Load()
			start := make(chan struct{})
			res := make([]*tu.Resp, len(jobs))
			var wg sync.WaitGroup
			for i, j := range jobs {
				wg.Add(1)
				go func() {
					defer wg.Done()
					<-start
					res[i] = j.b.Login(t, j.ar, email, "wrong password!")
				}()
			}
			close(start)
			wg.Wait()
			verifies := h.App.Hasher.Verifies.Load() - before
			wrong, throttled := 0, 0
			for i, r := range res {
				body := string(r.Body)
				switch {
				case r.StatusCode == 400 && strings.Contains(body, "Try again in"):
					throttled++
				case r.StatusCode == 400 && strings.Contains(body, "Email or password is incorrect."):
					wrong++
				default:
					t.Fatalf("attempt %d: %d %s", i, r.StatusCode, body)
				}
			}
			if verifies > throttle.PairFreeFailures || int(verifies) != wrong || wrong+throttled != len(jobs) {
				t.Fatalf("%d argon2 verifications (max %d), %d wrong-password and %d throttled answers of %d",
					verifies, throttle.PairFreeFailures, wrong, throttled, len(jobs))
			}
			// The persisted row counted every attempt that reached argon2, and the pair is locked.
			if n := h.Count(t, "login_throttle", "failed_count = ? AND locked_until > 0", wrong); n != 1 || h.Count(t, "login_throttle", "") != 1 {
				t.Fatalf("throttle rows: %d with failed_count=%d, %d in all", n, wrong, h.Count(t, "login_throttle", ""))
			}
			b, ar := fromIP(t, h, ip)
			if r := b.Login(t, ar, email, tu.DefaultPassword); !strings.Contains(string(r.Body), "Try again in") {
				t.Fatalf("the pair is not locked after the burst: %s", r.Body)
			}
		})
	}
}
