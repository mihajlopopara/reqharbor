package app_test

// S4 accounts core (06-go-server.md §10): PKCE, the token endpoint, redirect validation, cookies,
// CSRF, the canonical origin, CSP, sessions. Signup, bootstrap and throttle cases are in
// auth_signup_test.go and auth_throttle_test.go.

import (
	"crypto/sha256"
	"encoding/base64"
	"net/http"
	"net/url"
	"regexp"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/mihajlopopara/reqharbor/server/internal/web"

	tu "github.com/mihajlopopara/reqharbor/server/internal/testutil"
)

func TestAuth_PKCEHappyPathViaPassword(t *testing.T) {
	h := tu.New(t)
	if v := h.Call(t, "/v1/server", tu.Req{}).JSON(t); v["auth"].(map[string]any)["setupRequired"] != true {
		t.Fatalf("setupRequired before the first account: %v", v["auth"])
	}
	b := h.Browser()
	p := tu.NewPKCE()
	ar := b.Authorize(t, p)
	if res := b.Code(t, ar, h.App.SetupCode()); res.StatusCode != 303 || res.Header.Get("Location") != "/auth/signup?ar="+ar {
		t.Fatalf("setup code: %d %q", res.StatusCode, res.Header.Get("Location"))
	}
	if res := b.Get(t, "/auth/signup?ar="+ar); res.StatusCode != 200 || !strings.Contains(string(res.Body), "Create the admin account") {
		t.Fatalf("signup page %d", res.StatusCode)
	}
	res := b.Signup(t, ar, "Ada", "Ada@Example.test ", tu.DefaultPassword, "")
	code := tu.LoopbackCode(t, res, p)
	tok := h.Exchange(t, code, p)
	if tok.Header.Get("Cache-Control") != "no-store" {
		t.Fatalf("token Cache-Control %q", tok.Header.Get("Cache-Control"))
	}
	v := tok.JSON(t)
	for _, k := range []string{"access_token", "refresh_token", "token_type", "expires_in", "session_id", "user"} {
		if _, ok := v[k]; !ok {
			t.Fatalf("token response lacks %s: %s", k, tok.Body)
		}
	}
	if len(v) != 6 {
		t.Fatalf("unexpected token fields: %s", tok.Body)
	}
	tk := tu.ParseTokens(t, tok)
	if !strings.HasPrefix(tk.Access, "rha_") || len(tk.Access) != 47 || !strings.HasPrefix(tk.Refresh, "rhr_") || len(tk.Refresh) != 47 ||
		tk.TokenType != "Bearer" || tk.ExpiresIn != 3600 || tk.SessionID == "" {
		t.Fatalf("tokens %+v", tk)
	}
	tu.JSONEq(t, v["user"], map[string]any{"id": tk.User.ID, "email": "ada@example.test", "name": "Ada", "avatarUrl": nil, "isAdmin": true})
	// Only hashes are stored.
	if n := h.Count(t, "sessions", "access_hash = ? OR refresh_hash = ? OR access_hash = ?", tk.Access, tk.Refresh, tk.Refresh); n != 0 {
		t.Fatal("a raw token is stored")
	}
	if n := h.Count(t, "auth_requests", "code_hash = ?", code); n != 0 {
		t.Fatal("a raw code is stored")
	}
	// The personal space exists and setup is done.
	if h.Count(t, "workspaces w JOIN teams t ON t.id = w.team_id", "t.owner_user_id = ? AND t.personal = 1 AND w.token_hash = 'space:' || w.id", tk.User.ID) != 1 {
		t.Fatal("no personal space")
	}
	if v := h.Call(t, "/v1/server", tu.Req{}).JSON(t); v["auth"].(map[string]any)["setupRequired"] != false {
		t.Fatalf("setupRequired after the first account: %v", v["auth"])
	}
	// The loopback's landing page.
	if res := b.Get(t, "/auth/done"); res.StatusCode != 200 || !strings.Contains(string(res.Body), "You're signed in") {
		t.Fatalf("done %d", res.StatusCode)
	}
	// A password sign-in as the same user, then logout.
	again := h.SignIn(t, "ada@example.test", tu.DefaultPassword)
	if again.User.ID != tk.User.ID || again.SessionID == tk.SessionID {
		t.Fatalf("second sign-in %+v", again)
	}
	if r := h.Call(t, "/v1/auth/logout", tu.Req{Method: "POST", Token: again.Access}); r.StatusCode != 204 {
		t.Fatalf("logout %d %s", r.StatusCode, r.Body)
	}
	r := h.Call(t, "/v1/auth/logout", tu.Req{Method: "POST", Token: again.Access})
	if r.StatusCode != 401 || r.ErrorCode(t) != "auth_required" || r.Header.Get("WWW-Authenticate") != `Bearer realm="reqharbor", error="invalid_token"` {
		t.Fatalf("logout twice %d %s %v", r.StatusCode, r.Body, r.Header)
	}
	if r := h.Refresh(t, again.Refresh); r.StatusCode != 400 || r.ErrorCode(t) != "invalid_grant" {
		t.Fatalf("refresh after logout %d %s", r.StatusCode, r.Body)
	}
	// The other session still works.
	if r := h.Refresh(t, tk.Refresh); r.StatusCode != 200 {
		t.Fatalf("other session %d", r.StatusCode)
	}
}

