package providers

import (
	"bytes"
	"context"
	"crypto/rand"
	"crypto/rsa"
	"encoding/json"
	"errors"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"net/url"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"golang.org/x/oauth2"

	"github.com/mihajlopopara/reqharbor/server/internal/clock"
)

// The JWKS is fetched once and cached for its max-age; an unknown kid refetches it (rotation),
// at most once a minute; a failed refresh keeps the keys already known.
func TestJWKSCachingAndRotation(t *testing.T) {
	testKeys(t)
	srv := newJWKSServer(t, rsaJWK("rsa-1", rsaKey1))
	clk := clock.NewFake(testNow)
	v := newVerifier(srv, clk)
	tok := func(kid string, key *rsa.PrivateKey) string {
		c := validClaims()
		c["exp"] = json.Number(strconvF(clk.Now().Add(time.Hour)))
		c["iat"] = json.Number(strconvF(clk.Now()))
		return sign(t, map[string]any{"alg": "RS256", "kid": kid}, c, key)
	}
	ok := func(token string) {
		t.Helper()
		if _, err := v.verify(context.Background(), token, "n-123"); err != nil {
			t.Fatalf("verify: %v", err)
		}
	}
	ok(tok("rsa-1", rsaKey1))
	ok(tok("rsa-1", rsaKey1))
	if n := srv.count(); n != 1 {
		t.Fatalf("fetches %d, want 1 (cached)", n)
	}

	// Rotation: the IdP now signs with rsa-2. Within a minute of the last fetch, an unknown kid
	// does not refetch.
	srv.set(0, rsaJWK("rsa-1", rsaKey1), rsaJWK("rsa-2", rsaKey2))
	clk.Advance(30 * time.Second)
	if _, err := v.verify(context.Background(), tok("rsa-2", rsaKey2), "n-123"); ErrorCode(err) != "jwks_unknown_kid" {
		t.Fatalf("unknown kid within the refetch interval: %v", err)
	}
	if n := srv.count(); n != 1 {
		t.Fatalf("fetches %d, want 1", n)
	}
	clk.Advance(31 * time.Second)
	ok(tok("rsa-2", rsaKey2))
	if n := srv.count(); n != 2 {
		t.Fatalf("fetches %d, want 2 (rotation refetch)", n)
	}
	// Made-up kids cannot hammer the IdP: one refetch per minute at most.
	for range 5 {
		_, _ = v.verify(context.Background(), tok("bogus", rsaKey1), "n-123")
	}
	if n := srv.count(); n != 2 {
		t.Fatalf("fetches %d, want 2", n)
	}

	// The cache expires after max-age (600 s here); a failed refresh keeps the old keys.
	srv.set(http.StatusInternalServerError)
	clk.Advance(11 * time.Minute)
	ok(tok("rsa-2", rsaKey2))
	if n := srv.count(); n != 3 {
		t.Fatalf("fetches %d, want 3 (expired, refresh failed, stale keys used)", n)
	}
	// The old key was retired: once the set comes back without rsa-1, tokens signed with it fail.
	srv.set(0, rsaJWK("rsa-2", rsaKey2))
	clk.Advance(2 * time.Minute)
	ok(tok("rsa-2", rsaKey2))
	if _, err := v.verify(context.Background(), tok("rsa-1", rsaKey1), "n-123"); ErrorCode(err) != "jwks_unknown_kid" {
		t.Fatalf("retired key: %v", err)
	}
}

func strconvF(t time.Time) string { return strconv.FormatInt(t.Unix(), 10) }

