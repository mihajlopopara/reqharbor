package testutil

import (
	"crypto/sha256"
	"encoding/base64"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"net/url"
	"strconv"
	"strings"
	"sync"
	"testing"

	"github.com/mihajlopopara/reqharbor/server/internal/config"
)

// FakeGitHub is a GitHub OAuth App and REST API on an httptest TLS server (06-go-server.md §10
// S5): it mints codes bound to the PKCE challenge and redirect URI of each authorize URL, checks
// the client secret and the verifier at the token endpoint, and serves /user and /user/emails
// for the token's user.
type FakeGitHub struct {
	Server       *httptest.Server
	ClientID     string
	ClientSecret string

	mu           sync.Mutex
	codes        map[string]fakeGrant
	tokens       map[string]*GitHubUser
	emailsStatus int      // when set, the status of GET /user/emails
	userAgents   []string // the User-Agent headers of the API calls
	accessTokens []string // every token issued (tests check none is logged or stored)
	n            int
}

// SetEmailsStatus makes GET /user/emails answer status (0: normal).
func (f *FakeGitHub) SetEmailsStatus(status int) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.emailsStatus = status
}

// Tokens are the upstream access tokens the fake issued.
func (f *FakeGitHub) Tokens() []string {
	f.mu.Lock()
	defer f.mu.Unlock()
	return append([]string(nil), f.accessTokens...)
}

// Agents are the User-Agent headers of the API calls so far.
func (f *FakeGitHub) Agents() []string {
	f.mu.Lock()
	defer f.mu.Unlock()
	return append([]string(nil), f.userAgents...)
}

type fakeGrant struct {
	user      *GitHubUser
	challenge string
	redirect  string
}

// GitHubUser is a fake GitHub account.
type GitHubUser struct {
	ID        int64
	Login     string
	Name      string
	AvatarURL string
	Emails    []GitHubEmail
}

// GitHubEmail is one entry of GET /user/emails.
type GitHubEmail struct {
	Email    string `json:"email"`
	Primary  bool   `json:"primary"`
	Verified bool   `json:"verified"`
}

// NewFakeGitHub starts the fake.
func NewFakeGitHub(t testing.TB) *FakeGitHub {
	t.Helper()
	f := &FakeGitHub{ClientID: "Iv1.fakeclient", ClientSecret: "fake-client-secret-" + b64(8),
		codes: map[string]fakeGrant{}, tokens: map[string]*GitHubUser{}}
	mux := http.NewServeMux()
	mux.HandleFunc("POST /login/oauth/access_token", f.token)
	mux.HandleFunc("GET /user", f.user)
	mux.HandleFunc("GET /user/emails", f.emails)
	f.Server = httptest.NewTLSServer(mux)
	t.Cleanup(f.Server.Close)
	return f
}

// Options configure a harness to use this fake.
func (f *FakeGitHub) Options() []Option {
	return []Option{
		WithEnv("REQHARBOR_GITHUB_CLIENT_ID="+f.ClientID, "REQHARBOR_GITHUB_CLIENT_SECRET="+f.ClientSecret),
		WithConfig(func(c *config.Config) {
			c.Test.GitHubAuthURL = f.Server.URL + "/login/oauth/authorize"
			c.Test.GitHubTokenURL = f.Server.URL + "/login/oauth/access_token"
			c.Test.GitHubAPIURL = f.Server.URL
		}),
		WithHTTPClient(f.Server.Client()),
	}
}

// Authorize plays GitHub's authorize page for location (the 302 of /auth/start/github): it checks
// the query, mints a code for u and returns the callback path with code and state.
func (f *FakeGitHub) Authorize(t testing.TB, location string, u *GitHubUser) string {
	t.Helper()
	q := f.checkAuthorize(t, location)
	f.mu.Lock()
	f.n++
	code := "ghcode" + strconv.Itoa(f.n) + b64(8)
	f.codes[code] = fakeGrant{user: u, challenge: q.Get("code_challenge"), redirect: q.Get("redirect_uri")}
	f.mu.Unlock()
	cb := url.Values{"code": {code}, "state": {q.Get("state")}}
	return "/auth/callback/github?" + cb.Encode()
}

// Deny plays the user clicking Cancel on GitHub's authorize page.
func (f *FakeGitHub) Deny(t testing.TB, location string) string {
	t.Helper()
	q := f.checkAuthorize(t, location)
	cb := url.Values{"error": {"access_denied"}, "error_description": {"The user has denied your application access."},
		"state": {q.Get("state")}}
	return "/auth/callback/github?" + cb.Encode()
}