func TestAuth_CodeExchangeFailuresBurnTheCode(t *testing.T) {
	h := tu.New(t)
	h.CreateAdmin(t, "admin@example.test")
	codeFor := func() (string, tu.PKCE) {
		b := h.Browser()
		p := tu.NewPKCE()
		ar := b.Authorize(t, p)
		return tu.LoopbackCode(t, b.Login(t, ar, "admin@example.test", tu.DefaultPassword), p), p
	}
	expectInvalid := func(name string, r *tu.Resp) {
		t.Helper()
		if r.StatusCode != 400 || r.ErrorCode(t) != "invalid_grant" {
			t.Fatalf("%s: %d %s", name, r.StatusCode, r.Body)
		}
	}
	// Wrong verifier: refused, and the code is burned for the right one too.
	code, p := codeFor()
	wrong := p
	wrong.Verifier = tu.NewPKCE().Verifier
	expectInvalid("wrong verifier", h.Exchange(t, code, wrong))
	expectInvalid("after a failed exchange", h.Exchange(t, code, p))
	// Wrong redirect_uri burns it as well.
	code, p = codeFor()
	other := p
	other.Redirect = "http://127.0.0.1:49153/cb"
	expectInvalid("wrong redirect_uri", h.Exchange(t, code, other))
	expectInvalid("after a wrong redirect_uri", h.Exchange(t, code, p))
	// Reuse.
	code, p = codeFor()
	tu.ParseTokens(t, h.Exchange(t, code, p))
	expectInvalid("reuse", h.Exchange(t, code, p))
	// Expiry: 60 s.
	code, p = codeFor()
	h.Clock.Advance(61 * time.Second)
	expectInvalid("expired", h.Exchange(t, code, p))
	code, p = codeFor()
	h.Clock.Advance(59 * time.Second)
	tu.ParseTokens(t, h.Exchange(t, code, p))
	// A wrong client_id burns it too (it is checked after the consume for codes).
	code, p = codeFor()
	expectInvalid("unknown code", h.TokenForm(t, url.Values{"grant_type": {"authorization_code"}, "client_id": {"reqharbor-desktop"},
		"code": {"x" + code}, "code_verifier": {p.Verifier}, "redirect_uri": {p.Redirect}}))
	tu.ParseTokens(t, h.Exchange(t, code, p))
}

func TestAuth_TokenEndpointFields(t *testing.T) {
	h := tu.New(t)
	tk := h.CreateAdmin(t, "admin@example.test")
	call := func(ct, body string) *tu.Resp {
		return h.Call(t, "/v1/auth/token", tu.Req{Method: "POST", Raw: tu.Str(body), Headers: map[string]string{"Content-Type": ct, "CF-Connecting-IP": tu.RandomIP()}})
	}
	// JSON with the same RFC 6749 names.
	r := call("application/json", `{"grant_type":"refresh_token","client_id":"reqharbor-desktop","refresh_token":"`+tk.Refresh+`"}`)
	next := tu.ParseTokens(t, r)
	if next.SessionID != tk.SessionID || next.Refresh == tk.Refresh {
		t.Fatalf("json refresh %+v", next)
	}
	for name, c := range map[string]struct{ ct, body, code string }{
		"client_id missing":   {"application/x-www-form-urlencoded", "grant_type=refresh_token&refresh_token=" + next.Refresh, "invalid_grant"},
		"client_id wrong":     {"application/x-www-form-urlencoded", "grant_type=refresh_token&client_id=other&refresh_token=" + next.Refresh, "invalid_grant"},
		"grant_type missing":  {"application/x-www-form-urlencoded", "client_id=reqharbor-desktop&refresh_token=" + next.Refresh, "invalid_grant"},
		"grant_type unknown":  {"application/x-www-form-urlencoded", "grant_type=password&client_id=reqharbor-desktop", "invalid_grant"},
		"camelCase names":     {"application/json", `{"grantType":"refresh_token","clientId":"reqharbor-desktop","refreshToken":"` + next.Refresh + `"}`, "invalid_grant"},
		"non-string value":    {"application/json", `{"grant_type":"refresh_token","client_id":1}`, "invalid_grant"},
		"text/plain":          {"text/plain", "grant_type=refresh_token", "unsupported_media_type"},
		"no content type":     {"", "grant_type=refresh_token", "unsupported_media_type"},
		"multipart":           {"multipart/form-data; boundary=x", "--x--", "unsupported_media_type"},
		"repeated parameter":  {"application/x-www-form-urlencoded", "grant_type=refresh_token&grant_type=refresh_token&client_id=reqharbor-desktop", "invalid_grant"},
		"code grant, no code": {"application/x-www-form-urlencoded", "grant_type=authorization_code&client_id=reqharbor-desktop", "invalid_grant"},
	} {
		r := call(c.ct, c.body)
		want := 400
		if c.code == "unsupported_media_type" {
			want = 415
		}
		if r.StatusCode != want || r.ErrorCode(t) != c.code {
			t.Errorf("%s: %d %s", name, r.StatusCode, r.Body)
		}
	}
	// The failed requests above did not consume the refresh token.
	tu.ParseTokens(t, h.Refresh(t, next.Refresh))
	// GET is 405.
	if r := h.Call(t, "/v1/auth/token", tu.Req{}); r.StatusCode != 405 || r.Header.Get("Allow") != "POST" {
		t.Fatalf("GET %d", r.StatusCode)
	}
}

