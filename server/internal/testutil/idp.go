package testutil

// FakeIdP is an OpenID Provider on an httptest TLS server (06-go-server.md §10 S10), playing
// either Google or a generic OIDC issuer: a discovery document (that can fail, be rewritten or
// point at http endpoints), a JWKS with RS256 and ES256 signing keys that can rotate, a token
// endpoint that checks the client credentials, the redirect URI and PKCE and returns a signed ID
// token carrying the round trip's nonce, and a userinfo endpoint.

import (
	"crypto"
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/hmac"
	"crypto/rand"
	"crypto/rsa"
	"crypto/sha256"
	"encoding/asn1"
	"encoding/base64"
	"encoding/json"
	"math/big"
	"net/http"
	"net/http/httptest"
	"net/url"
	"strconv"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/mihajlopopara/reqharbor/server/internal/clock"
	"github.com/mihajlopopara/reqharbor/server/internal/config"
)

// IdPKey is a signing key of the fake.
type IdPKey struct {
	Kid string
	Alg string // RS256 or ES256
	rsa *rsa.PrivateKey
	ec  *ecdsa.PrivateKey
}

// NewIdPKey generates a key for alg (RS256: RSA 2048; ES256: P-256).
func NewIdPKey(t testing.TB, kid, alg string) *IdPKey {
	t.Helper()
	k := &IdPKey{Kid: kid, Alg: alg}
	var err error
	switch alg {
	case "RS256":
		k.rsa, err = rsa.GenerateKey(rand.Reader, 2048)
	case "ES256":
		k.ec, err = ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	default:
		t.Fatalf("NewIdPKey: alg %s", alg)
	}
	if err != nil {
		t.Fatal(err)
	}
	return k
}

// JWK is the key's public JSON Web Key.
func (k *IdPKey) JWK() map[string]any {
	enc := base64.RawURLEncoding.EncodeToString
	if k.rsa != nil {
		return map[string]any{"kty": "RSA", "kid": k.Kid, "use": "sig", "alg": "RS256",
			"n": enc(k.rsa.N.Bytes()), "e": enc(big.NewInt(int64(k.rsa.E)).Bytes())}
	}
	b, _ := k.ec.PublicKey.Bytes() // 0x04 || x || y
	return map[string]any{"kty": "EC", "kid": k.Kid, "use": "sig", "alg": "ES256", "crv": "P-256",
		"x": enc(b[1:33]), "y": enc(b[33:65])}
}

// Sign returns the compact JWS of claims with header (alg and kid from the key unless header sets
// them). alg "none" gives an unsigned token; "HS256" signs with hsKey.
func (k *IdPKey) Sign(t testing.TB, header, claims map[string]any, hsKey []byte) string {
	t.Helper()
	s, err := k.sign(header, claims, hsKey)
	if err != nil {
		t.Fatal(err)
	}
	return s
}

func (k *IdPKey) sign(header, claims map[string]any, hsKey []byte) (string, error) {
	h := map[string]any{"alg": k.Alg, "kid": k.Kid, "typ": "JWT"}
	for name, v := range header {
		if v == nil {
			delete(h, name)
		} else {
			h[name] = v
		}
	}
	hb, _ := json.Marshal(h)
	cb, _ := json.Marshal(claims)
	enc := base64.RawURLEncoding.EncodeToString
	input := enc(hb) + "." + enc(cb)
	sum := sha256.Sum256([]byte(input))
	var sig []byte
	switch h["alg"] {
	case "none":
	case "HS256":
		m := hmac.New(sha256.New, hsKey)
		m.Write([]byte(input))
		sig = m.Sum(nil)
	default:
		var err error
		if k.rsa != nil {
			sig, err = rsa.SignPKCS1v15(rand.Reader, k.rsa, crypto.SHA256, sum[:])
		} else {
			var der []byte
			der, err = ecdsa.SignASN1(rand.Reader, k.ec, sum[:])
			var rs struct{ R, S *big.Int }
			if err == nil {
				_, err = asn1.Unmarshal(der, &rs)
			}
			sig = make([]byte, 64)
			if err == nil {
				rs.R.FillBytes(sig[:32])
				rs.S.FillBytes(sig[32:])
			}
		}
		if err != nil {
			return "", err
		}
	}
	return input + "." + enc(sig), nil
}