func (f *FakeGitHub) checkAuthorize(t testing.TB, location string) url.Values {
	t.Helper()
	u, err := url.Parse(location)
	if err != nil || u.Scheme+"://"+u.Host != f.Server.URL || u.Path != "/login/oauth/authorize" {
		t.Fatalf("not the fake's authorize URL: %q", location)
	}
	q := u.Query()
	want := map[string]string{
		"client_id":             f.ClientID,
		"redirect_uri":          "https://" + CanonicalHost + "/auth/callback/github",
		"scope":                 "read:user user:email",
		"code_challenge_method": "S256",
		"response_type":         "code",
	}
	for k, v := range want {
		if q.Get(k) != v {
			t.Fatalf("authorize %s = %q, want %q (%s)", k, q.Get(k), v, location)
		}
	}
	if len(q.Get("code_challenge")) != 43 || q.Get("state") == "" {
		t.Fatalf("authorize without challenge or state: %s", location)
	}
	if q.Has("client_secret") || strings.Contains(location, f.ClientSecret) {
		t.Fatal("the client secret is in the authorize URL")
	}
	return q
}

func (f *FakeGitHub) token(w http.ResponseWriter, r *http.Request) {
	_ = r.ParseForm()
	w.Header().Set("Content-Type", "application/json")
	fail := func(code string) {
		_ = json.NewEncoder(w).Encode(map[string]string{"error": code, "error_description": "fake"})
	}
	f.mu.Lock()
	defer f.mu.Unlock()
	if r.PostForm.Get("client_id") != f.ClientID || r.PostForm.Get("client_secret") != f.ClientSecret {
		fail("incorrect_client_credentials")
		return
	}
	g, ok := f.codes[r.PostForm.Get("code")]
	delete(f.codes, r.PostForm.Get("code"))
	sum := sha256.Sum256([]byte(r.PostForm.Get("code_verifier")))
	switch {
	case !ok:
		fail("bad_verification_code")
		return
	case r.PostForm.Get("redirect_uri") != g.redirect:
		fail("redirect_uri_mismatch")
		return
	case base64.RawURLEncoding.EncodeToString(sum[:]) != g.challenge:
		fail("invalid_grant") // PKCE mismatch
		return
	}
	tok := "gho_" + b64(24)
	f.tokens[tok] = g.user
	f.accessTokens = append(f.accessTokens, tok)
	_ = json.NewEncoder(w).Encode(map[string]string{"access_token": tok, "token_type": "bearer", "scope": "read:user,user:email"})
}

func (f *FakeGitHub) authed(w http.ResponseWriter, r *http.Request) *GitHubUser {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.userAgents = append(f.userAgents, r.Header.Get("User-Agent"))
	u := f.tokens[strings.TrimPrefix(r.Header.Get("Authorization"), "Bearer ")]
	if u == nil || r.Header.Get("User-Agent") == "" {
		http.Error(w, `{"message":"Bad credentials"}`, http.StatusUnauthorized)
	}
	return u
}

func (f *FakeGitHub) user(w http.ResponseWriter, r *http.Request) {
	u := f.authed(w, r)
	if u == nil {
		return
	}
	var name any
	if u.Name != "" {
		name = u.Name
	}
	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(map[string]any{"id": u.ID, "login": u.Login, "name": name, "avatar_url": u.AvatarURL,
		"email": nil})
}

func (f *FakeGitHub) emails(w http.ResponseWriter, r *http.Request) {
	u := f.authed(w, r)
	if u == nil {
		return
	}
	f.mu.Lock()
	status := f.emailsStatus
	f.mu.Unlock()
	if status != 0 {
		http.Error(w, `{"message":"nope"}`, status)
		return
	}
	emails := u.Emails
	if emails == nil {
		emails = []GitHubEmail{}
	}
	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(emails)
}

// StartGitHub opens /auth/start/github for ar and returns the 302 Location.
func (b *Browser) StartGitHub(t testing.TB, ar string) string {
	t.Helper()
	res := b.Get(t, "/auth/start/github?ar="+ar)
	if res.StatusCode != http.StatusFound {
		t.Fatalf("start github: %d %s", res.StatusCode, truncate(res.Body))
	}
	return res.Header.Get("Location")
}

// GitHubSignIn runs the whole browser part for an existing auth request: start, GitHub's
// authorize page as u, and the callback. It returns the callback's response.
func (b *Browser) GitHubSignIn(t testing.TB, f *FakeGitHub, ar string, u *GitHubUser) *Resp {
	t.Helper()
	return b.Get(t, f.Authorize(t, b.StartGitHub(t, ar), u))
}

// GitHubTokens signs in as u from a fresh browser and auth request and exchanges the code.
func (h *Harness) GitHubTokens(t testing.TB, f *FakeGitHub, u *GitHubUser) Tokens {
	t.Helper()
	b := h.Browser()
	p := NewPKCE()
	res := b.GitHubSignIn(t, f, b.Authorize(t, p), u)
	return ParseTokens(t, h.Exchange(t, LoopbackCode(t, res, p), p))
}