func TestJWKSUnavailable(t *testing.T) {
	testKeys(t)
	srv := newJWKSServer(t)
	srv.set(http.StatusServiceUnavailable)
	clk := clock.NewFake(testNow)
	v := newVerifier(srv, clk)
	tok := sign(t, map[string]any{"alg": "RS256", "kid": "rsa-1"}, validClaims(), rsaKey1)
	if _, err := v.verify(context.Background(), tok, "n-123"); ErrorCode(err) != "jwks_unavailable" {
		t.Fatalf("%v", err)
	}
	// With no key cached, a failed fetch is not retried for 10 s: sign-ins fail fast.
	srv.set(0, rsaJWK("rsa-1", rsaKey1))
	clk.Advance(JWKSFailureBackoff - time.Second)
	for range 3 {
		if _, err := v.verify(context.Background(), tok, "n-123"); ErrorCode(err) != "jwks_unavailable" {
			t.Fatalf("within the backoff: %v", err)
		}
	}
	if n := srv.count(); n != 1 {
		t.Fatalf("fetches %d, want 1", n)
	}
	clk.Advance(time.Second)
	if _, err := v.verify(context.Background(), tok, "n-123"); err != nil {
		t.Fatalf("after the backoff: %v", err)
	}
	if n := srv.count(); n != 2 {
		t.Fatalf("fetches %d, want 2", n)
	}
}

// slowJWKS is a key set whose responses wait for release.
func slowJWKS(t *testing.T, keys ...map[string]any) (*httptest.Server, *atomic.Int32, chan struct{}) {
	var hits atomic.Int32
	release := make(chan struct{})
	srv := httptest.NewTLSServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		hits.Add(1)
		select {
		case <-release:
		case <-r.Context().Done():
			return
		}
		_ = json.NewEncoder(w).Encode(map[string]any{"keys": keys})
	}))
	t.Cleanup(srv.Close)
	return srv, &hits, release
}

// Concurrent sign-ins that need the set share one fetch, and the lock is not held during it.
func TestJWKSSingleFlight(t *testing.T) {
	testKeys(t)
	srv, hits, release := slowJWKS(t, rsaJWK("rsa-1", rsaKey1), rsaJWK("rsa-2", rsaKey2))
	clk := clock.NewFake(testNow)
	j := NewJWKS(srv.URL, SecureClient(srv.Client()), clk)
	var wg sync.WaitGroup
	errs := make(chan error, 8)
	for range 8 {
		wg.Add(1)
		go func() {
			defer wg.Done()
			_, err := j.Keys(context.Background(), "rsa-1", AlgRS256)
			errs <- err
		}()
	}
	deadline := time.Now().Add(5 * time.Second)
	for hits.Load() == 0 && time.Now().Before(deadline) {
		time.Sleep(time.Millisecond)
	}
	// The lock is free while the fetch is in flight.
	if n := j.Fetches(); n != 1 {
		t.Fatalf("fetches %d, want 1", n)
	}
	close(release)
	wg.Wait()
	close(errs)
	for err := range errs {
		if err != nil {
			t.Fatal(err)
		}
	}
	if n := hits.Load(); n != 1 {
		t.Fatalf("requests %d, want 1", n)
	}
}

// A rotation refetch that hangs does not block sign-ins whose key is cached; a waiter whose own
// context ends gives up without cancelling the shared fetch.
func TestJWKSSlowRefetchDoesNotBlockCachedKeys(t *testing.T) {
	testKeys(t)
	var hits atomic.Int32
	release := make(chan struct{})
	srv := httptest.NewTLSServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if hits.Add(1) > 1 {
			select {
			case <-release:
			case <-r.Context().Done():
				return
			}
		}
		_ = json.NewEncoder(w).Encode(map[string]any{"keys": []any{rsaJWK("rsa-1", rsaKey1), rsaJWK("rsa-2", rsaKey2)}})
	}))
	t.Cleanup(srv.Close)
	clk := clock.NewFake(testNow)
	j := NewJWKS(srv.URL, SecureClient(srv.Client()), clk)
	if _, err := j.Keys(context.Background(), "rsa-1", AlgRS256); err != nil {
		t.Fatal(err)
	}
	clk.Advance(2 * time.Minute)
	done := make(chan error, 1)
	go func() { _, err := j.Keys(context.Background(), "rsa-new", AlgRS256); done <- err }()
	deadline := time.Now().Add(5 * time.Second)
	for hits.Load() < 2 && time.Now().Before(deadline) {
		time.Sleep(time.Millisecond)
	}
	start := time.Now()
	if _, err := j.Keys(context.Background(), "rsa-2", AlgRS256); err != nil || time.Since(start) > time.Second {
		t.Fatalf("cached key blocked by the refetch: %v after %v", err, time.Since(start))
	}
	ctx, cancel := context.WithTimeout(context.Background(), 20*time.Millisecond)
	defer cancel()
	if _, err := j.Keys(ctx, "rsa-new", AlgRS256); !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("waiter with an ended context: %v", err)
	}
	close(release)
	if err := <-done; ErrorCode(err) != "jwks_unknown_kid" {
		t.Fatalf("rotation refetch: %v", err)
	}
	if n := hits.Load(); n != 2 {
		t.Fatalf("requests %d, want 2", n)
	}
}

