// Package throttle is the password brute-force control (06-go-server.md §5.9):
//
//   - per (email, IP bucket), persisted in login_throttle under a keyed hash: after 5 failures
//     each further failure locks that pair for min(2^(failed-5), 60) minutes, without running
//     argon2, for known and unknown emails alike; other IP buckets are unaffected;
//   - per email across all IPs, in memory: after 20 failures in the last hour the login form for
//     that email needs a proof of work. It slows guessing down and never refuses the right password.
//
// Every key is derived from the normalised email, never from the user id, so an unknown email
// meets exactly the same thresholds, messages and timing as a known one.
package throttle

import (
	"crypto/hmac"
	"crypto/rand"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"errors"
	"math/bits"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/mihajlopopara/reqharbor/server/internal/clock"
	"github.com/mihajlopopara/reqharbor/server/internal/store"
)

// StateKey is the server_state key of the 32-byte HMAC key (base64).
const StateKey = "throttle_key"

// Tunables of §5.9.
const (
	PairFreeFailures   = 5                // failures before a pair locks
	MaxLock            = 60 * time.Minute // longest pair lock
	AccountWindow      = time.Hour        // the account-wide failure window
	AccountPoWFailures = 20               // failures in the window before a proof of work is needed
	PoWBaseDifficulty  = 18               // leading zero bits at the threshold
	PoWMaxDifficulty   = 22
	PoWStep            = 20 // failures per extra bit
	ChallengeTTL       = 5 * time.Minute
	RowTTL             = 24 * time.Hour // GC deletes rows this long after their last failure
	maxRemembered      = 256            // failure timestamps kept per email
	maxChallenges      = 100_000
)

// LoadKey reads the HMAC key from server_state; ErrNotFound when it was never created.
func LoadKey(tx store.ReadTx) ([]byte, error) {
	v, err := tx.ServerState(StateKey)
	if err != nil {
		return nil, err
	}
	k, err := base64.StdEncoding.DecodeString(v)
	if err != nil || len(k) != 32 {
		return nil, errors.New("throttle: server_state throttle_key is not 32 bytes of base64")
	}
	return k, nil
}

// EnsureKey returns the HMAC key, creating it on first use.
func EnsureKey(tx store.WriteTx) ([]byte, error) {
	k, err := LoadKey(tx)
	if !errors.Is(err, store.ErrNotFound) {
		return k, err
	}
	k = make([]byte, 32)
	if _, err := rand.Read(k); err != nil {
		return nil, err
	}
	return k, tx.SetServerState(StateKey, base64.StdEncoding.EncodeToString(k))
}

func mac(key []byte, parts ...string) string {
	h := hmac.New(sha256.New, key)
	h.Write([]byte(strings.Join(parts, "\x00")))
	return hex.EncodeToString(h.Sum(nil))
}

// EmailKey is HMAC(key, email): it finds every row of one email without knowing any IP.
func EmailKey(key []byte, email string) string { return mac(key, "email", email) }

// PairKey is HMAC(key, email || 0 || ip bucket).
func PairKey(key []byte, email, bucket string) string { return mac(key, "pair", email, bucket) }

// ClearEmail deletes every throttle row of an email (password reset, set-password, user unlock).
func ClearEmail(tx store.WriteTx, email string) (int64, error) {
	key, err := LoadKey(tx)
	if errors.Is(err, store.ErrNotFound) {
		return 0, nil
	}
	if err != nil {
		return 0, err
	}
	return tx.DeleteThrottleByEmail(EmailKey(key, email))
}

// Throttle holds the key and the in-memory account-wide state.
type Throttle struct {
	key   []byte
	clock clock.Clock

	mu         sync.Mutex
	failures   map[string][]time.Time // email key -> failure times within the window
	challenges map[string]challenge
}

type challenge struct {
	bind       string // auth request and email key
	difficulty int
	expires    time.Time
}

// New returns a throttle with the server's key. The key may be set later with SetKey, before the
// first request (the app reads it from server_state after the migrations).
func New(key []byte, c clock.Clock) *Throttle {
	return &Throttle{key: key, clock: c, failures: map[string][]time.Time{}, challenges: map[string]challenge{}}
}

// SetKey installs the HMAC key. It must run before the routes open.
func (t *Throttle) SetKey(key []byte) { t.key = key }

