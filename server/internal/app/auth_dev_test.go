//go:build devauth

package app_test

// S5 dev provider (06-go-server.md §5.4 guards (3)–(5), §10 S5), in devauth builds only: the
// loopback RemoteAddr guard (forwarded headers never count), never listed, never trusted, and
// the same account resolution and signup rules as every provider.

import (
	"net/http"
	"net/http/httptest"
	"net/url"
	"strings"
	"testing"

	"github.com/mihajlopopara/reqharbor/server/internal/accounts"

	tu "github.com/mihajlopopara/reqharbor/server/internal/testutil"
)

func devHarness(t *testing.T, env ...string) *tu.Harness {
	t.Helper()
	return tu.New(t, tu.WithEnv(append([]string{"REQHARBOR_DEV_AUTH=true"}, env...)...))
}

func devPost(t *testing.T, b *tu.Browser, ar string, fields ...string) *tu.Resp {
	t.Helper()
	f := url.Values{"ar": {ar}}
	for i := 0; i+1 < len(fields); i += 2 {
		f.Set(fields[i], fields[i+1])
	}
	return b.Post(t, "/auth/dev", f)
}

// raw sends one request straight to the handler with a chosen TCP peer.
func raw(h *tu.Harness, method, target, remote string, b *tu.Browser, form url.Values) *httptest.ResponseRecorder {
	var body *strings.Reader
	if form != nil {
		body = strings.NewReader(form.Encode())
	} else {
		body = strings.NewReader("")
	}
	req := httptest.NewRequest(method, "http://"+tu.CanonicalHost+target, body)
	req.RemoteAddr = remote
	req.Header.Set("X-Forwarded-Proto", "https")
	req.Header.Set("X-Forwarded-For", "127.0.0.1")
	req.Header.Set("CF-Connecting-IP", "127.0.0.1")
	req.Header.Set("X-Real-IP", "127.0.0.1")
	if form != nil {
		req.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	}
	for k, v := range b.Cookies {
		req.AddCookie(&http.Cookie{Name: k, Value: v})
	}
	rec := httptest.NewRecorder()
	h.App.Handler().ServeHTTP(rec, req)
	return rec
}

func TestDev_RefusedFromNonLoopbackPeersWhateverTheHeaders(t *testing.T) {
	h := devHarness(t, "REQHARBOR_SIGNUP=open")
	h.CreateAdmin(t, "admin@example.test")
	b := h.Browser()
	ar := b.Authorize(t, tu.NewPKCE())
	form := url.Values{"ar": {ar}, "email": {"dev@example.test"}}
	for _, tc := range []struct {
		remote string
		ok     bool
	}{
		{"203.0.113.9:4000", false},
		{"10.0.0.1:4000", false},
		{"192.168.1.20:4000", false},
		{"[2001:db8::1]:4000", false},
		{"[::1]:4000", true},
		{"[::ffff:127.0.0.1]:4000", true},
		{"127.0.0.1:4000", true},
	} {
		start := raw(h, "GET", "/auth/start/dev?ar="+ar, tc.remote, b, nil)
		post := raw(h, "POST", "/auth/dev", tc.remote, b, url.Values{"ar": {"x"}})
		if tc.ok {
			if start.Code != 200 || !strings.Contains(start.Body.String(), `action="/auth/dev"`) || post.Code == 404 {
				t.Errorf("%s: start %d post %d", tc.remote, start.Code, post.Code)
			}
			continue
		}
		if start.Code != 404 || post.Code != 404 {
			t.Errorf("%s with loopback forwarded headers: start %d post %d", tc.remote, start.Code, post.Code)
		}
		if r := raw(h, "POST", "/auth/dev", tc.remote, b, form); r.Code != 404 {
			t.Errorf("%s: a full dev form got %d", tc.remote, r.Code)
		}
		page := raw(h, "GET", "/auth/authorize?"+tu.NewPKCE().Query().Encode(), tc.remote, h.Browser(), nil)
		if page.Code != 200 || strings.Contains(page.Body.String(), "/auth/start/dev") {
			t.Errorf("%s: the authorize page offers the dev provider (%d)", tc.remote, page.Code)
		}
	}
	if h.Count(t, "users", "email = 'dev@example.test'") != 0 {
		t.Fatal("a non-loopback peer created a dev account")
	}
}

func TestDev_SignInIsNeverListedNorTrusted(t *testing.T) {
	h := devHarness(t, "REQHARBOR_SIGNUP=open")
	h.CreateAdmin(t, "admin@example.test")
	if got := strings.Join(methodIDs(t, h), ","); got != "password=Email and password" {
		t.Fatalf("discovery lists %q", got)
	}
	b := h.Browser()
	p := tu.NewPKCE()
	ar := b.Authorize(t, p)
	page := b.Get(t, "/auth/start/dev?ar="+ar)
	if page.StatusCode != 200 || !strings.Contains(string(page.Body), "Dev sign-in") {
		t.Fatalf("%d %s", page.StatusCode, page.Body)
	}
	if login := h.Browser().Get(t, "/auth/authorize?"+tu.NewPKCE().Query().Encode()); !strings.Contains(string(login.Body), `href="/auth/start/dev?ar=`) {
		t.Fatal("the authorize page lacks the dev link for a loopback peer")
	}
	if r := devPost(t, b, ar, "email", "not an email"); r.StatusCode != 400 || !strings.Contains(string(r.Body), "Enter a valid email address.") {
		t.Fatalf("%d", r.StatusCode)
	}
	tk := tu.ParseTokens(t, h.Exchange(t, tu.LoopbackCode(t, devPost(t, b, ar, "email", "Dev@Example.test", "name", "Dev"), p), p))
	row := userByEmail(t, h, "dev@example.test")
	if tk.User.Email != "dev@example.test" || row.Trusted || row.Admin || row.Via != accounts.ViaOpen || row.Name != "Dev" {
		t.Fatalf("%+v %+v", tk.User, row)
	}
	if identityEmail(t, h, "dev", "dev@example.test") != "dev@example.test" {
		t.Fatal("identity")
	}
}