// The response is capped at 1 MiB.
func TestJWKSSizeCap(t *testing.T) {
	testKeys(t)
	big := map[string]any{"keys": []any{rsaJWK("rsa-1", rsaKey1)}, "pad": strings.Repeat("x", maxJWKSBytes)}
	srv := httptest.NewTLSServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) { _ = json.NewEncoder(w).Encode(big) }))
	t.Cleanup(srv.Close)
	j := NewJWKS(srv.URL, SecureClient(srv.Client()), clock.NewFake(testNow))
	if _, err := j.Keys(context.Background(), "rsa-1", AlgRS256); ErrorCode(err) != "jwks_unavailable" {
		t.Fatalf("%v", err)
	}
}

func TestParseJWKS(t *testing.T) {
	testKeys(t)
	small, err := rsa.GenerateKey(rand.Reader, 1024)
	if err != nil {
		t.Fatal(err)
	}
	good := rsaJWK("good", rsaKey1)
	ec := ecJWK("ec", ecKey1)
	mut := func(base map[string]any, k string, v any) map[string]any {
		out := map[string]any{}
		for kk, vv := range base {
			out[kk] = vv
		}
		if v == nil {
			delete(out, k)
		} else {
			out[k] = v
		}
		return out
	}
	skipped := []map[string]any{
		mut(good, "use", "enc"),
		mut(good, "alg", "RS512"),
		mut(good, "n", "!!"),
		mut(good, "e", enc([]byte{2})), // even exponent
		mut(ec, "crv", "P-384"),
		mut(ec, "alg", "ES384"),
		mut(ec, "x", enc([]byte{1, 2, 3})),
		mut(ec, "y", enc(bytes.Repeat([]byte{0xff}, 32))), // not on the curve
		{"kty": "oct", "k": enc([]byte("secret")), "kid": "hmac"},
		{"kty": "OKP", "crv": "Ed25519", "x": enc(make([]byte, 32))},
	}
	skipped = append(skipped, rsaJWK("small", small))
	body, _ := json.Marshal(map[string]any{"keys": skipped})
	if _, err := ParseJWKS(body); ErrorCode(err) != "jwks_no_keys" {
		t.Fatalf("every key unusable: %v", err)
	}
	body, _ = json.Marshal(map[string]any{"keys": append(skipped, good, mut(ec, "alg", nil), mut(good, "use", nil))})
	keys, err := ParseJWKS(body)
	if err != nil || len(keys) != 3 {
		t.Fatalf("%d keys, %v", len(keys), err)
	}
	if _, err := ParseJWKS([]byte(`<html>`)); ErrorCode(err) != "jwks_bad_response" {
		t.Fatalf("%v", err)
	}
}

func TestCacheTTL(t *testing.T) {
	for cc, want := range map[string]time.Duration{
		"":                             JWKSDefaultTTL,
		"public, max-age=600":          10 * time.Minute,
		"max-age=10":                   JWKSMinTTL,
		"max-age=9999999":              JWKSMaxTTL,
		"no-cache":                     JWKSDefaultTTL,
		"max-age=abc":                  JWKSDefaultTTL,
		"private, max-age=\"7200\"":    2 * time.Hour,
		"max-age=99999999999999999999": JWKSDefaultTTL,
	} {
		if got := cacheTTL(cc); got != want {
			t.Errorf("%q: %v, want %v", cc, got, want)
		}
	}
}