func TestAuth_TokenEndpointVersionGate(t *testing.T) {
	h := tu.New(t, tu.WithEnv("REQHARBOR_MIN_CLIENT_VERSION=0.3.0"))
	h.ClientVersion = "0.3.0"
	tk := h.CreateAdmin(t, "admin@example.test")
	form := url.Values{"grant_type": {"refresh_token"}, "client_id": {"reqharbor-desktop"}, "refresh_token": {tk.Refresh}}.Encode()
	for v, want := range map[string]int{"": 426, "0.2.9": 426, "0.3.0-rc1": 426, "0.3.0": 200} {
		headers := map[string]string{"Content-Type": "application/x-www-form-urlencoded"}
		if v != "" {
			headers["X-ReqHarbor-Version"] = v
		}
		r := h.Call(t, "/v1/auth/token", tu.Req{Method: "POST", Raw: tu.Str(form), Headers: headers})
		if r.StatusCode != want || (want == 426 && r.ErrorCode(t) != "client_too_old") {
			t.Fatalf("version %q: %d %s", v, r.StatusCode, r.Body)
		}
	}
	// Legacy routes are never gated.
	h.NewWorkspace(t, "")
}

func TestAuth_RedirectURIAndParameterMatrix(t *testing.T) {
	h := tu.New(t)
	good := tu.NewPKCE()
	cases := map[string]func(q url.Values){
		"localhost":            func(q url.Values) { q.Set("redirect_uri", "http://localhost:49152/cb") },
		"[::1]":                func(q url.Values) { q.Set("redirect_uri", "http://[::1]:49152/cb") },
		"https":                func(q url.Values) { q.Set("redirect_uri", "https://127.0.0.1:49152/cb") },
		"oob":                  func(q url.Values) { q.Set("redirect_uri", "urn:ietf:wg:oauth:2.0:oob") },
		"port 80":              func(q url.Values) { q.Set("redirect_uri", "http://127.0.0.1:80/cb") },
		"port 1023":            func(q url.Values) { q.Set("redirect_uri", "http://127.0.0.1:1023/cb") },
		"port 65536":           func(q url.Values) { q.Set("redirect_uri", "http://127.0.0.1:65536/cb") },
		"no port":              func(q url.Values) { q.Set("redirect_uri", "http://127.0.0.1/cb") },
		"/cb/x":                func(q url.Values) { q.Set("redirect_uri", "http://127.0.0.1:49152/cb/x") },
		"trailing slash":       func(q url.Values) { q.Set("redirect_uri", "http://127.0.0.1:49152/cb/") },
		"query":                func(q url.Values) { q.Set("redirect_uri", "http://127.0.0.1:49152/cb?x=1") },
		"other host":           func(q url.Values) { q.Set("redirect_uri", "http://evil.example:49152/cb") },
		"client_id":            func(q url.Values) { q.Set("client_id", "other") },
		"plain method":         func(q url.Values) { q.Set("code_challenge_method", "plain") },
		"short challenge":      func(q url.Values) { q.Set("code_challenge", "abc") },
		"no state":             func(q url.Values) { q.Del("state") },
		"long state":           func(q url.Values) { q.Set("state", strings.Repeat("s", 129)) },
		"device name markup":   func(q url.Values) { q.Set("device_name", "<script>") },
		"device name too long": func(q url.Values) { q.Set("device_name", strings.Repeat("d", 65)) },
	}
	for name, mutate := range cases {
		q := good.Query()
		mutate(q)
		b := h.Browser()
		r := b.Get(t, "/auth/authorize?"+q.Encode())
		if r.StatusCode != 400 || r.Header.Get("Location") != "" || len(r.Header.Values("Set-Cookie")) != 0 ||
			!strings.Contains(string(r.Body), "This sign-in link isn") {
			t.Errorf("%s: %d %v", name, r.StatusCode, r.Header)
		}
	}
	if n := h.Count(t, "auth_requests", ""); n != 0 {
		t.Fatalf("%d auth requests created by invalid links", n)
	}
	// The valid link works, with the highest port and a unicode device name.
	q := good.Query()
	q.Set("redirect_uri", "http://127.0.0.1:65535/cb")
	q.Set("device_name", "Žana PC (Linux) 2.0")
	q.Set("method", "password")
	if r := h.Browser().Get(t, "/auth/authorize?"+q.Encode()); r.StatusCode != 200 {
		t.Fatalf("valid: %d %s", r.StatusCode, r.Body)
	}
}