// Decision is what Check found.
type Decision struct {
	// Locked: this (email, IP bucket) is throttled; RetryIn is how long remains. No argon2 runs.
	Locked  bool
	RetryIn time.Duration
	// Difficulty > 0: the account-wide slow-down applies and the form needs a proof of work.
	Difficulty int
}

// RetryMinutes is RetryIn rounded up to whole minutes (at least 1).
func (d Decision) RetryMinutes() int {
	m := int((d.RetryIn + time.Minute - 1) / time.Minute)
	return max(m, 1)
}

// Check looks at the pair row and the account-wide counter.
func (t *Throttle) Check(tx store.ReadTx, email, bucket string) (Decision, error) {
	now := t.clock.Now()
	var d Decision
	row, err := tx.Throttle(PairKey(t.key, email, bucket))
	switch {
	case err == nil:
		if until := time.UnixMilli(row.LockedUntil); until.After(now) {
			d.Locked, d.RetryIn = true, until.Sub(now)
			return d, nil
		}
	case !errors.Is(err, store.ErrNotFound):
		return d, err
	}
	ek := EmailKey(t.key, email)
	n := t.recent(ek, now)
	if n < AccountPoWFailures {
		return d, nil
	}
	// The counter only applies while the email still has throttle rows: `user unlock` and a
	// password reset delete them (possibly from another process), and that clears the slow-down.
	exists, err := tx.ThrottleExists(ek)
	if err != nil {
		return d, err
	}
	if !exists {
		t.mu.Lock()
		delete(t.failures, ek)
		t.mu.Unlock()
		return d, nil
	}
	d.Difficulty = min(PoWBaseDifficulty+(n-AccountPoWFailures)/PoWStep, PoWMaxDifficulty)
	return d, nil
}

func (t *Throttle) recent(ek string, now time.Time) int {
	t.mu.Lock()
	defer t.mu.Unlock()
	list := t.failures[ek]
	cut := 0
	for cut < len(list) && !list[cut].After(now.Add(-AccountWindow)) {
		cut++
	}
	if cut > 0 {
		list = list[cut:]
		if len(list) == 0 {
			delete(t.failures, ek)
		} else {
			t.failures[ek] = list
		}
	}
	return len(list)
}

// LockFor is the pair lock after failed consecutive failures: min(2^(failed-5), 60) minutes from
// the fifth failure on.
func LockFor(failed int64) time.Duration {
	if failed < PairFreeFailures {
		return 0
	}
	exp := failed - PairFreeFailures
	if exp >= 6 {
		return MaxLock
	}
	return min(time.Duration(1<<exp)*time.Minute, MaxLock)
}

// Begin is the pair decision for one attempt that passed Check and the proof of work, taken
// atomically with recording it: in the caller's write transaction (SQLite has a single writer) it
// re-reads the pair row and either answers Locked, or counts the attempt as a failure up front
// (failed_count+1, last_failed_at, and the LockFor lock from the fifth on) before argon2 runs. So
// concurrent attempts for one pair queue on the writer, and at most PairFreeFailures of them can
// reach argon2 before the lock; a correct password then deletes the row (Succeed). Known and
// unknown emails take the same path.
func (t *Throttle) Begin(tx store.WriteTx, email, bucket string) (Decision, error) {
	now := t.clock.Now()
	pk := PairKey(t.key, email, bucket)
	row, err := tx.Throttle(pk)
	if errors.Is(err, store.ErrNotFound) {
		row, err = &store.Throttle{KeyHash: pk, EmailKey: EmailKey(t.key, email)}, nil
	}
	if err != nil {
		return Decision{}, err
	}
	if until := time.UnixMilli(row.LockedUntil); until.After(now) {
		return Decision{Locked: true, RetryIn: until.Sub(now)}, nil
	}
	row.FailedCount++
	row.LastFailedAt = clock.Millis(now)
	if lock := LockFor(row.FailedCount); lock > 0 {
		row.LockedUntil = clock.Millis(now.Add(lock))
	}
	return Decision{}, tx.PutThrottle(*row)
}

// Failed records a wrong password in the in-memory account-wide counter (the pair row was
// already counted by Begin).
func (t *Throttle) Failed(email string) {
	now := t.clock.Now()
	ek := EmailKey(t.key, email)
	t.mu.Lock()
	list := append(t.failures[ek], now)
	if len(list) > maxRemembered {
		list = list[len(list)-maxRemembered:]
	}
	t.failures[ek] = list
	t.mu.Unlock()
}