// A redirect from https to http, or to http on a non-loopback host, is refused before it is
// followed; https to https and loopback http to loopback http are followed.
func TestSecureClientRedirects(t *testing.T) {
	var plainHits atomic.Int32
	plain := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		plainHits.Add(1)
		_, _ = w.Write([]byte(`{}`))
	}))
	defer plain.Close()
	var target atomic.Value
	tlsSrv := httptest.NewTLSServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path == "/final" {
			_, _ = w.Write([]byte(`{}`))
			return
		}
		http.Redirect(w, r, target.Load().(string), http.StatusTemporaryRedirect)
	}))
	defer tlsSrv.Close()
	c := SecureClient(tlsSrv.Client())

	target.Store(plain.URL + "/token") // https → http (loopback): a downgrade
	if _, err := c.Post(tlsSrv.URL+"/token", "application/x-www-form-urlencoded", strings.NewReader("a=b")); !errors.Is(err, ErrInsecureRedirect) {
		t.Fatalf("https → http: %v", err)
	}
	if plainHits.Load() != 0 {
		t.Fatal("the http target was contacted")
	}
	target.Store(tlsSrv.URL + "/final") // https → https
	if res, err := c.Get(tlsSrv.URL + "/start"); err != nil || res.StatusCode != 200 {
		t.Fatalf("https → https: %v", err)
	} else {
		res.Body.Close()
	}

	check := SecureClient(&http.Client{}).CheckRedirect
	req := func(raw string) *http.Request { u, _ := url.Parse(raw); return &http.Request{URL: u} }
	for _, tc := range []struct {
		from, to string
		ok       bool
	}{
		{"http://127.0.0.1:5556/token", "http://127.0.0.1:5556/token2", true},
		{"http://localhost:5556/token", "http://[::1]:5556/x", true},
		{"http://127.0.0.1:5556/token", "http://idp.example.com/token", false},
		{"https://idp.example.com/token", "http://idp.example.com/token", false},
		{"https://idp.example.com/token", "http://127.0.0.1/token", false},
		{"https://idp.example.com/token", "https://login.example.com/token", true},
		{"https://idp.example.com/token", "ftp://idp.example.com/token", false},
	} {
		err := check(req(tc.to), []*http.Request{req(tc.from)})
		if (err == nil) != tc.ok {
			t.Errorf("%s → %s: %v", tc.from, tc.to, err)
		}
	}
	via := make([]*http.Request, maxRedirects)
	for i := range via {
		via[i] = req("https://idp.example.com/")
	}
	if check(req("https://idp.example.com/"), via) == nil {
		t.Error("redirect loop followed")
	}
}

// fakeDiscovery serves a discovery document for {srv}/realms/acme.
type fakeDiscovery struct {
	*httptest.Server
	issuer string
	mu     sync.Mutex
	fails  int
	body   string
	mutate func(map[string]any)
	hits   int
}

func (f *fakeDiscovery) count() int {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.hits
}

func newFakeDiscovery(t *testing.T) *fakeDiscovery {
	f := &fakeDiscovery{}
	f.Server = httptest.NewTLSServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		f.mu.Lock()
		defer f.mu.Unlock()
		f.hits++
		if r.URL.Path != "/realms/acme/.well-known/openid-configuration" {
			http.NotFound(w, r)
			return
		}
		if f.fails > 0 {
			f.fails--
			w.WriteHeader(http.StatusServiceUnavailable)
			return
		}
		if f.body != "" {
			_, _ = w.Write([]byte(f.body))
			return
		}
		doc := map[string]any{"issuer": f.issuer, "authorization_endpoint": f.URL + "/authorize", "token_endpoint": f.URL + "/token",
			"userinfo_endpoint": f.URL + "/userinfo", "jwks_uri": f.URL + "/jwks", "id_token_signing_alg_values_supported": []string{"RS256"}}
		if f.mutate != nil {
			f.mutate(doc)
		}
		_ = json.NewEncoder(w).Encode(doc)
	}))
	f.issuer = f.URL + "/realms/acme"
	t.Cleanup(f.Close)
	return f
}