func TestAuth_LoginCSRFNeedsTheCookieOfThisBrowser(t *testing.T) {
	h := tu.New(t)
	h.CreateAdmin(t, "admin@example.test")
	victim, attacker := h.Browser(), h.Browser()
	p := tu.NewPKCE()
	ar := attacker.Authorize(t, p)
	victim.Authorize(t, tu.NewPKCE()) // the victim has a cookie of its own
	for name, b := range map[string]*tu.Browser{"mismatched cookie": victim, "no cookie": h.Browser()} {
		r := b.Login(t, ar, "admin@example.test", tu.DefaultPassword)
		if r.StatusCode != 400 || !strings.Contains(string(r.Body), "This sign-in has expired") || r.Header.Get("Location") != "" {
			t.Fatalf("%s: %d %s", name, r.StatusCode, r.Body)
		}
		if r := b.Get(t, "/auth/signup?ar="+ar); r.StatusCode != 400 {
			t.Fatalf("%s signup page: %d", name, r.StatusCode)
		}
	}
	// The browser that started it signs in.
	tu.LoopbackCode(t, attacker.Login(t, ar, "admin@example.test", tu.DefaultPassword), p)
	// A finished request cannot be used again.
	if r := attacker.Login(t, ar, "admin@example.test", tu.DefaultPassword); r.StatusCode != 400 {
		t.Fatalf("finished request reused: %d", r.StatusCode)
	}
	// An expired one neither.
	p2 := tu.NewPKCE()
	ar2 := attacker.Authorize(t, p2)
	h.Clock.Advance(10*time.Minute + time.Second)
	if r := attacker.Login(t, ar2, "admin@example.test", tu.DefaultPassword); r.StatusCode != 400 {
		t.Fatalf("expired request: %d", r.StatusCode)
	}
}

func TestAuth_CrossOriginPOSTRejected(t *testing.T) {
	h := tu.New(t)
	h.CreateAdmin(t, "admin@example.test")
	b := h.Browser()
	p := tu.NewPKCE()
	ar := b.Authorize(t, p)
	form := url.Values{"ar": {ar}, "email": {"admin@example.test"}, "password": {tu.DefaultPassword}}
	for name, hd := range map[string]map[string]string{
		"Sec-Fetch-Site cross-site": {"Sec-Fetch-Site": "cross-site"},
		"Sec-Fetch-Site same-site":  {"Sec-Fetch-Site": "same-site"},
		"foreign Origin":            {"Origin": "https://evil.example"},
	} {
		r := b.PostWith(t, "/auth/password/login", form, hd)
		if r.StatusCode != 403 || r.Header.Get("Location") != "" || r.Header.Get("Content-Security-Policy") != web.AuthCSP {
			t.Fatalf("%s: %d %s", name, r.StatusCode, r.Body)
		}
	}
	if n := h.Count(t, "auth_requests", "id = ? AND password_attempts = 0", ar); n != 1 {
		t.Fatal("a cross-origin POST reached the handler")
	}
	r := b.PostWith(t, "/auth/password/login", form, map[string]string{"Sec-Fetch-Site": "same-origin", "Origin": "https://reqharbor.test"})
	tu.LoopbackCode(t, r, p)
}