// Sweep drops account-wide counters whose newest failure left the window and expired
// challenges, so random emails cannot grow memory without bound (jobs.RunSweepers).
func (t *Throttle) Sweep() {
	now := t.clock.Now()
	cut := now.Add(-AccountWindow)
	t.mu.Lock()
	defer t.mu.Unlock()
	for k, list := range t.failures {
		if len(list) == 0 || !list[len(list)-1].After(cut) {
			delete(t.failures, k)
		}
	}
	for k, v := range t.challenges {
		if !v.expires.After(now) {
			delete(t.challenges, k)
		}
	}
}

// Succeed deletes this pair's row after a correct password. Rows of other IP buckets stay: an
// attacker's lock is not lifted by the owner's success, and the owner was never blocked by it.
func (t *Throttle) Succeed(tx store.WriteTx, email, bucket string) error {
	return tx.DeleteThrottle(PairKey(t.key, email, bucket))
}

// Clear deletes every row of the email and its in-memory counter.
func (t *Throttle) Clear(tx store.WriteTx, email string) error {
	if _, err := tx.DeleteThrottleByEmail(EmailKey(t.key, email)); err != nil {
		return err
	}
	t.Forget(email)
	return nil
}

// Forget drops the in-memory counter of an email.
func (t *Throttle) Forget(email string) {
	t.mu.Lock()
	delete(t.failures, EmailKey(t.key, email))
	t.mu.Unlock()
}

// NewChallenge issues a single-use proof-of-work challenge bound to the auth request and the
// email, valid for five minutes. It returns the challenge as base64url.
func (t *Throttle) NewChallenge(authRequestID, email string, difficulty int) string {
	var b [16]byte
	if _, err := rand.Read(b[:]); err != nil {
		panic("throttle: crypto/rand failed: " + err.Error())
	}
	c := base64.RawURLEncoding.EncodeToString(b[:])
	now := t.clock.Now()
	t.mu.Lock()
	defer t.mu.Unlock()
	if len(t.challenges) >= maxChallenges {
		for k, v := range t.challenges {
			if !v.expires.After(now) {
				delete(t.challenges, k)
			}
		}
	}
	t.challenges[c] = challenge{bind: authRequestID + "\x00" + EmailKey(t.key, email), difficulty: difficulty, expires: now.Add(ChallengeTTL)}
	return c
}

// VerifyProof checks `challenge.nonce` for this auth request and email at least at difficulty
// bits, and consumes the challenge whatever the outcome.
func (t *Throttle) VerifyProof(authRequestID, email, proof string, difficulty int) bool {
	c, nonce, ok := strings.Cut(proof, ".")
	if !ok || nonce == "" || len(nonce) > 20 {
		return false
	}
	if _, err := strconv.ParseUint(nonce, 10, 64); err != nil {
		return false
	}
	t.mu.Lock()
	ch, found := t.challenges[c]
	delete(t.challenges, c)
	t.mu.Unlock()
	if !found || !ch.expires.After(t.clock.Now()) || ch.bind != authRequestID+"\x00"+EmailKey(t.key, email) {
		return false
	}
	raw, err := base64.RawURLEncoding.DecodeString(c)
	if err != nil {
		return false
	}
	return LeadingZeroBits(raw, nonce) >= max(difficulty, ch.difficulty)
}

// LeadingZeroBits returns the number of leading zero bits of SHA-256(challenge || nonce), where
// nonce is the decimal ASCII text pow.js sends.
func LeadingZeroBits(challenge []byte, nonce string) int {
	sum := sha256.Sum256(append(append([]byte{}, challenge...), nonce...))
	n := 0
	for _, b := range sum {
		if b == 0 {
			n += 8
			continue
		}
		n += bits.LeadingZeros8(b)
		break
	}
	return n
}

// Solve finds a nonce for a challenge (tests and tools; the browser runs pow.js).
func Solve(challengeB64 string, difficulty int) string {
	raw, err := base64.RawURLEncoding.DecodeString(challengeB64)
	if err != nil {
		return ""
	}
	for i := uint64(0); ; i++ {
		n := strconv.FormatUint(i, 10)
		if LeadingZeroBits(raw, n) >= difficulty {
			return challengeB64 + "." + n
		}
	}
}