func (f *fakeDiscovery) oidc(t *testing.T, logs *bytes.Buffer, env ...string) *OIDC {
	c := cfg(append([]string{"REQHARBOR_OIDC_ISSUER=" + f.issuer, "REQHARBOR_OIDC_CLIENT_ID=client-1", "REQHARBOR_OIDC_CLIENT_SECRET=s3cret"}, env...)...)
	c.Test.OIDCDiscoveryBackoff = time.Millisecond
	if logs == nil {
		logs = &bytes.Buffer{}
	}
	ps := Enabled(c, Deps{Client: f.Client(), Clock: clock.NewFake(testNow), Logger: slog.New(slog.NewTextHandler(&syncWriter{w: logs}, nil))})
	if len(ps) != 1 {
		t.Fatalf("providers %v", ps)
	}
	return ps[0].(*OIDC)
}

type syncWriter struct {
	mu sync.Mutex
	w  *bytes.Buffer
}

func (s *syncWriter) Write(p []byte) (int, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.w.Write(p)
}

func TestOIDCDiscoveryChecks(t *testing.T) {
	for _, tc := range []struct {
		name   string
		mutate func(f *fakeDiscovery, doc map[string]any)
		perm   string // substring of the ConfigError; "" for success
	}{
		{"valid", nil, ""},
		{"issuer mismatch", func(f *fakeDiscovery, d map[string]any) { d["issuer"] = "https://other.example.com/realms/acme" }, "differs from REQHARBOR_OIDC_ISSUER"},
		{"issuer with a trailing slash", func(f *fakeDiscovery, d map[string]any) { d["issuer"] = f.issuer + "/" }, "differs from REQHARBOR_OIDC_ISSUER"},
		{"http token_endpoint", func(f *fakeDiscovery, d map[string]any) { d["token_endpoint"] = "http://idp.example.com/token" }, "token_endpoint"},
		{"http authorization_endpoint", func(f *fakeDiscovery, d map[string]any) { d["authorization_endpoint"] = "http://idp.example.com/auth" }, "authorization_endpoint"},
		{"http userinfo_endpoint", func(f *fakeDiscovery, d map[string]any) { d["userinfo_endpoint"] = "http://idp.example.com/userinfo" }, "userinfo_endpoint"},
		{"http jwks_uri", func(f *fakeDiscovery, d map[string]any) { d["jwks_uri"] = "http://idp.example.com/jwks" }, "jwks_uri"},
		{"relative token_endpoint", func(f *fakeDiscovery, d map[string]any) { d["token_endpoint"] = "/token" }, "token_endpoint"},
		{"http loopback endpoints are allowed", func(f *fakeDiscovery, d map[string]any) {
			d["token_endpoint"] = "http://127.0.0.1:5556/token"
			d["userinfo_endpoint"] = "http://localhost:5556/userinfo"
		}, ""},
		{"no userinfo endpoint is fine", func(f *fakeDiscovery, d map[string]any) { delete(d, "userinfo_endpoint") }, ""},
		{"no token endpoint", func(f *fakeDiscovery, d map[string]any) { delete(d, "token_endpoint") }, "no token_endpoint"},
		{"no jwks_uri", func(f *fakeDiscovery, d map[string]any) { delete(d, "jwks_uri") }, "no jwks_uri"},
		{"HS256 only", func(f *fakeDiscovery, d map[string]any) {
			d["id_token_signing_alg_values_supported"] = []string{"HS256"}
		}, "RS256 and ES256"},
		{"ES256 only", func(f *fakeDiscovery, d map[string]any) {
			d["id_token_signing_alg_values_supported"] = []string{"ES256"}
		}, ""},
		{"private_key_jwt only", func(f *fakeDiscovery, d map[string]any) {
			d["token_endpoint_auth_methods_supported"] = []string{"private_key_jwt"}
		}, "neither client_secret_basic nor client_secret_post"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			f := newFakeDiscovery(t)
			if tc.mutate != nil {
				f.mutate = func(d map[string]any) { tc.mutate(f, d) }
			}
			_, err := f.oidc(t, nil).Discover(context.Background())
			var ce *ConfigError
			switch {
			case tc.perm == "" && err != nil:
				t.Fatalf("want success: %v", err)
			case tc.perm != "" && (!errors.As(err, &ce) || !strings.Contains(ce.Msg, tc.perm)):
				t.Fatalf("want a config error about %q, got %v", tc.perm, err)
			}
		})
	}
}

