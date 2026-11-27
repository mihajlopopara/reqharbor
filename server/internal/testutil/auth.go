package testutil

import (
	"context"
	"crypto/rand"
	"crypto/sha256"
	"encoding/base64"
	"encoding/json"
	"net/http"
	"net/url"
	"regexp"
	"strings"
	"testing"
	"time"

	"github.com/mihajlopopara/reqharbor/server/internal/accounts"
	"github.com/mihajlopopara/reqharbor/server/internal/store"
)

// Sign-in helpers: a Browser plays the hosted pages (cookie jar, client IP), PKCE is the app's
// side of the loopback flow (06-go-server.md §5.2).

// PKCE is one app sign-in: verifier, S256 challenge, state and loopback redirect URI.
type PKCE struct {
	Verifier, Challenge, State, Redirect, Device string
}

func b64(n int) string {
	b := make([]byte, n)
	_, _ = rand.Read(b)
	return base64.RawURLEncoding.EncodeToString(b)
}

// NewPKCE returns fresh sign-in parameters on loopback port 49152.
func NewPKCE() PKCE {
	v := b64(32)
	sum := sha256.Sum256([]byte(v))
	return PKCE{Verifier: v, Challenge: base64.RawURLEncoding.EncodeToString(sum[:]), State: b64(16),
		Redirect: "http://127.0.0.1:49152/cb", Device: "MacBook Pro (macOS)"}
}

// Query is the authorize query string.
func (p PKCE) Query() url.Values {
	q := url.Values{}
	q.Set("client_id", "reqharbor-desktop")
	q.Set("redirect_uri", p.Redirect)
	q.Set("code_challenge", p.Challenge)
	q.Set("code_challenge_method", "S256")
	q.Set("state", p.State)
	q.Set("device_name", p.Device)
	return q
}

// Browser is a cookie jar and a client IP on the canonical origin.
type Browser struct {
	h       *Harness
	Cookies map[string]string
	IP      string
	Headers map[string]string
}

// Clone returns a copy with its own cookie jar and headers, so copies can send requests
// concurrently (a Browser itself is not safe for concurrent use).
func (b *Browser) Clone() *Browser {
	c := &Browser{h: b.h, Cookies: map[string]string{}, IP: b.IP, Headers: map[string]string{}}
	for k, v := range b.Cookies {
		c.Cookies[k] = v
	}
	for k, v := range b.Headers {
		c.Headers[k] = v
	}
	return c
}

// Browser returns a browser from a random client IP.
func (h *Harness) Browser() *Browser {
	return &Browser{h: h, Cookies: map[string]string{}, IP: RandomIP(), Headers: map[string]string{}}
}

func (b *Browser) do(t testing.TB, method, path string, form url.Values, extra map[string]string) *Resp {
	t.Helper()
	headers := map[string]string{"CF-Connecting-IP": b.IP}
	for k, v := range b.Headers {
		headers[k] = v
	}
	for k, v := range extra {
		headers[k] = v
	}
	if len(b.Cookies) > 0 {
		var parts []string
		for k, v := range b.Cookies {
			parts = append(parts, k+"="+v)
		}
		headers["Cookie"] = strings.Join(parts, "; ")
	}
	req := Req{Method: method, Headers: headers}
	if form != nil {
		req.Raw = Str(form.Encode())
		headers["Content-Type"] = "application/x-www-form-urlencoded"
	}
	res := b.h.Call(t, path, req)
	for _, c := range res.Cookies() {
		if c.MaxAge < 0 {
			delete(b.Cookies, c.Name)
		} else {
			b.Cookies[c.Name] = c.Value
		}
	}
	return res
}

// Get loads a page.
func (b *Browser) Get(t testing.TB, path string) *Resp {
	t.Helper()
	return b.do(t, "GET", path, nil, nil)
}

// Post submits a form.
func (b *Browser) Post(t testing.TB, path string, form url.Values) *Resp {
	t.Helper()
	return b.do(t, "POST", path, form, nil)
}

// PostWith submits a form with extra headers.
func (b *Browser) PostWith(t testing.TB, path string, form url.Values, headers map[string]string) *Resp {
	t.Helper()
	return b.do(t, "POST", path, form, headers)
}