func TestAuth_CanonicalOrigin(t *testing.T) {
	h := tu.New(t)
	h.CreateAdmin(t, "admin@example.test")
	p := tu.NewPKCE()
	query := p.Query().Encode()
	before := h.Count(t, "auth_requests", "")
	// The app was pointed at http://127.0.0.1:P: the browser is sent to PUBLIC_URL, query intact.
	host := strings.TrimPrefix(h.URL, "http://")
	r := h.Call(t, "/auth/authorize?"+query, tu.Req{Headers: map[string]string{"Host": host, "X-Forwarded-Proto": ""}})
	if r.StatusCode != 302 || r.Header.Get("Location") != "https://reqharbor.test/auth/authorize?"+query || len(r.Header.Values("Set-Cookie")) != 0 {
		t.Fatalf("off-origin GET: %d %q %v", r.StatusCode, r.Header.Get("Location"), r.Header.Values("Set-Cookie"))
	}
	// A known scheme that differs redirects too (http on the right host).
	r = h.Call(t, "/auth/forgot", tu.Req{Headers: map[string]string{"X-Forwarded-Proto": "http"}})
	if r.StatusCode != 302 || r.Header.Get("Location") != "https://reqharbor.test/auth/forgot" {
		t.Fatalf("http scheme: %d %q", r.StatusCode, r.Header.Get("Location"))
	}
	if h.Count(t, "auth_requests", "") != before {
		t.Fatal("an off-origin GET created an auth request")
	}
	// The flow then completes on the canonical origin.
	b := h.Browser()
	ar := b.Authorize(t, p)
	// An off-origin POST renders wrong_origin and changes nothing.
	r = b.PostWith(t, "/auth/password/login", url.Values{"ar": {ar}, "email": {"admin@example.test"}, "password": {tu.DefaultPassword}},
		map[string]string{"Host": host, "X-Forwarded-Proto": ""})
	if r.StatusCode != 403 || !strings.Contains(string(r.Body), "Open this page from https://reqharbor.test") {
		t.Fatalf("off-origin POST: %d %s", r.StatusCode, r.Body)
	}
	if n := h.Count(t, "auth_requests", "id = ? AND password_attempts = 0 AND code_hash IS NULL", ar); n != 1 {
		t.Fatal("the off-origin POST changed the auth request")
	}
	tu.LoopbackCode(t, b.Login(t, ar, "admin@example.test", tu.DefaultPassword), p)

	// A matching host with an unknown scheme (untrusted peer, so X-Forwarded-Proto is ignored) is
	// served, not redirected in a loop, and logs the warning once.
	u := tu.New(t, tu.WithEnv("REQHARBOR_TRUSTED_PROXIES=10.9.9.9/32"))
	for range 2 {
		r := u.Call(t, "/auth/forgot", tu.Req{})
		if r.StatusCode != 200 {
			t.Fatalf("unknown scheme: %d %q", r.StatusCode, r.Header.Get("Location"))
		}
	}
	if n := u.Logs.Count("requests for its host arrive over plain http from an untrusted peer"); n != 1 {
		t.Fatalf("warning logged %d times", n)
	}
}

func TestAuth_CookieOnlyUnderAuthAndV1IgnoresIt(t *testing.T) {
	h := tu.New(t)
	b := h.Browser()
	r := b.Get(t, "/auth/authorize?"+tu.NewPKCE().Query().Encode())
	cookies := r.Header.Values("Set-Cookie")
	if len(cookies) != 1 {
		t.Fatalf("Set-Cookie %v", cookies)
	}
	c := cookies[0]
	for _, want := range []string{"__Secure-rh_ar=", "Path=/auth", "Max-Age=600", "HttpOnly", "Secure", "SameSite=Lax"} {
		if !strings.Contains(c, want) {
			t.Fatalf("cookie %q lacks %s", c, want)
		}
	}
	// /v1 handlers never read cookies: a forged one authenticates nothing.
	forged := map[string]string{"Cookie": "__Secure-rh_ar=" + b.Cookies["__Secure-rh_ar"]}
	if r := h.Call(t, "/v1/auth/logout", tu.Req{Method: "POST", Headers: forged}); r.StatusCode != 401 {
		t.Fatalf("logout with a cookie: %d", r.StatusCode)
	}
	if r := h.Call(t, "/v1/server", tu.Req{Headers: forged}); r.StatusCode != 200 || len(r.Header.Values("Set-Cookie")) != 0 {
		t.Fatalf("/v1/server %d", r.StatusCode)
	}
	// An http PUBLIC_URL drops __Secure- and Secure and shows the banner.
	plain := tu.New(t, tu.WithEnv("REQHARBOR_PUBLIC_URL=http://reqharbor.test"))
	pb := plain.Browser()
	pb.Headers["X-Forwarded-Proto"] = "http"
	r = pb.Get(t, "/auth/authorize?"+tu.NewPKCE().Query().Encode())
	c = r.Header.Get("Set-Cookie")
	if r.StatusCode != 200 || !strings.HasPrefix(c, "rh_ar=") || strings.Contains(c, "Secure") ||
		!strings.Contains(string(r.Body), "This server doesn't use HTTPS. Others on your network may see your password.") {
		t.Fatalf("http server: %d %q %s", r.StatusCode, c, r.Body)
	}
	if strings.Contains(string(b.Get(t, "/auth/forgot").Body), "use HTTPS") {
		t.Fatal("banner on an https server")
	}
}

var scriptRe = regexp.MustCompile(`(?s)<script>(.*?)</script>`)
var styleRe = regexp.MustCompile(`(?s)<style>(.*?)</style>`)

func hashOf(s string) string {
	sum := sha256.Sum256([]byte(s))
	return base64.StdEncoding.EncodeToString(sum[:])
}