var (
	idpKeysOnce sync.Once
	idpRSA      *IdPKey
	idpEC       *IdPKey
)

// sharedIdPKeys are generated once per test binary (RSA generation is slow under -race).
func sharedIdPKeys(t testing.TB) (*IdPKey, *IdPKey) {
	idpKeysOnce.Do(func() {
		idpRSA = NewIdPKey(t, "rsa-1", "RS256")
		idpEC = NewIdPKey(t, "ec-1", "ES256")
	})
	return idpRSA, idpEC
}

// IdPUser is an account at the fake IdP and how its tokens look.
type IdPUser struct {
	Sub, Email, Name, Picture string
	// EmailVerified is the email_verified claim: true, false, "true", … or nil to leave it out.
	EmailVerified any
	// NoEmailInIDToken leaves email and email_verified out of the ID token (userinfo has them).
	NoEmailInIDToken bool
	// UserinfoSub replaces sub in the userinfo response.
	UserinfoSub string
	// Claims are added to (or, with a nil value, removed from) the ID token's claims.
	Claims map[string]any
	// Header overrides the ID token's JWS header (a nil value removes the field).
	Header map[string]any
	// Key signs the ID token instead of the fake's current key.
	Key *IdPKey
}

type idpGrant struct {
	user      *IdPUser
	challenge string
	redirect  string
	nonce     string
}

// FakeIdP is the fake provider.
type FakeIdP struct {
	Server       *httptest.Server
	ProviderID   string // "google" or "oidc"
	Issuer       string // the discovery issuer (OIDC) or the iss of Google's tokens
	ClientID     string
	ClientSecret string
	// Scope is the scope the authorize URL must ask for.
	Scope string

	mu            sync.Mutex
	clock         clock.Clock
	signing       *IdPKey
	published     []*IdPKey
	codes         map[string]idpGrant
	access        map[string]*IdPUser
	docMutate     func(map[string]any)
	docFailures   int
	tokenRedirect string
	clientAuth    string // "basic" or "post": what the token endpoint requires ("" = either)
	hits          map[string]int
	accessTokens  []string
	idTokens      []string
	n             int
}

// NewFakeOIDC starts a generic OIDC provider whose issuer is {server}/realms/acme.
func NewFakeOIDC(t testing.TB) *FakeIdP {
	f := newFakeIdP(t, "oidc")
	f.Issuer = f.Server.URL + "/realms/acme"
	f.Scope = "openid email profile"
	return f
}

// NewFakeGoogle starts a fake Google: ID tokens say iss https://accounts.google.com and the
// authorize URL must carry prompt=select_account.
func NewFakeGoogle(t testing.TB) *FakeIdP {
	f := newFakeIdP(t, "google")
	f.Issuer = "https://accounts.google.com"
	f.Scope = "openid email profile"
	f.clientAuth = "post"
	return f
}

func newFakeIdP(t testing.TB, id string) *FakeIdP {
	t.Helper()
	rk, _ := sharedIdPKeys(t)
	f := &FakeIdP{ProviderID: id, ClientID: id + "-client-" + b64(6), ClientSecret: "idp-secret-" + b64(12),
		clock: clock.System{}, signing: rk, codes: map[string]idpGrant{}, access: map[string]*IdPUser{}, hits: map[string]int{}}
	_, ek := sharedIdPKeys(t)
	f.published = []*IdPKey{rk, ek}
	mux := http.NewServeMux()
	mux.HandleFunc("GET /realms/acme/.well-known/openid-configuration", f.discovery)
	mux.HandleFunc("POST /token", f.token)
	mux.HandleFunc("GET /userinfo", f.userinfo)
	mux.HandleFunc("GET /jwks", f.jwks)
	f.Server = httptest.NewTLSServer(mux)
	t.Cleanup(f.Server.Close)
	return f
}

