package providers

// The issuer's JSON Web Key Set (RFC 7517), fetched on demand and cached (S10). Keys are picked
// by kid and by the token's algorithm; an unknown kid refetches the set (key rotation), at most
// once a minute, so a stream of tokens naming made-up kids cannot hammer the IdP. When a refresh
// fails the keys already known keep working; with none known, a failed fetch is retried after
// 10 seconds at the earliest.

import (
	"context"
	"crypto"
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rsa"
	"encoding/json"
	"errors"
	"io"
	"math/big"
	"net/http"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/mihajlopopara/reqharbor/server/internal/clock"
)

// JWKS cache bounds.
const (
	JWKSDefaultTTL   = time.Hour       // when the response has no usable Cache-Control max-age
	JWKSMinTTL       = 5 * time.Minute // a shorter max-age is raised to this
	JWKSMaxTTL       = 24 * time.Hour  // a longer max-age is lowered to this
	JWKSRefetchAfter = time.Minute     // an unknown kid refetches at most this often
	// JWKSFailureBackoff: with no keys cached, a failed fetch is not retried for this long, so an
	// unreachable JWKS does not turn every sign-in into an outbound request.
	JWKSFailureBackoff = 10 * time.Second
	maxJWKSBytes       = 1 << 20   // response cap
	maxJWKSKeys        = 100       // keys kept from one set
	minRSABits         = 2048      // smaller RSA keys are ignored
	maxRSAExponent     = 1<<31 - 1 // larger public exponents are ignored
)

// jwk is one usable verification key.
type jwk struct {
	kid string
	alg string // the key's own "alg", or "" for any algorithm of its type
	key crypto.PublicKey
}

// JWKS is a cached key set. The network fetch runs outside the lock, and concurrent callers that
// need a fetch share one (a hand-rolled singleflight), so a slow IdP never blocks a sign-in whose
// key is already cached.
type JWKS struct {
	url    string
	client *http.Client
	clock  clock.Clock

	mu        sync.Mutex
	keys      []jwk     // replaced as a whole, never modified in place
	fetchedAt time.Time // last successful fetch
	triedAt   time.Time // last attempt
	failedAt  time.Time // last failed attempt (zero after a success)
	ttl       time.Duration
	fetches   int
	inflight  *jwksCall
}

// jwksCall is one fetch in flight; done is closed when err is set.
type jwksCall struct {
	done chan struct{}
	err  error
}

// NewJWKS returns an empty cache for the set at url.
func NewJWKS(url string, client *http.Client, clk clock.Clock) *JWKS {
	if clk == nil {
		clk = clock.System{}
	}
	return &JWKS{url: url, client: client, clock: clk}
}

// Fetches is how many times the set was requested (tests).
func (j *JWKS) Fetches() int {
	j.mu.Lock()
	defer j.mu.Unlock()
	return j.fetches
}

// since reports whether at least d has passed from t to now (a clock that moved back counts as
// passed, so a wrong clock cannot freeze the cache).
func since(now, t time.Time, d time.Duration) bool {
	return now.Sub(t) >= d || now.Before(t)
}

// Keys returns the keys that may verify a token with this kid ("" matches every key) and alg.
// It fetches the set when none is cached (but not within JWKSFailureBackoff of a failed fetch)
// or the cached one is older than its TTL (at most once per JWKSRefetchAfter), and refetches
// once when no key matches and the last attempt is at least JWKSRefetchAfter old (rotation).
// While a refresh fails, the cached keys keep working.
func (j *JWKS) Keys(ctx context.Context, kid, alg string) ([]crypto.PublicKey, error) {
	j.mu.Lock()
	now := j.clock.Now()
	var need bool
	switch {
	case j.keys == nil:
		need = j.failedAt.IsZero() || since(now, j.failedAt, JWKSFailureBackoff) || j.inflight != nil
	case since(now, j.fetchedAt, j.ttl):
		need = since(now, j.triedAt, JWKSRefetchAfter) || j.inflight != nil
	}
	j.mu.Unlock()

	var fetchErr error
	if need {
		fetchErr = j.refresh(ctx)
	}
	j.mu.Lock()
	keys := j.keys
	j.mu.Unlock()
	if keys == nil {
		if fetchErr == nil {
			fetchErr = &Error{Code: "jwks_unavailable"}
		}
		return nil, fetchErr
	}
	if out := match(keys, kid, alg); len(out) > 0 {
		return out, nil
	}

	j.mu.Lock()
	again := since(j.clock.Now(), j.triedAt, JWKSRefetchAfter) || j.inflight != nil
	j.mu.Unlock()
	if again {
		if err := j.refresh(ctx); err != nil {
			return nil, err
		}
		j.mu.Lock()
		keys = j.keys
		j.mu.Unlock()
		if out := match(keys, kid, alg); len(out) > 0 {
			return out, nil
		}
	}
	return nil, &Error{Code: "jwks_unknown_kid"}
}

func match(keys []jwk, kid, alg string) []crypto.PublicKey {
	var out []crypto.PublicKey
	for _, k := range keys {
		if kid != "" && k.kid != kid {
			continue
		}
		if k.alg != "" && k.alg != alg {
			continue
		}
		switch k.key.(type) {
		case *rsa.PublicKey:
			if alg != AlgRS256 {
				continue
			}
		case *ecdsa.PublicKey:
			if alg != AlgES256 {
				continue
			}
		}
		out = append(out, k.key)
	}
	return out
}