func TestOIDCClientAuthStyle(t *testing.T) {
	for _, tc := range []struct {
		methods []string
		want    oauth2.AuthStyle
	}{
		{nil, oauth2.AuthStyleInHeader},
		{[]string{"client_secret_post", "client_secret_basic"}, oauth2.AuthStyleInHeader},
		{[]string{"client_secret_post"}, oauth2.AuthStyleInParams},
	} {
		f := newFakeDiscovery(t)
		f.mutate = func(d map[string]any) {
			if tc.methods != nil {
				d["token_endpoint_auth_methods_supported"] = tc.methods
			}
		}
		fl, err := f.oidc(t, nil).Discover(context.Background())
		if err != nil || fl.oauth.Endpoint.AuthStyle != tc.want {
			t.Fatalf("%v: %v %v", tc.methods, err, fl)
		}
	}
}

// Transient failures are retried; the provider becomes available (and changed is called) only
// once discovery succeeds. A configuration error is never retried.
func TestOIDCRunRetriesThenEnables(t *testing.T) {
	f := newFakeDiscovery(t)
	f.fails = 3
	logs := &bytes.Buffer{}
	o := f.oidc(t, logs, "REQHARBOR_OIDC_NAME=Acme SSO", "REQHARBOR_OIDC_SCOPES=openid email groups")
	if o.Available() || o.AuthURL("s", "v", "n") != "" || o.Label() != "Acme SSO" {
		t.Fatal("available before discovery")
	}
	if _, err := o.Exchange(context.Background(), "c", "v", "n"); ErrorCode(err) != "not_discovered" {
		t.Fatalf("%v", err)
	}
	var changed atomic.Int32
	o.Run(context.Background(), func() { changed.Add(1) })
	if !o.Available() || changed.Load() != 1 || f.count() != 4 {
		t.Fatalf("available %v, changed %d, hits %d", o.Available(), changed.Load(), f.count())
	}
	if !strings.Contains(logs.String(), "OIDC discovery failed; retrying") || !strings.Contains(logs.String(), "discovery_status_503") ||
		!strings.Contains(logs.String(), "OIDC sign-in enabled") {
		t.Fatalf("logs: %s", logs)
	}
	u, _ := url.Parse(o.AuthURL("st", "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk", "nonce-1"))
	q := u.Query()
	if u.Scheme+"://"+u.Host+u.Path != f.URL+"/authorize" || q.Get("scope") != "openid email groups" || q.Get("nonce") != "nonce-1" ||
		q.Get("code_challenge_method") != "S256" || q.Get("redirect_uri") != "https://rh.example.com/auth/callback/oidc" || q.Has("prompt") {
		t.Fatalf("authorize URL %s", u)
	}

	bad := newFakeDiscovery(t)
	bad.mutate = func(d map[string]any) { d["issuer"] = "https://evil.example.com" }
	logs.Reset()
	o = bad.oidc(t, logs)
	o.Run(context.Background(), func() { t.Error("changed called") })
	if o.Available() || bad.count() != 1 || !strings.Contains(logs.String(), "OIDC sign-in disabled") {
		t.Fatalf("available %v, hits %d, logs %s", o.Available(), bad.count(), logs)
	}

	// A malformed document (a proxy's error page) is retried; ctx ends the loop.
	page := newFakeDiscovery(t)
	page.body = "<html>bad gateway</html>"
	o = page.oidc(t, nil)
	ctx, cancel := context.WithTimeout(context.Background(), 50*time.Millisecond)
	defer cancel()
	o.Run(ctx, nil)
	if o.Available() || page.count() < 2 {
		t.Fatalf("available %v, hits %d", o.Available(), page.count())
	}
}