// Options configure a harness to use this fake (and share the harness clock for token times).
func (f *FakeIdP) Options() []Option {
	opts := []Option{WithHTTPClient(f.Server.Client()), whenStarted(func(h *Harness) {
		f.mu.Lock()
		f.clock = h.Clock
		f.mu.Unlock()
	})}
	if f.ProviderID == "google" {
		return append(opts,
			WithEnv("REQHARBOR_GOOGLE_CLIENT_ID="+f.ClientID, "REQHARBOR_GOOGLE_CLIENT_SECRET="+f.ClientSecret),
			WithConfig(func(c *config.Config) {
				c.Test.GoogleAuthURL = f.Server.URL + "/authorize"
				c.Test.GoogleTokenURL = f.Server.URL + "/token"
				c.Test.GoogleJWKSURL = f.Server.URL + "/jwks"
			}))
	}
	return append(opts,
		WithEnv("REQHARBOR_OIDC_ISSUER="+f.Issuer, "REQHARBOR_OIDC_CLIENT_ID="+f.ClientID, "REQHARBOR_OIDC_CLIENT_SECRET="+f.ClientSecret),
		WithConfig(func(c *config.Config) { c.Test.OIDCDiscoveryBackoff = 5 * time.Millisecond }))
}

// SetDoc rewrites the discovery document before it is served.
func (f *FakeIdP) SetDoc(mutate func(doc map[string]any)) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.docMutate = mutate
}

// FailDiscovery makes the next n discovery requests answer 503.
func (f *FakeIdP) FailDiscovery(n int) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.docFailures = n
}

// RedirectToken makes the token endpoint answer 307 to target.
func (f *FakeIdP) RedirectToken(target string) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.tokenRedirect = target
}

// RequireClientAuth makes the token endpoint accept only "basic" or "post" client credentials.
func (f *FakeIdP) RequireClientAuth(style string) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.clientAuth = style
}

// Rotate signs from now on with key and publishes keys (the JWKS).
func (f *FakeIdP) Rotate(key *IdPKey, published ...*IdPKey) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.signing, f.published = key, published
}

// Hits is how many requests the endpoint ("discovery", "token", "userinfo", "jwks") received.
func (f *FakeIdP) Hits(endpoint string) int {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.hits[endpoint]
}

// Tokens are every access and ID token the fake issued (tests check none is logged or stored).
func (f *FakeIdP) Tokens() []string {
	f.mu.Lock()
	defer f.mu.Unlock()
	return append(append([]string(nil), f.accessTokens...), f.idTokens...)
}

// CallbackURL is the redirect URI the server must send.
func (f *FakeIdP) CallbackURL() string {
	return "https://" + CanonicalHost + "/auth/callback/" + f.ProviderID
}

func (f *FakeIdP) checkAuthorize(t testing.TB, location string) url.Values {
	t.Helper()
	u, err := url.Parse(location)
	if err != nil || u.Scheme+"://"+u.Host != f.Server.URL || u.Path != "/authorize" {
		t.Fatalf("not the fake's authorize URL: %q", location)
	}
	q := u.Query()
	want := map[string]string{
		"client_id":             f.ClientID,
		"redirect_uri":          f.CallbackURL(),
		"scope":                 f.Scope,
		"code_challenge_method": "S256",
		"response_type":         "code",
	}
	if f.ProviderID == "google" {
		want["prompt"] = "select_account"
	}
	for k, v := range want {
		if q.Get(k) != v {
			t.Fatalf("authorize %s = %q, want %q (%s)", k, q.Get(k), v, location)
		}
	}
	if len(q.Get("code_challenge")) != 43 || q.Get("state") == "" || len(q.Get("nonce")) < 22 {
		t.Fatalf("authorize without challenge, state or nonce: %s", location)
	}
	if q.Has("client_secret") || strings.Contains(location, f.ClientSecret) {
		t.Fatal("the client secret is in the authorize URL")
	}
	return q
}