// refresh fetches the set, or waits for the fetch already in flight. On failure the cached keys
// stay as they were. The fetch itself is detached from the caller's cancellation (but bounded by
// HTTPTimeout), so a caller that gives up does not fail the others waiting on it.
func (j *JWKS) refresh(ctx context.Context) error {
	j.mu.Lock()
	if c := j.inflight; c != nil {
		j.mu.Unlock()
		select {
		case <-c.done:
			return c.err
		case <-ctx.Done():
			return ctx.Err()
		}
	}
	c := &jwksCall{done: make(chan struct{})}
	j.inflight = c
	j.triedAt = j.clock.Now()
	j.fetches++
	j.mu.Unlock()

	fctx, cancel := context.WithTimeout(context.WithoutCancel(ctx), HTTPTimeout)
	keys, ttl, err := j.fetch(fctx)
	cancel()

	j.mu.Lock()
	now := j.clock.Now()
	if err == nil {
		j.keys, j.ttl, j.fetchedAt, j.failedAt = keys, ttl, now, time.Time{}
	} else {
		j.failedAt = now
	}
	j.inflight = nil
	c.err = err
	close(c.done)
	j.mu.Unlock()
	return err
}

func (j *JWKS) fetch(ctx context.Context) ([]jwk, time.Duration, error) {
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, j.url, nil)
	if err != nil {
		return nil, 0, &Error{Code: "jwks_unavailable"}
	}
	req.Header.Set("Accept", "application/jwk-set+json, application/json")
	req.Header.Set("User-Agent", UserAgent)
	res, err := j.client.Do(req)
	if err != nil {
		if errors.Is(err, ErrInsecureRedirect) {
			return nil, 0, &Error{Code: "insecure_redirect"}
		}
		if errors.Is(err, context.DeadlineExceeded) || errors.Is(err, context.Canceled) {
			return nil, 0, err
		}
		return nil, 0, &Error{Code: "jwks_unavailable"}
	}
	defer res.Body.Close()
	body, err := io.ReadAll(io.LimitReader(res.Body, maxJWKSBytes+1))
	if err != nil || res.StatusCode != http.StatusOK || len(body) > maxJWKSBytes {
		return nil, 0, &Error{Code: "jwks_unavailable"}
	}
	keys, err := ParseJWKS(body)
	if err != nil {
		return nil, 0, err
	}
	return keys, cacheTTL(res.Header.Get("Cache-Control")), nil
}

// cacheTTL reads max-age from Cache-Control, clamped to [JWKSMinTTL, JWKSMaxTTL].
func cacheTTL(cc string) time.Duration {
	ttl := JWKSDefaultTTL
	for _, d := range strings.Split(cc, ",") {
		d = strings.TrimSpace(strings.ToLower(d))
		if v, ok := strings.CutPrefix(d, "max-age="); ok {
			if n, err := strconv.ParseInt(strings.Trim(v, `"`), 10, 64); err == nil && n >= 0 {
				ttl = time.Duration(min(n, int64(JWKSMaxTTL/time.Second))) * time.Second
			}
		}
	}
	return max(JWKSMinTTL, min(ttl, JWKSMaxTTL))
}

type rawJWK struct {
	Kty string `json:"kty"`
	Kid string `json:"kid"`
	Use string `json:"use"`
	Alg string `json:"alg"`
	N   string `json:"n"`
	E   string `json:"e"`
	Crv string `json:"crv"`
	X   string `json:"x"`
	Y   string `json:"y"`
}

// ParseJWKS reads the RSA (at least 2048 bits) and P-256 signing keys of a key set. Encryption
// keys, other key types and curves, and keys whose alg is neither RS256 nor ES256 are skipped; a
// set without any usable key is an error.
func ParseJWKS(body []byte) ([]jwk, error) {
	var set struct {
		Keys []json.RawMessage `json:"keys"`
	}
	if err := json.Unmarshal(body, &set); err != nil {
		return nil, &Error{Code: "jwks_bad_response"}
	}
	var out []jwk
	for _, rk := range set.Keys {
		if len(out) == maxJWKSKeys {
			break
		}
		var k rawJWK
		if json.Unmarshal(rk, &k) != nil || (k.Use != "" && k.Use != "sig") {
			continue
		}
		switch k.Kty {
		case "RSA":
			if k.Alg != "" && k.Alg != AlgRS256 {
				continue
			}
			if pub := rsaKey(k); pub != nil {
				out = append(out, jwk{kid: k.Kid, alg: k.Alg, key: pub})
			}
		case "EC":
			if k.Alg != "" && k.Alg != AlgES256 {
				continue
			}
			if pub := p256Key(k); pub != nil {
				out = append(out, jwk{kid: k.Kid, alg: k.Alg, key: pub})
			}
		}
	}
	if len(out) == 0 {
		return nil, &Error{Code: "jwks_no_keys"}
	}
	return out, nil
}

func rsaKey(k rawJWK) *rsa.PublicKey {
	n, err1 := b64urlDecode(k.N)
	e, err2 := b64urlDecode(k.E)
	if err1 != nil || err2 != nil || len(e) == 0 || len(e) > 4 {
		return nil
	}
	mod := new(big.Int).SetBytes(n)
	exp := new(big.Int).SetBytes(e)
	if mod.BitLen() < minRSABits || !exp.IsInt64() || exp.Int64() < 3 || exp.Int64() > maxRSAExponent || exp.Bit(0) == 0 {
		return nil
	}
	return &rsa.PublicKey{N: mod, E: int(exp.Int64())}
}

func p256Key(k rawJWK) *ecdsa.PublicKey {
	if k.Crv != "P-256" {
		return nil
	}
	x, err1 := b64urlDecode(k.X)
	y, err2 := b64urlDecode(k.Y)
	if err1 != nil || err2 != nil || len(x) != 32 || len(y) != 32 {
		return nil
	}
	pub, err := ecdsa.ParseUncompressedPublicKey(elliptic.P256(), append(append([]byte{4}, x...), y...))
	if err != nil {
		return nil
	}
	return pub
}