func TestAuth_CSPOnEveryAuthPage(t *testing.T) {
	h := tu.New(t)
	b := h.Browser()
	p := tu.NewPKCE()
	setup := b.Get(t, "/auth/authorize?"+p.Query().Encode())
	ar := tu.AR(t, setup)
	pages := map[string]*tu.Resp{
		"authorize (setup)": setup,
		"code error":        b.Code(t, ar, "WRONG-CODE"),
		"signup (setup)":    b.Get(t, "/auth/signup?ar="+ar),
		"done":              b.Get(t, "/auth/done"),
		"forgot":            b.Get(t, "/auth/forgot"),
		"forgot POST":       b.Post(t, "/auth/forgot", url.Values{}),
		"reset":             b.Get(t, "/auth/reset"),
		"reset POST":        b.Post(t, "/auth/reset", url.Values{"t": {"nope"}, "password": {"x"}}),
		"bad link":          b.Get(t, "/auth/authorize"),
		"expired request":   b.Get(t, "/auth/signup?ar=AAAAAAAAAAAAAAAAAAAAAA"),
	}
	h.CreateAdmin(t, "admin@example.test")
	ar2 := b.Authorize(t, tu.NewPKCE())
	pages["authorize"] = b.Get(t, "/auth/authorize?"+tu.NewPKCE().Query().Encode())
	pages["login error"] = b.Login(t, ar2, "nobody@example.test", "wrong password")
	pages["signup"] = b.Get(t, "/auth/signup?ar="+ar2)
	scripts := map[string]bool{hashOf(web.ResetJS): true, hashOf(web.PowJS): true}
	for name, r := range pages {
		if r.Header.Get("Content-Security-Policy") != web.AuthCSP || r.Header.Get("X-Robots-Tag") != "noindex" ||
			r.Header.Get("Cache-Control") != "no-store" || r.Header.Get("X-Frame-Options") != "DENY" ||
			!strings.HasPrefix(r.Header.Get("Content-Type"), "text/html") {
			t.Errorf("%s: headers %v", name, r.Header)
		}
		styles := styleRe.FindAllStringSubmatch(string(r.Body), -1)
		if len(styles) != 1 || styles[0][1] != web.AuthCSS {
			t.Errorf("%s: %d styles", name, len(styles))
		}
		for _, s := range scriptRe.FindAllStringSubmatch(string(r.Body), -1) {
			if !scripts[hashOf(s[1])] {
				t.Errorf("%s: unhashed script", name)
			}
		}
		if strings.Count(string(r.Body), "<script") != len(scriptRe.FindAllString(string(r.Body), -1)) {
			t.Errorf("%s: a script tag with attributes", name)
		}
	}
	for _, src := range []string{"'sha256-" + hashOf(web.AuthCSS) + "'", "'sha256-" + hashOf(web.ResetJS) + "'", "'sha256-" + hashOf(web.PowJS) + "'",
		"form-action 'self' http://127.0.0.1:*", "frame-ancestors 'none'", "default-src 'none'", "img-src data:", "base-uri 'none'"} {
		if !strings.Contains(web.AuthCSP, src) {
			t.Errorf("CSP lacks %s", src)
		}
	}
	if !strings.Contains(string(pages["reset"].Body), "<script>"+web.ResetJS+"</script>") ||
		!strings.Contains(string(pages["reset"].Body), "Enable JavaScript or ask your server admin for help.") {
		t.Error("reset page lacks reset.js or its noscript")
	}
}