// Authorize plays the IdP's authorize page for location (the 302 of /auth/start/{provider}) as
// u: it checks the query, mints a code bound to the challenge, redirect URI and nonce, and returns
// the callback path with code and state.
func (f *FakeIdP) Authorize(t testing.TB, location string, u *IdPUser) string {
	t.Helper()
	q := f.checkAuthorize(t, location)
	f.mu.Lock()
	f.n++
	code := "idpcode" + strconv.Itoa(f.n) + b64(8)
	f.codes[code] = idpGrant{user: u, challenge: q.Get("code_challenge"), redirect: q.Get("redirect_uri"), nonce: q.Get("nonce")}
	f.mu.Unlock()
	return "/auth/callback/" + f.ProviderID + "?" + url.Values{"code": {code}, "state": {q.Get("state")}}.Encode()
}

// Deny plays the user cancelling at the IdP.
func (f *FakeIdP) Deny(t testing.TB, location string) string {
	t.Helper()
	q := f.checkAuthorize(t, location)
	return "/auth/callback/" + f.ProviderID + "?" + url.Values{"error": {"access_denied"}, "state": {q.Get("state")}}.Encode()
}

// Doc is the discovery document as served (before SetDoc's rewrite).
func (f *FakeIdP) Doc() map[string]any {
	base := f.Server.URL
	return map[string]any{
		"issuer":                                f.Issuer,
		"authorization_endpoint":                base + "/authorize",
		"token_endpoint":                        base + "/token",
		"userinfo_endpoint":                     base + "/userinfo",
		"jwks_uri":                              base + "/jwks",
		"response_types_supported":              []string{"code"},
		"subject_types_supported":               []string{"public"},
		"id_token_signing_alg_values_supported": []string{"RS256", "ES256"},
		"token_endpoint_auth_methods_supported": []string{"client_secret_basic", "client_secret_post"},
		"code_challenge_methods_supported":      []string{"S256"},
	}
}

func (f *FakeIdP) discovery(w http.ResponseWriter, _ *http.Request) {
	f.mu.Lock()
	f.hits["discovery"]++
	fail := f.docFailures > 0
	if fail {
		f.docFailures--
	}
	mutate := f.docMutate
	f.mu.Unlock()
	if fail {
		http.Error(w, "unavailable", http.StatusServiceUnavailable)
		return
	}
	doc := f.Doc()
	if mutate != nil {
		mutate(doc)
	}
	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(doc)
}

func (f *FakeIdP) jwks(w http.ResponseWriter, _ *http.Request) {
	f.mu.Lock()
	f.hits["jwks"]++
	keys := make([]any, 0, len(f.published))
	for _, k := range f.published {
		keys = append(keys, k.JWK())
	}
	f.mu.Unlock()
	w.Header().Set("Content-Type", "application/json")
	w.Header().Set("Cache-Control", "public, max-age=3600")
	_ = json.NewEncoder(w).Encode(map[string]any{"keys": keys})
}

// clientCredentials returns the client id and secret of a token request and how they came.
func clientCredentials(r *http.Request) (id, secret, style string) {
	if u, p, ok := r.BasicAuth(); ok {
		id, _ = url.QueryUnescape(u)
		secret, _ = url.QueryUnescape(p)
		return id, secret, "basic"
	}
	return r.PostForm.Get("client_id"), r.PostForm.Get("client_secret"), "post"
}