var arRe = regexp.MustCompile(`name="ar" value="([A-Za-z0-9_-]{22})"`)

// AR returns the auth request id in a page.
func AR(t testing.TB, res *Resp) string {
	t.Helper()
	m := arRe.FindSubmatch(res.Body)
	if m == nil {
		t.Fatalf("no auth request in page %d: %s", res.StatusCode, truncate(res.Body))
	}
	return string(m[1])
}

// Authorize opens the sign-in page and returns its auth request id.
func (b *Browser) Authorize(t testing.TB, p PKCE) string {
	t.Helper()
	res := b.Get(t, "/auth/authorize?"+p.Query().Encode())
	if res.StatusCode != 200 {
		t.Fatalf("authorize: %d %s", res.StatusCode, truncate(res.Body))
	}
	return AR(t, res)
}

// Code posts an invite or setup code.
func (b *Browser) Code(t testing.TB, ar, code string) *Resp {
	t.Helper()
	return b.Post(t, "/auth/code", url.Values{"ar": {ar}, "code": {code}})
}

// Signup posts the signup form (code may be "").
func (b *Browser) Signup(t testing.TB, ar, name, email, password, code string) *Resp {
	t.Helper()
	f := url.Values{"ar": {ar}, "name": {name}, "email": {email}, "password": {password}}
	if code != "" {
		f.Set("code", code)
	}
	return b.Post(t, "/auth/password/signup", f)
}

// Login posts the password form.
func (b *Browser) Login(t testing.TB, ar, email, password string) *Resp {
	t.Helper()
	return b.Post(t, "/auth/password/login", url.Values{"ar": {ar}, "email": {email}, "password": {password}})
}

// LoopbackCode checks that res is the 303 to the app's loopback URI with the right state and
// returns the authorization code.
func LoopbackCode(t testing.TB, res *Resp, p PKCE) string {
	t.Helper()
	if res.StatusCode != http.StatusSeeOther {
		t.Fatalf("want 303 to the loopback, got %d: %s", res.StatusCode, truncate(res.Body))
	}
	loc, err := url.Parse(res.Header.Get("Location"))
	if err != nil || loc.Scheme+"://"+loc.Host+loc.Path != p.Redirect {
		t.Fatalf("Location %q is not %s", res.Header.Get("Location"), p.Redirect)
	}
	if loc.Query().Get("state") != p.State {
		t.Fatalf("state %q, want %q", loc.Query().Get("state"), p.State)
	}
	code := loc.Query().Get("code")
	if code == "" {
		t.Fatalf("no code in %s", loc)
	}
	return code
}

// Tokens is a token-endpoint response.
type Tokens struct {
	Access    string `json:"access_token"`
	Refresh   string `json:"refresh_token"`
	TokenType string `json:"token_type"`
	ExpiresIn int64  `json:"expires_in"`
	SessionID string `json:"session_id"`
	User      struct {
		ID      string `json:"id"`
		Email   string `json:"email"`
		Name    string `json:"name"`
		IsAdmin bool   `json:"isAdmin"`
	} `json:"user"`
}

// TokenForm posts a form-encoded token request.
func (h *Harness) TokenForm(t testing.TB, form url.Values) *Resp {
	t.Helper()
	headers := map[string]string{"Content-Type": "application/x-www-form-urlencoded", "CF-Connecting-IP": RandomIP()}
	if h.ClientVersion != "" {
		headers["X-ReqHarbor-Version"] = h.ClientVersion
	}
	return h.Call(t, "/v1/auth/token", Req{Method: "POST", Raw: Str(form.Encode()), Headers: headers})
}

// Exchange trades a code for tokens.
func (h *Harness) Exchange(t testing.TB, code string, p PKCE) *Resp {
	t.Helper()
	return h.TokenForm(t, url.Values{"grant_type": {"authorization_code"}, "client_id": {"reqharbor-desktop"},
		"code": {code}, "code_verifier": {p.Verifier}, "redirect_uri": {p.Redirect}})
}