func TestAuth_RefreshRotationGraceAndReuse(t *testing.T) {
	h := tu.New(t)
	t0 := h.CreateAdmin(t, "admin@example.test")
	logout := func(access string) int {
		return h.Call(t, "/v1/auth/logout", tu.Req{Method: "POST", Token: access}).StatusCode
	}
	t1 := tu.ParseTokens(t, h.Refresh(t, t0.Refresh))
	if t1.Access == t0.Access || t1.Refresh == t0.Refresh || t1.SessionID != t0.SessionID || t1.User.ID != t0.User.ID {
		t.Fatalf("rotation %+v", t1)
	}
	// Grace: the superseded token returns the identical pair for 60 s and writes nothing.
	h.Clock.Advance(59 * time.Second)
	g := tu.ParseTokens(t, h.Refresh(t, t0.Refresh))
	if g.Access != t1.Access || g.Refresh != t1.Refresh || g.SessionID != t1.SessionID {
		t.Fatalf("grace pair differs: %+v vs %+v", g, t1)
	}
	if g.ExpiresIn != 3600-59 {
		t.Fatalf("grace expires_in %d", g.ExpiresIn)
	}
	// At 61 s it is reuse: the session is revoked.
	h.Clock.Advance(2 * time.Second)
	r := h.Refresh(t, t0.Refresh)
	if r.StatusCode != 401 || r.ErrorCode(t) != "session_revoked" {
		t.Fatalf("reuse after grace: %d %s", r.StatusCode, r.Body)
	}
	if code := logout(t1.Access); code != 401 {
		t.Fatalf("the revoked session's access token still works: %d", code)
	}
	if r := h.Refresh(t, t1.Refresh); r.StatusCode != 400 {
		t.Fatalf("the revoked session's refresh token still works: %d", r.StatusCode)
	}
	// Reuse straight after rotating twice: the oldest token is unknown, not reuse.
	s := h.SignIn(t, "admin@example.test", tu.DefaultPassword)
	s1 := tu.ParseTokens(t, h.Refresh(t, s.Refresh))
	s2 := tu.ParseTokens(t, h.Refresh(t, s1.Refresh))
	if r := h.Refresh(t, s.Refresh); r.StatusCode != 400 || r.ErrorCode(t) != "invalid_grant" {
		t.Fatalf("two generations back: %d %s", r.StatusCode, r.Body)
	}
	if code := logout(s2.Access); code != 204 {
		t.Fatalf("live session: %d", code)
	}
	// The old access token stops working after a rotation.
	s3 := h.SignIn(t, "admin@example.test", tu.DefaultPassword)
	tu.ParseTokens(t, h.Refresh(t, s3.Refresh))
	if code := logout(s3.Access); code != 401 {
		t.Fatalf("old access token after rotation: %d", code)
	}
	// Access tokens expire after an hour: session_expired.
	s4 := h.SignIn(t, "admin@example.test", tu.DefaultPassword)
	h.Clock.Advance(time.Hour)
	r = h.Call(t, "/v1/auth/logout", tu.Req{Method: "POST", Token: s4.Access})
	if r.StatusCode != 401 || r.ErrorCode(t) != "session_expired" {
		t.Fatalf("expired access: %d %s", r.StatusCode, r.Body)
	}
	tu.ParseTokens(t, h.Refresh(t, s4.Refresh))
	// Refresh tokens expire after 60 idle days.
	s5 := h.SignIn(t, "admin@example.test", tu.DefaultPassword)
	h.Clock.Advance(60 * 24 * time.Hour)
	if r := h.Refresh(t, s5.Refresh); r.StatusCode != 400 {
		t.Fatalf("idle refresh: %d", r.StatusCode)
	}
}

func TestAuth_SessionCapRevokesTheOldest(t *testing.T) {
	h := tu.New(t)
	first := h.CreateAdmin(t, "admin@example.test")
	var last tu.Tokens
	for range 20 {
		h.Clock.Advance(time.Millisecond)
		last = h.SignIn(t, "admin@example.test", tu.DefaultPassword)
	}
	if r := h.Refresh(t, first.Refresh); r.StatusCode != 400 {
		t.Fatalf("21st session did not revoke the oldest: %d", r.StatusCode)
	}
	if n := h.Count(t, "sessions", "user_id = ? AND revoked_at IS NULL", first.User.ID); n != 20 {
		t.Fatalf("%d live sessions", n)
	}
	tu.ParseTokens(t, h.Refresh(t, last.Refresh))
}

func TestAuth_DisabledUserGets401(t *testing.T) {
	h := tu.New(t)
	tk := h.CreateAdmin(t, "admin@example.test")
	h.MustExec(t, "UPDATE users SET disabled_at = 1 WHERE id = ?", tk.User.ID) // sessions not revoked on purpose
	r := h.Call(t, "/v1/auth/logout", tu.Req{Method: "POST", Token: tk.Access})
	if r.StatusCode != 401 || r.ErrorCode(t) != "auth_required" {
		t.Fatalf("disabled user: %d %s", r.StatusCode, r.Body)
	}
	if r := h.Refresh(t, tk.Refresh); r.StatusCode != 400 || r.ErrorCode(t) != "invalid_grant" {
		t.Fatalf("disabled refresh: %d %s", r.StatusCode, r.Body)
	}
	b := h.Browser()
	p := tu.NewPKCE()
	ar := b.Authorize(t, p)
	if r := b.Login(t, ar, "admin@example.test", tu.DefaultPassword); r.StatusCode != 403 || !strings.Contains(string(r.Body), "This account is disabled") {
		t.Fatalf("disabled login: %d %s", r.StatusCode, r.Body)
	}
}