func (f *FakeIdP) token(w http.ResponseWriter, r *http.Request) {
	_ = r.ParseForm()
	f.mu.Lock()
	defer f.mu.Unlock()
	f.hits["token"]++
	if f.tokenRedirect != "" {
		http.Redirect(w, r, f.tokenRedirect, http.StatusTemporaryRedirect)
		return
	}
	w.Header().Set("Content-Type", "application/json")
	fail := func(status int, code string) {
		w.WriteHeader(status)
		_ = json.NewEncoder(w).Encode(map[string]string{"error": code})
	}
	id, secret, style := clientCredentials(r)
	if id != f.ClientID || secret != f.ClientSecret || (f.clientAuth != "" && style != f.clientAuth) {
		fail(http.StatusUnauthorized, "invalid_client")
		return
	}
	g, ok := f.codes[r.PostForm.Get("code")]
	delete(f.codes, r.PostForm.Get("code"))
	sum := sha256.Sum256([]byte(r.PostForm.Get("code_verifier")))
	switch {
	case !ok || r.PostForm.Get("grant_type") != "authorization_code":
		fail(http.StatusBadRequest, "invalid_grant")
		return
	case r.PostForm.Get("redirect_uri") != g.redirect:
		fail(http.StatusBadRequest, "invalid_grant")
		return
	case base64.RawURLEncoding.EncodeToString(sum[:]) != g.challenge:
		fail(http.StatusBadRequest, "invalid_grant") // PKCE mismatch
		return
	}
	u := g.user
	now := f.clock.Now()
	claims := map[string]any{"iss": f.Issuer, "sub": u.Sub, "aud": f.ClientID, "exp": now.Add(time.Hour).Unix(),
		"iat": now.Unix(), "nonce": g.nonce}
	if !u.NoEmailInIDToken {
		if u.Email != "" {
			claims["email"] = u.Email
		}
		if u.EmailVerified != nil {
			claims["email_verified"] = u.EmailVerified
		}
	}
	if u.Name != "" {
		claims["name"] = u.Name
	}
	if u.Picture != "" {
		claims["picture"] = u.Picture
	}
	for k, v := range u.Claims {
		if v == nil {
			delete(claims, k)
		} else {
			claims[k] = v
		}
	}
	key := f.signing
	if u.Key != nil {
		key = u.Key
	}
	idToken, err := key.sign(u.Header, claims, []byte(f.ClientSecret))
	if err != nil {
		fail(http.StatusInternalServerError, "server_error")
		return
	}
	access := "idp_at_" + b64(24)
	f.access[access] = u
	f.accessTokens = append(f.accessTokens, access)
	f.idTokens = append(f.idTokens, idToken)
	_ = json.NewEncoder(w).Encode(map[string]any{"access_token": access, "token_type": "Bearer", "expires_in": 3600,
		"id_token": idToken, "scope": f.Scope})
}

func (f *FakeIdP) userinfo(w http.ResponseWriter, r *http.Request) {
	f.mu.Lock()
	f.hits["userinfo"]++
	u := f.access[strings.TrimPrefix(r.Header.Get("Authorization"), "Bearer ")]
	f.mu.Unlock()
	if u == nil {
		http.Error(w, `{"error":"invalid_token"}`, http.StatusUnauthorized)
		return
	}
	out := map[string]any{"sub": u.Sub}
	if u.UserinfoSub != "" {
		out["sub"] = u.UserinfoSub
	}
	if u.Email != "" {
		out["email"] = u.Email
	}
	if u.EmailVerified != nil {
		out["email_verified"] = u.EmailVerified
	}
	if u.Name != "" {
		out["name"] = u.Name
	}
	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(out)
}

// StartProvider opens /auth/start/{id} for ar and returns the 302 Location.
func (b *Browser) StartProvider(t testing.TB, id, ar string) string {
	t.Helper()
	res := b.Get(t, "/auth/start/"+id+"?ar="+ar)
	if res.StatusCode != http.StatusFound {
		t.Fatalf("start %s: %d %s", id, res.StatusCode, truncate(res.Body))
	}
	return res.Header.Get("Location")
}

// IdPSignIn runs the whole browser part for an existing auth request: start, the IdP's authorize
// page as u, and the callback. It returns the callback's response.
func (b *Browser) IdPSignIn(t testing.TB, f *FakeIdP, ar string, u *IdPUser) *Resp {
	t.Helper()
	return b.Get(t, f.Authorize(t, b.StartProvider(t, f.ProviderID, ar), u))
}

// IdPTokens signs in as u from a fresh browser and auth request and exchanges the code.
func (h *Harness) IdPTokens(t testing.TB, f *FakeIdP, u *IdPUser) Tokens {
	t.Helper()
	b := h.Browser()
	p := NewPKCE()
	res := b.IdPSignIn(t, f, b.Authorize(t, p), u)
	return ParseTokens(t, h.Exchange(t, LoopbackCode(t, res, p), p))
}