func TestDev_AdminEmailNeverAppliesToADevIdentity(t *testing.T) {
	const admin = "boss@example.test"
	t.Run("fresh server: the admin email does not bootstrap", func(t *testing.T) {
		h := devHarness(t, "REQHARBOR_ADMIN_EMAIL="+admin, "REQHARBOR_SIGNUP=open")
		b := h.Browser()
		ar := b.Authorize(t, tu.NewPKCE())
		r := devPost(t, b, ar, "email", admin)
		if r.StatusCode != 400 || !strings.Contains(string(r.Body), "This server hasn&#39;t been set up yet.") || h.Count(t, "users", "") != 0 {
			t.Fatalf("%d %s", r.StatusCode, r.Body)
		}
	})
	t.Run("no admin exists: created, but not admin and not trusted", func(t *testing.T) {
		h := devHarness(t, "REQHARBOR_ADMIN_EMAIL="+admin, "REQHARBOR_SIGNUP=open")
		h.CreateAdmin(t, "first@example.test")
		h.MustExec(t, "UPDATE users SET is_admin = 0")
		b := h.Browser()
		p := tu.NewPKCE()
		ar := b.Authorize(t, p)
		tk := tu.ParseTokens(t, h.Exchange(t, tu.LoopbackCode(t, devPost(t, b, ar, "email", admin), p), p))
		row := userByEmail(t, h, admin)
		if tk.User.IsAdmin || row.Admin || row.Trusted || row.Via != accounts.ViaOpen {
			t.Fatalf("%+v", row)
		}
	})
	t.Run("setup code through the dev form: admin through setup, still untrusted", func(t *testing.T) {
		h := devHarness(t, "REQHARBOR_ADMIN_EMAIL="+admin)
		b := h.Browser()
		p := tu.NewPKCE()
		ar := b.Authorize(t, p)
		res := devPost(t, b, ar, "email", admin, "code", h.App.SetupCode())
		tu.LoopbackCode(t, res, p)
		if row := userByEmail(t, h, admin); !row.Admin || row.Trusted || row.Via != accounts.ViaSetup {
			t.Fatalf("%+v", row)
		}
	})
}

func TestDev_SignupRulesAndReturningIdentity(t *testing.T) {
	h := devHarness(t) // invite mode
	h.CreateAdmin(t, "admin@example.test")
	b := h.Browser()
	p := tu.NewPKCE()
	ar := b.Authorize(t, p)
	if r := devPost(t, b, ar, "email", "inv@example.test"); r.StatusCode != 400 || !strings.Contains(string(r.Body), "Enter the invite code your server admin gave you.") {
		t.Fatalf("%d %s", r.StatusCode, r.Body)
	}
	if r := devPost(t, b, ar, "email", "inv@example.test", "code", "WRONG-0000-0000-0000"); r.StatusCode != 400 || !strings.Contains(string(r.Body), "That code isn&#39;t valid.") {
		t.Fatalf("%d %s", r.StatusCode, r.Body)
	}
	first := tu.ParseTokens(t, h.Exchange(t, tu.LoopbackCode(t,
		devPost(t, b, ar, "email", "inv@example.test", "name", "Inv", "subject", "dev-1", "code", h.Invite(t, "", 1)), p), p))
	if row := userByEmail(t, h, "inv@example.test"); row.Via != accounts.ViaInvite || row.Trusted {
		t.Fatalf("%+v", row)
	}

	// Returning identity (same subject) with another, untrusted email: users.email stays, the
	// identity records the new one, the name is refreshed. No invite is needed to sign in.
	b2 := h.Browser()
	p2 := tu.NewPKCE()
	ar2 := b2.Authorize(t, p2)
	again := tu.ParseTokens(t, h.Exchange(t, tu.LoopbackCode(t, devPost(t, b2, ar2, "email", "moved@example.test", "name", "Renamed", "subject", "dev-1"), p2), p2))
	row := userByEmail(t, h, "inv@example.test")
	if again.User.ID != first.User.ID || row.Name != "Renamed" || row.Trusted || identityEmail(t, h, "dev", "dev-1") != "moved@example.test" {
		t.Fatalf("%+v %+v", again.User, row)
	}
	if h.Count(t, "users", "email = 'moved@example.test'") != 0 {
		t.Fatal("an untrusted email changed users.email")
	}
}

func TestDev_EmailInUseWithAnUntrustedEmailNamesNothing(t *testing.T) {
	h := devHarness(t, "REQHARBOR_SIGNUP=open")
	h.CreateAdmin(t, "admin@example.test")
	h.CreateUser(t, "jane@acme.dev")
	users := h.Count(t, "users", "")
	b := h.Browser()
	ar := b.Authorize(t, tu.NewPKCE())
	r := devPost(t, b, ar, "email", "jane@acme.dev")
	body := string(r.Body)
	if r.StatusCode != http.StatusConflict || !strings.Contains(body, "This email is already used by another account. Ask your server admin.") {
		t.Fatalf("%d %s", r.StatusCode, body)
	}
	for _, s := range []string{"j***@acme.dev", "Email and password", "Forgot password"} {
		if strings.Contains(body, s) {
			t.Errorf("page reveals %q", s)
		}
	}
	if h.Count(t, "users", "") != users || h.Count(t, "identities", "provider = 'dev'") != 0 {
		t.Fatal("rows were created")
	}
}
