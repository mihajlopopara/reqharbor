package throttle

import (
	"strconv"
	"testing"
	"time"

	"github.com/mihajlopopara/reqharbor/server/internal/clock"
)

func TestLockFor(t *testing.T) {
	for failed, want := range map[int64]time.Duration{
		0: 0, 4: 0, 5: time.Minute, 6: 2 * time.Minute, 7: 4 * time.Minute, 10: 32 * time.Minute,
		11: 60 * time.Minute, 12: 60 * time.Minute, 1000: 60 * time.Minute,
	} {
		if got := LockFor(failed); got != want {
			t.Errorf("LockFor(%d) = %v, want %v", failed, got, want)
		}
	}
}

func TestKeysAreKeyedAndDistinct(t *testing.T) {
	k1, k2 := make([]byte, 32), make([]byte, 32)
	k2[0] = 1
	if EmailKey(k1, "a@x") == EmailKey(k2, "a@x") || EmailKey(k1, "a@x") == EmailKey(k1, "b@x") {
		t.Fatal("email keys")
	}
	if PairKey(k1, "a@x", "1.2.3.4") == PairKey(k1, "a@x", "1.2.3.5") || PairKey(k1, "a@x", "b") == EmailKey(k1, "a@x") {
		t.Fatal("pair keys")
	}
	// The separator keeps (email, bucket) unambiguous.
	if PairKey(k1, "a@x1", "2") == PairKey(k1, "a@x", "12") {
		t.Fatal("ambiguous pair key")
	}
}

func TestProofOfWork(t *testing.T) {
	c := clock.NewFake(time.Unix(1_700_000_000, 0))
	th := New(make([]byte, 32), c)
	ch := th.NewChallenge("ar1", "a@x", 8)
	proof := Solve(ch, 8)
	// Bound to the auth request and the email.
	if th.VerifyProof("ar2", "a@x", proof, 8) {
		t.Fatal("proof accepted for another auth request")
	}
	ch = th.NewChallenge("ar1", "a@x", 8)
	if th.VerifyProof("ar1", "b@x", Solve(ch, 8), 8) {
		t.Fatal("proof accepted for another email")
	}
	// Single use.
	ch = th.NewChallenge("ar1", "a@x", 8)
	proof = Solve(ch, 8)
	if !th.VerifyProof("ar1", "a@x", proof, 8) || th.VerifyProof("ar1", "a@x", proof, 8) {
		t.Fatal("proof not single-use")
	}
	// Expires after five minutes.
	ch = th.NewChallenge("ar1", "a@x", 8)
	proof = Solve(ch, 8)
	c.Advance(ChallengeTTL + time.Second)
	if th.VerifyProof("ar1", "a@x", proof, 8) {
		t.Fatal("expired challenge accepted")
	}
	// Too few bits, malformed proofs.
	ch = th.NewChallenge("ar1", "a@x", 20)
	for _, p := range []string{"", ch, ch + ".", ch + ".x", ch + ".-1", "nope.1", ch + ".0"} {
		if th.VerifyProof("ar1", "a@x", p, 20) {
			t.Fatalf("proof %q accepted", p)
		}
	}
	if LeadingZeroBits([]byte{}, "") < 0 {
		t.Fatal("bits")
	}
}

func TestAccountWindowDifficulty(t *testing.T) {
	c := clock.NewFake(time.Unix(1_700_000_000, 0))
	th := New(make([]byte, 32), c)
	ek := EmailKey(th.key, "a@x")
	for range 19 {
		th.failures[ek] = append(th.failures[ek], c.Now())
	}
	if th.recent(ek, c.Now()) != 19 {
		t.Fatal("count")
	}
	c.Advance(AccountWindow + time.Second)
	if th.recent(ek, c.Now()) != 0 {
		t.Fatal("window did not slide")
	}
}

func TestSweepDropsStaleCountersAndChallenges(t *testing.T) {
	c := clock.NewFake(time.Unix(1_700_000_000, 0))
	th := New(make([]byte, 32), c)
	th.Failed("old@x")
	old := th.NewChallenge("ar1", "old@x", 8)
	c.Advance(30 * time.Minute)
	th.Failed("new@x")
	fresh := th.NewChallenge("ar1", "new@x", 8)
	c.Advance(30*time.Minute + time.Second) // old@x's only failure and old's challenge are past
	th.Sweep()
	th.mu.Lock()
	_, oldKept := th.failures[EmailKey(th.key, "old@x")]
	_, newKept := th.failures[EmailKey(th.key, "new@x")]
	_, oldCh := th.challenges[old]
	_, freshCh := th.challenges[fresh]
	th.mu.Unlock()
	if oldKept || !newKept {
		t.Fatalf("failures after sweep: old kept %v, new kept %v", oldKept, newKept)
	}
	if oldCh || freshCh {
		t.Fatalf("challenges after sweep: old %v, fresh %v (both past five minutes)", oldCh, freshCh)
	}
	// A challenge still inside its five minutes survives a sweep.
	live := th.NewChallenge("ar1", "new@x", 8)
	c.Advance(time.Minute)
	th.Sweep()
	if !th.VerifyProof("ar1", "new@x", Solve(live, 8), 8) {
		t.Fatal("a live challenge was swept")
	}
	// Many random emails do not accumulate.
	for i := range 1000 {
		th.Failed("r" + strconv.Itoa(i) + "@x")
	}
	c.Advance(AccountWindow + time.Second)
	th.Sweep()
	th.mu.Lock()
	n := len(th.failures)
	th.mu.Unlock()
	if n != 0 {
		t.Fatalf("%d counters left after the window", n)
	}
}