func TestAuth_TokensNeverInErrorsOrLogs(t *testing.T) {
	h := tu.New(t)
	b := h.Browser()
	p := tu.NewPKCE()
	ar := b.Authorize(t, p)
	setup := h.App.SetupCode()
	b.Code(t, ar, setup)
	res := b.Signup(t, ar, "Admin", "admin@example.test", tu.DefaultPassword, "")
	code := tu.LoopbackCode(t, res, p)
	tk := tu.ParseTokens(t, h.Exchange(t, code, p))
	r := h.Exchange(t, code, p) // reuse: an error that must not echo the code
	if strings.Contains(string(r.Body), code) {
		t.Fatal("error body echoes the code")
	}
	bogus := "rhr_" + strings.Repeat("A", 43)
	if r := h.Refresh(t, bogus); strings.Contains(string(r.Body), bogus) {
		t.Fatal("error body echoes the refresh token")
	}
	t2 := tu.ParseTokens(t, h.Refresh(t, tk.Refresh))
	h.Call(t, "/v1/auth/logout", tu.Req{Method: "POST", Token: t2.Access})
	h.Call(t, "/v1/auth/logout", tu.Req{Method: "POST", Token: t2.Access})
	b.Login(t, b.Authorize(t, tu.NewPKCE()), "admin@example.test", "a wrong password!")
	logs := h.Logs.String()
	for name, secret := range map[string]string{"access": tk.Access, "refresh": tk.Refresh, "rotated access": t2.Access,
		"rotated refresh": t2.Refresh, "code": code, "verifier": p.Verifier, "password": tu.DefaultPassword,
		"wrong password": "a wrong password!", "cookie": b.Cookies["__Secure-rh_ar"], "auth request": ar} {
		if secret != "" && strings.Contains(logs, secret) {
			t.Errorf("the %s appears in the logs", name)
		}
	}
	if !strings.Contains(logs, "First-run setup") || !strings.Contains(logs, `"route":"/v1/auth/token"`) {
		t.Fatal("expected log lines are missing")
	}
	if strings.Contains(logs, p.State) || strings.Contains(logs, p.Challenge) {
		t.Fatal("a query string was logged")
	}
}

func TestAuth_MethodsAreCheckedOnEveryAuthRoute(t *testing.T) {
	h := tu.New(t)
	for path, allow := range map[string]string{
		"/auth/authorize": "GET", "/auth/signup": "GET", "/auth/code": "POST", "/auth/password/login": "POST",
		"/auth/password/signup": "POST", "/auth/done": "GET", "/auth/forgot": "GET, POST", "/auth/reset": "GET, POST",
		"/v1/auth/logout": "POST",
	} {
		r := h.Call(t, path, tu.Req{Method: http.MethodDelete})
		if r.StatusCode != 405 || r.Header.Get("Allow") != allow {
			t.Errorf("%s: %d %q", path, r.StatusCode, r.Header.Get("Allow"))
		}
	}
	// Providers arrive in S5.
	if r := h.Call(t, "/auth/start/github", tu.Req{}); r.StatusCode != 404 {
		t.Errorf("/auth/start/github %d", r.StatusCode)
	}
}

// A superseded refresh token reused after the grace window while the current one rotates the
// same session: whichever commits first, the reuse answers 401 session_revoked (and the session
// is revoked) or 400 invalid_grant, never 500. The revocation runs in the transaction that found
// the reuse, so a rotation in between cannot make it miss the session.
func TestAuth_RefreshReuseRacingARotation(t *testing.T) {
	h := tu.New(t)
	h.CreateAdmin(t, "admin@example.test")
	for i := range 8 {
		s0 := h.SignIn(t, "admin@example.test", tu.DefaultPassword)
		s1 := tu.ParseTokens(t, h.Refresh(t, s0.Refresh))
		h.Clock.Advance(61 * time.Second)
		var reuse, rotate *tu.Resp
		var wg sync.WaitGroup
		start := make(chan struct{})
		wg.Add(2)
		go func() { defer wg.Done(); <-start; reuse = h.Refresh(t, s0.Refresh) }()
		go func() { defer wg.Done(); <-start; rotate = h.Refresh(t, s1.Refresh) }()
		close(start)
		wg.Wait()
		switch {
		case reuse.StatusCode == 401 && reuse.ErrorCode(t) == "session_revoked":
			// Revoked: the rotation either lost (invalid_grant) or won and its tokens are now dead.
			if rotate.StatusCode == 200 {
				if code := h.Call(t, "/v1/auth/logout", tu.Req{Method: "POST", Token: tu.ParseTokens(t, rotate).Access}).StatusCode; code != 401 {
					t.Fatalf("round %d: rotated session survived the reuse: %d", i, code)
				}
			} else if rotate.StatusCode != 400 || rotate.ErrorCode(t) != "invalid_grant" {
				t.Fatalf("round %d: rotation after revocation: %d %s", i, rotate.StatusCode, rotate.Body)
			}
		case reuse.StatusCode == 400 && reuse.ErrorCode(t) == "invalid_grant":
			// The rotation committed first: s0 is two generations back, so it is unknown.
			if rotate.StatusCode != 200 {
				t.Fatalf("round %d: rotation: %d %s", i, rotate.StatusCode, rotate.Body)
			}
		default:
			t.Fatalf("round %d: reuse answered %d %s", i, reuse.StatusCode, reuse.Body)
		}
	}
}