func TestOIDCDiscoveryRedirectToHTTPIsAConfigError(t *testing.T) {
	plain := httptest.NewServer(http.NotFoundHandler())
	defer plain.Close()
	srv := httptest.NewTLSServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		http.Redirect(w, r, plain.URL+r.URL.Path, http.StatusFound)
	}))
	defer srv.Close()
	c := cfg("REQHARBOR_OIDC_ISSUER="+srv.URL, "REQHARBOR_OIDC_CLIENT_ID=c", "REQHARBOR_OIDC_CLIENT_SECRET=s")
	o := NewOIDC(c, Deps{Client: srv.Client(), Clock: clock.System{}, Logger: slog.Default()})
	var ce *ConfigError
	if _, err := o.Discover(context.Background()); !errors.As(err, &ce) {
		t.Fatalf("%v", err)
	}
}

func TestGoogleAuthURL(t *testing.T) {
	c := cfg("REQHARBOR_GOOGLE_CLIENT_ID=gid.apps.googleusercontent.com", "REQHARBOR_GOOGLE_CLIENT_SECRET=topsecret")
	ps := Enabled(c, Deps{})
	if len(ps) != 1 || ps[0].ID() != "google" || ps[0].Label() != "Google" || !IsAvailable(ps[0]) {
		t.Fatalf("%v", ps)
	}
	g := ps[0].(*Google)
	if g.CallbackURL() != "https://rh.example.com/auth/callback/google" {
		t.Fatal(g.CallbackURL())
	}
	u, _ := url.Parse(g.AuthURL("st", "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk", "nonce-1"))
	if u.Scheme+"://"+u.Host+u.Path != GoogleAuthURL {
		t.Fatal(u)
	}
	q := u.Query()
	for k, v := range map[string]string{"client_id": "gid.apps.googleusercontent.com", "state": "st", "scope": "openid email profile",
		"redirect_uri": "https://rh.example.com/auth/callback/google", "code_challenge_method": "S256", "prompt": "select_account",
		"code_challenge": "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM", "response_type": "code", "nonce": "nonce-1"} {
		if q.Get(k) != v {
			t.Errorf("%s = %q, want %q", k, q.Get(k), v)
		}
	}
	if strings.Contains(u.String(), "topsecret") {
		t.Fatal("secret in the authorize URL")
	}
	if _, err := g.Exchange(context.Background(), "c", "v", ""); ErrorCode(err) != "nonce_missing" {
		t.Fatalf("exchange without a nonce: %v", err)
	}
}

func TestEnabledOrder(t *testing.T) {
	c := cfg("REQHARBOR_GITHUB_CLIENT_ID=a", "REQHARBOR_GITHUB_CLIENT_SECRET=b", "REQHARBOR_GOOGLE_CLIENT_ID=c", "REQHARBOR_GOOGLE_CLIENT_SECRET=d",
		"REQHARBOR_OIDC_ISSUER=https://sso.example.com", "REQHARBOR_OIDC_CLIENT_ID=e", "REQHARBOR_OIDC_CLIENT_SECRET=f")
	var ids []string
	for _, p := range Enabled(c, Deps{}) {
		ids = append(ids, p.ID()+":"+boolStr(IsAvailable(p)))
	}
	if got := strings.Join(ids, ","); got != "github:true,google:true,oidc:false" {
		t.Fatal(got)
	}
	if _, ok := Enabled(c, Deps{})[2].(Dynamic); !ok {
		t.Fatal("oidc is not dynamic")
	}
}

func boolStr(b bool) string {
	if b {
		return "true"
	}
	return "false"
}