// Refresh rotates a refresh token.
func (h *Harness) Refresh(t testing.TB, refresh string) *Resp {
	t.Helper()
	return h.TokenForm(t, url.Values{"grant_type": {"refresh_token"}, "client_id": {"reqharbor-desktop"}, "refresh_token": {refresh}})
}

// ParseTokens decodes a 200 token response.
func ParseTokens(t testing.TB, res *Resp) Tokens {
	t.Helper()
	if res.StatusCode != 200 {
		t.Fatalf("token: %d %s", res.StatusCode, truncate(res.Body))
	}
	var tk Tokens
	if err := json.Unmarshal(res.Body, &tk); err != nil {
		t.Fatal(err)
	}
	return tk
}

// DefaultPassword is the password the sign-in helpers use.
const DefaultPassword = "correct horse battery"

// CreateAdmin creates the first account with the setup code, through the pages, and signs it in.
func (h *Harness) CreateAdmin(t testing.TB, email string) Tokens {
	t.Helper()
	code := h.App.SetupCode()
	if code == "" {
		t.Fatal("no setup code: the server already has accounts")
	}
	b := h.Browser()
	p := NewPKCE()
	ar := b.Authorize(t, p)
	if res := b.Code(t, ar, code); res.StatusCode != http.StatusSeeOther {
		t.Fatalf("setup code: %d %s", res.StatusCode, truncate(res.Body))
	}
	res := b.Signup(t, ar, "Admin", email, DefaultPassword, "")
	return ParseTokens(t, h.Exchange(t, LoopbackCode(t, res, p), p))
}

// Invite creates a server invite code directly in the store.
func (h *Harness) Invite(t testing.TB, email string, uses int64) string {
	t.Helper()
	var inv *accounts.Invite
	err := h.Store.Write(context.Background(), func(tx store.WriteTx) error {
		var err error
		inv, err = accounts.NewInvite(tx, email, uses, 7*24*time.Hour, h.Clock.Now())
		return err
	})
	if err != nil {
		t.Fatal(err)
	}
	return inv.Code
}

// CreateUser signs up email with an invite code (creating the admin first when the server has no
// account) and returns its tokens.
func (h *Harness) CreateUser(t testing.TB, email string) Tokens {
	t.Helper()
	if h.App.SetupCode() != "" && !h.hasUsers(t) {
		h.CreateAdmin(t, "admin-"+b64(4)+"@example.test")
	}
	b := h.Browser()
	p := NewPKCE()
	ar := b.Authorize(t, p)
	res := b.Signup(t, ar, "User", email, DefaultPassword, h.Invite(t, "", 1))
	return ParseTokens(t, h.Exchange(t, LoopbackCode(t, res, p), p))
}

// SignIn signs in with a password through the pages.
func (h *Harness) SignIn(t testing.TB, email, password string) Tokens {
	t.Helper()
	b := h.Browser()
	p := NewPKCE()
	ar := b.Authorize(t, p)
	return ParseTokens(t, h.Exchange(t, LoopbackCode(t, b.Login(t, ar, email, password), p), p))
}

func (h *Harness) hasUsers(t testing.TB) bool {
	t.Helper()
	var n int64
	if err := h.Store.Read(context.Background(), func(tx store.ReadTx) error {
		var err error
		n, err = tx.UserCount()
		return err
	}); err != nil {
		t.Fatal(err)
	}
	return n > 0
}

// Count runs SELECT COUNT(*) FROM table [WHERE where] on the harness database.
func (h *Harness) Count(t testing.TB, table, where string, args ...any) int {
	t.Helper()
	q := "SELECT COUNT(*) FROM " + table
	if where != "" {
		q += " WHERE " + where
	}
	var n int
	if err := h.DB.Reader.QueryRow(q, args...).Scan(&n); err != nil {
		t.Fatalf("%s: %v", q, err)
	}
	return n
}

// MustExec runs a statement on the writer (test setup).
func (h *Harness) MustExec(t testing.TB, q string, args ...any) {
	t.Helper()
	if _, err := h.DB.Writer.Exec(q, args...); err != nil {
		t.Fatalf("%s: %v", q, err)
	}
}
