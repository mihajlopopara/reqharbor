package app_test

// S5 GitHub provider (06-go-server.md §5.4, §10 S5), against a fake GitHub on an httptest TLS
// server: discovery, the start redirect (state, PKCE, scopes), primary verified email selection,
// trusted emails and REQHARBOR_ADMIN_EMAIL, returning-user rules, email_in_use, access_denied
// propagation and the callback's state and cookie checks. Dev-provider cases are in
// auth_dev_test.go (devauth builds) and auth_nodev_test.go (default builds).

import (
	"context"
	"net/http"
	"net/url"
	"strings"
	"testing"

	"github.com/mihajlopopara/reqharbor/server/internal/accounts"
	"github.com/mihajlopopara/reqharbor/server/internal/store"
	"github.com/mihajlopopara/reqharbor/server/internal/web"

	tu "github.com/mihajlopopara/reqharbor/server/internal/testutil"
)

func githubHarness(t *testing.T, env ...string) (*tu.Harness, *tu.FakeGitHub) {
	t.Helper()
	gh := tu.NewFakeGitHub(t)
	return tu.New(t, append(gh.Options(), tu.WithEnv(env...))...), gh
}

func ghUser(id int64, email string) *tu.GitHubUser {
	return &tu.GitHubUser{ID: id, Login: "octo" + email[:1], Name: "Octo " + email[:1], AvatarURL: "https://avatars.example.test/u/1",
		Emails: []tu.GitHubEmail{{Email: "other@example.test", Verified: true}, {Email: email, Primary: true, Verified: true}}}
}

type userRow struct {
	ID, Email, Via, Name string
	Trusted, Admin       bool
	Avatar               *string
}

func userByEmail(t *testing.T, h *tu.Harness, email string) *userRow {
	t.Helper()
	var u store.User
	err := h.Store.Read(context.Background(), func(tx store.ReadTx) error {
		p, err := tx.UserByEmail(email)
		if err == nil {
			u = *p
		}
		return err
	})
	if err != nil {
		t.Fatalf("user %s: %v", email, err)
	}
	return &userRow{ID: u.ID, Email: u.Email, Via: u.SignupVia, Name: u.Name, Trusted: u.EmailTrusted, Admin: u.IsAdmin, Avatar: u.AvatarURL}
}

func identityEmail(t *testing.T, h *tu.Harness, provider, subject string) string {
	t.Helper()
	var email string
	if err := h.DB.Reader.QueryRow("SELECT email FROM identities WHERE provider = ? AND subject = ?", provider, subject).Scan(&email); err != nil {
		t.Fatal(err)
	}
	return email
}

func methodIDs(t *testing.T, h *tu.Harness) []string {
	t.Helper()
	var out []string
	for _, m := range h.Call(t, "/v1/server", tu.Req{}).JSON(t)["auth"].(map[string]any)["methods"].([]any) {
		mm := m.(map[string]any)
		out = append(out, mm["id"].(string)+"="+mm["label"].(string))
	}
	return out
}

func TestGitHub_DiscoveryListsGitHubOnlyWhenConfigured(t *testing.T) {
	gh := tu.NewFakeGitHub(t)
	for _, tc := range []struct {
		name string
		opts []tu.Option
		want string
	}{
		{"not configured", nil, "password=Email and password"},
		{"configured", gh.Options(), "password=Email and password,github=GitHub"},
		{"configured, password off", append(gh.Options(), tu.WithEnv("REQHARBOR_PASSWORD_AUTH=false")), "github=GitHub"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			h := tu.New(t, tc.opts...)
			if got := strings.Join(methodIDs(t, h), ","); got != tc.want {
				t.Fatalf("methods %q, want %q", got, tc.want)
			}
			start := h.Call(t, "/auth/start/github", tu.Req{})
			if configured := len(tc.opts) > 0; configured != (start.StatusCode != 404) {
				t.Fatalf("/auth/start/github: %d", start.StatusCode)
			}
		})
	}
}

func TestGitHub_StartRedirectsWithStatePKCEAndScopes(t *testing.T) {
	h, gh := githubHarness(t, "REQHARBOR_SIGNUP=open")
	h.CreateAdmin(t, "admin@example.test")
	b := h.Browser()
	p := tu.NewPKCE()
	ar := b.Authorize(t, p)
	page := h.Browser().Get(t, "/auth/authorize?"+p.Query().Encode())
	if !strings.Contains(string(page.Body), `<a class="button" href="/auth/start/github?ar=`) || strings.Contains(string(page.Body), `action="/auth/start`) {
		t.Fatalf("the GitHub button must be a plain link: %s", page.Body)
	}
	res := b.Get(t, "/auth/start/github?ar="+ar)
	if res.StatusCode != http.StatusFound || res.Header.Get("Cache-Control") != "no-store" || len(res.Cookies()) != 0 {
		t.Fatalf("start: %d %v", res.StatusCode, res.Header)
	}
	first := res.Header.Get("Location")
	gh.Authorize(t, first, ghUser(1, "a@example.test")) // checks client_id, redirect_uri, scope, S256
	if n := h.Count(t, "auth_requests", "id = ? AND provider = 'github' AND upstream_state_hash IS NOT NULL AND upstream_verifier IS NOT NULL", ar); n != 1 {
		t.Fatalf("upstream state not stored: %d", n)
	}
	// The state is stored hashed only.
	st, _ := url.Parse(first)
	if n := h.Count(t, "auth_requests", "upstream_state_hash = ?", st.Query().Get("state")); n != 0 {
		t.Fatal("upstream state stored in clear")
	}
	// A second start replaces the round trip: the first state no longer works.
	second := b.StartGitHub(t, ar)
	if first == second {
		t.Fatal("start reused the state")
	}
	if r := b.Get(t, gh.Authorize(t, first, ghUser(1, "a@example.test"))); r.StatusCode != 400 || !strings.Contains(string(r.Body), "This sign-in has expired") {
		t.Fatalf("stale state: %d %s", r.StatusCode, r.Body)
	}
	tu.LoopbackCode(t, b.Get(t, gh.Authorize(t, second, ghUser(1, "a@example.test"))), p)

	// Without the cookie of this auth request, start is a login-CSRF reject.
	other := h.Browser()
	if r := other.Get(t, "/auth/start/github?ar="+b.Authorize(t, tu.NewPKCE())); r.StatusCode != 400 {
		t.Fatalf("foreign start: %d", r.StatusCode)
	}
	// Off origin, start redirects to the canonical origin first (§5.3), query intact.
	off := h.Call(t, "/auth/start/github?ar=x", tu.Req{Headers: map[string]string{"Host": "127.0.0.1:9", "X-Forwarded-Proto": "http"}})
	if off.StatusCode != http.StatusFound || off.Header.Get("Location") != "https://"+tu.CanonicalHost+"/auth/start/github?ar=x" {
		t.Fatalf("off origin: %d %q", off.StatusCode, off.Header.Get("Location"))
	}
}

func TestGitHub_PrimaryVerifiedEmailSelection(t *testing.T) {
	h, gh := githubHarness(t, "REQHARBOR_SIGNUP=open")
	h.CreateAdmin(t, "admin@example.test")
	for i, tc := range []struct {
		name   string
		emails []tu.GitHubEmail
		want   string // "" means no_verified_email
	}{
		{"primary verified among others", []tu.GitHubEmail{{Email: "x@example.test", Verified: true}, {Email: "Pick@Example.TEST", Primary: true, Verified: true}}, "pick@example.test"},
		{"primary unverified, another verified", []tu.GitHubEmail{{Email: "p@example.test", Primary: true}, {Email: "v@example.test", Verified: true}}, ""},
		{"only non-primary verified", []tu.GitHubEmail{{Email: "v2@example.test", Verified: true}}, ""},
		{"no emails", nil, ""},
		{"invalid primary", []tu.GitHubEmail{{Email: "not an email", Primary: true, Verified: true}}, ""},
	} {
		t.Run(tc.name, func(t *testing.T) {
			users := h.Count(t, "users", "")
			b := h.Browser()
			p := tu.NewPKCE()
			ar := b.Authorize(t, p)
			u := &tu.GitHubUser{ID: int64(100 + i), Login: "login" + tc.name[:3], Emails: tc.emails}
			res := b.GitHubSignIn(t, gh, ar, u)
			if tc.want == "" {
				if res.StatusCode != 400 || !strings.Contains(string(res.Body), "Your GitHub account has no verified email. Add one and try again.") ||
					!strings.Contains(string(res.Body), `href="/auth/start/github?ar=`+ar+`"`) {
					t.Fatalf("%d %s", res.StatusCode, res.Body)
				}
				if h.Count(t, "users", "") != users || h.Count(t, "identities", "") != users-1 {
					t.Fatal("rows were created")
				}
				return
			}
			tk := tu.ParseTokens(t, h.Exchange(t, tu.LoopbackCode(t, res, p), p))
			row := userByEmail(t, h, tc.want)
			if tk.User.Email != tc.want || !row.Trusted || row.Via != accounts.ViaOpen || row.Name != u.Login {
				t.Fatalf("%+v %+v", tk.User, row)
			}
			if me := h.Call(t, "/v1/me", tu.Req{Token: tk.Access}); me.JSON(t)["method"] != "github" {
				t.Fatalf("/v1/me method: %s", me.Body)
			}
		})
	}
}

func TestGitHub_NewAccountsFollowTheSignupRules(t *testing.T) {
	t.Run("admin email on a fresh server: trusted, admin, no setup code", func(t *testing.T) {
		h, gh := githubHarness(t, "REQHARBOR_ADMIN_EMAIL=Boss@Example.test")
		u := ghUser(7, "boss@example.test")
		tk := h.GitHubTokens(t, gh, u)
		row := userByEmail(t, h, "boss@example.test")
		if !tk.User.IsAdmin || !row.Admin || !row.Trusted || row.Via != accounts.ViaAdminEmail || row.Avatar == nil || *row.Avatar != u.AvatarURL {
			t.Fatalf("%+v %+v", tk.User, row)
		}
		if identityEmail(t, h, "github", "7") != "boss@example.test" || h.Count(t, "workspaces", "team_id IS NOT NULL") != 1 {
			t.Fatal("identity or personal space missing")
		}
		if h.Call(t, "/v1/server", tu.Req{}).JSON(t)["auth"].(map[string]any)["setupRequired"] != false {
			t.Fatal("setupRequired still true")
		}
	})
	t.Run("fresh server without admin email: setup code, then GitHub resumes", func(t *testing.T) {
		h, gh := githubHarness(t)
		b := h.Browser()
		p := tu.NewPKCE()
		ar := b.Authorize(t, p)
		u := ghUser(8, "first@example.test")
		res := b.GitHubSignIn(t, gh, ar, u)
		if res.StatusCode != 400 || !strings.Contains(string(res.Body), "Finish setting up this server") ||
			!strings.Contains(string(res.Body), `name="continue" value="github"`) || h.Count(t, "users", "") != 0 {
			t.Fatalf("%d %s", res.StatusCode, res.Body)
		}
		res = b.Post(t, "/auth/code", url.Values{"ar": {ar}, "code": {h.App.SetupCode()}, "continue": {"github"}})
		if res.StatusCode != http.StatusSeeOther || res.Header.Get("Location") != "/auth/start/github?ar="+ar {
			t.Fatalf("%d %q", res.StatusCode, res.Header.Get("Location"))
		}
		tk := tu.ParseTokens(t, h.Exchange(t, tu.LoopbackCode(t, b.GitHubSignIn(t, gh, ar, u), p), p))
		if row := userByEmail(t, h, "first@example.test"); !tk.User.IsAdmin || row.Via != accounts.ViaSetup || !row.Trusted {
			t.Fatalf("%+v", row)
		}
	})
	t.Run("invite mode: code page, invite, then GitHub resumes", func(t *testing.T) {
		h, gh := githubHarness(t)
		h.CreateAdmin(t, "admin@example.test")
		b := h.Browser()
		p := tu.NewPKCE()
		ar := b.Authorize(t, p)
		u := ghUser(9, "invited@example.test")
		res := b.GitHubSignIn(t, gh, ar, u)
		if res.StatusCode != 400 || !strings.Contains(string(res.Body), "Enter your invite code") || !strings.Contains(string(res.Body), "Enter the invite code your server admin gave you.") {
			t.Fatalf("%d %s", res.StatusCode, res.Body)
		}
		if r := b.Post(t, "/auth/code", url.Values{"ar": {ar}, "code": {"WRONG-CODE-0000-0000"}, "continue": {"github"}}); r.StatusCode != 400 ||
			!strings.Contains(string(r.Body), `name="continue" value="github"`) {
			t.Fatalf("wrong code: %d %s", r.StatusCode, r.Body)
		}
		// An unknown provider in continue is ignored.
		if r := b.Post(t, "/auth/code", url.Values{"ar": {ar}, "code": {h.Invite(t, "", 1)}, "continue": {"https://evil.example"}}); r.Header.Get("Location") != "/auth/signup?ar="+ar {
			t.Fatalf("continue not validated: %q", r.Header.Get("Location"))
		}
		tu.ParseTokens(t, h.Exchange(t, tu.LoopbackCode(t, b.GitHubSignIn(t, gh, ar, u), p), p))
		if row := userByEmail(t, h, "invited@example.test"); row.Via != accounts.ViaInvite || row.Admin || !row.Trusted {
			t.Fatalf("%+v", row)
		}
	})
	t.Run("closed and the user cap", func(t *testing.T) {
		h, gh := githubHarness(t, "REQHARBOR_SIGNUP=closed")
		h.CreateAdmin(t, "admin@example.test")
		b := h.Browser()
		if r := b.GitHubSignIn(t, gh, b.Authorize(t, tu.NewPKCE()), ghUser(10, "c@example.test")); r.StatusCode != 403 || !strings.Contains(string(r.Body), "Signup is closed") {
			t.Fatalf("%d %s", r.StatusCode, r.Body)
		}
		h2, gh2 := githubHarness(t, "REQHARBOR_SIGNUP=open", "REQHARBOR_MAX_USERS=1")
		h2.CreateAdmin(t, "admin@example.test")
		b2 := h2.Browser()
		if r := b2.GitHubSignIn(t, gh2, b2.Authorize(t, tu.NewPKCE()), ghUser(11, "d@example.test")); r.StatusCode != 403 || !strings.Contains(string(r.Body), "No new accounts") {
			t.Fatalf("%d %s", r.StatusCode, r.Body)
		}
		if h.Count(t, "users", "") != 1 || h2.Count(t, "users", "") != 1 {
			t.Fatal("rows were created")
		}
	})
}

func TestGitHub_ReturningUser(t *testing.T) {
	type step struct {
		emails    []tu.GitHubEmail
		wantEmail string // users.email after the sign-in
		wantIdent string // identities.email after the sign-in
	}
	primary := func(e string) []tu.GitHubEmail { return []tu.GitHubEmail{{Email: e, Primary: true, Verified: true}} }
	for _, tc := range []struct {
		name string
		step step
	}{
		{"trusted unused email replaces users.email", step{primary("new@example.test"), "new@example.test", "new@example.test"}},
		{"trusted email of another account changes nothing but the identity", step{primary("taken@example.test"), "gh@example.test", "taken@example.test"}},
		{"no verified email still signs in and changes nothing", step{[]tu.GitHubEmail{{Email: "unv@example.test", Primary: true}}, "gh@example.test", "gh@example.test"}},
		{"same email", step{primary("gh@example.test"), "gh@example.test", "gh@example.test"}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			h, gh := githubHarness(t, "REQHARBOR_SIGNUP=open")
			h.CreateAdmin(t, "admin@example.test")
			h.CreateUser(t, "taken@example.test")
			u := ghUser(42, "gh@example.test")
			first := h.GitHubTokens(t, gh, u)
			u.Emails, u.Name, u.AvatarURL = tc.step.emails, "Renamed", "https://avatars.example.test/u/2"
			again := h.GitHubTokens(t, gh, u)
			if again.User.ID != first.User.ID || h.Count(t, "users", "") != 3 {
				t.Fatalf("returning user resolved to another account: %+v %+v", first.User, again.User)
			}
			row := userByEmail(t, h, tc.step.wantEmail)
			if row.ID != first.User.ID || !row.Trusted || row.Name != "Renamed" || row.Avatar == nil || *row.Avatar != u.AvatarURL {
				t.Fatalf("%+v", row)
			}
			if got := identityEmail(t, h, "github", "42"); got != tc.step.wantIdent {
				t.Fatalf("identity email %q, want %q", got, tc.step.wantIdent)
			}
		})
	}

	t.Run("is_admin is never changed by a sign-in", func(t *testing.T) {
		h, gh := githubHarness(t, "REQHARBOR_ADMIN_EMAIL=boss@example.test", "REQHARBOR_SIGNUP=open")
		u := ghUser(5, "boss@example.test")
		if !h.GitHubTokens(t, gh, u).User.IsAdmin {
			t.Fatal("not admin at creation")
		}
		if !h.GitHubTokens(t, gh, u).User.IsAdmin {
			t.Fatal("admin lost on sign-in")
		}
		// user set-admin --off: no admin exists any more, and the email still equals ADMIN_EMAIL.
		h.MustExec(t, "UPDATE users SET is_admin = 0")
		if h.GitHubTokens(t, gh, u).User.IsAdmin || userByEmail(t, h, "boss@example.test").Admin {
			t.Fatal("ADMIN_EMAIL re-granted admin on sign-in")
		}
	})

	t.Run("a disabled account cannot sign in", func(t *testing.T) {
		h, gh := githubHarness(t, "REQHARBOR_SIGNUP=open")
		h.CreateAdmin(t, "admin@example.test")
		u := ghUser(6, "off@example.test")
		h.GitHubTokens(t, gh, u)
		h.MustExec(t, "UPDATE users SET disabled_at = 1 WHERE email = 'off@example.test'")
		b := h.Browser()
		ar := b.Authorize(t, tu.NewPKCE())
		r := b.GitHubSignIn(t, gh, ar, u)
		if r.StatusCode != 403 || !strings.Contains(string(r.Body), "This account is disabled. Ask your server admin.") {
			t.Fatalf("%d %s", r.StatusCode, r.Body)
		}
		if h.Count(t, "auth_requests", "id = ? AND code_hash IS NOT NULL", ar) != 0 {
			t.Fatal("a code was minted")
		}
	})
}

func TestGitHub_EmailInUse(t *testing.T) {
	for _, tc := range []struct {
		name     string
		existing func(t *testing.T, h *tu.Harness, gh *tu.FakeGitHub)
		want     []string
		absent   []string
	}{
		{
			name:     "password signup without SMTP (untrusted): method named, forgot-password hint",
			existing: func(t *testing.T, h *tu.Harness, _ *tu.FakeGitHub) { h.CreateUser(t, "jane@acme.dev") },
			want: []string{"j***@acme.dev already signs in with Email and password. Sign in with Email and password instead.",
				"If this is your address, use Forgot password to claim the account.", `href="/auth/forgot"`, "Back to sign in"},
			absent: []string{"jane@acme.dev", "Ask your server admin."},
		},
		{
			name: "CLI account (trusted): method named, no hint",
			existing: func(t *testing.T, h *tu.Harness, _ *tu.FakeGitHub) {
				err := h.Store.Write(context.Background(), func(tx store.WriteTx) error {
					_, err := accounts.Create(tx, accounts.CreateParams{Email: "jane@acme.dev", EmailTrusted: true, PHC: "$argon2id$v=19$m=8192,t=3,p=2$c2FsdA$aGFzaA",
						CLI: true, Now: h.Clock.Now()})
					return err
				})
				if err != nil {
					t.Fatal(err)
				}
			},
			want:   []string{"j***@acme.dev already signs in with Email and password."},
			absent: []string{"jane@acme.dev", "Forgot password"},
		},
		{
			name: "another GitHub account with that email",
			existing: func(t *testing.T, h *tu.Harness, gh *tu.FakeGitHub) {
				h.GitHubTokens(t, gh, ghUser(88, "jane@acme.dev"))
			},
			want:   []string{"j***@acme.dev already signs in with GitHub. Sign in with GitHub instead."},
			absent: []string{"Forgot password"},
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			h, gh := githubHarness(t, "REQHARBOR_SIGNUP=open")
			h.CreateAdmin(t, "admin@example.test")
			tc.existing(t, h, gh)
			users, idents := h.Count(t, "users", ""), h.Count(t, "identities", "")
			b := h.Browser()
			ar := b.Authorize(t, tu.NewPKCE())
			r := b.GitHubSignIn(t, gh, ar, ghUser(77, "Jane@acme.dev"))
			if r.StatusCode != http.StatusConflict || r.Header.Get("Content-Security-Policy") != web.AuthCSP {
				t.Fatalf("%d %s", r.StatusCode, r.Body)
			}
			for _, s := range tc.want {
				if !strings.Contains(string(r.Body), s) {
					t.Errorf("page lacks %q: %s", s, r.Body)
				}
			}
			for _, s := range tc.absent {
				if strings.Contains(string(r.Body), s) {
					t.Errorf("page has %q", s)
				}
			}
			if h.Count(t, "users", "") != users || h.Count(t, "identities", "") != idents || h.Count(t, "auth_requests", "id = ? AND code_hash IS NOT NULL", ar) != 0 {
				t.Fatal("email_in_use created rows or minted a code")
			}
		})
	}
}

func TestGitHub_AccessDeniedGoesBackToTheLoopback(t *testing.T) {
	h, gh := githubHarness(t, "REQHARBOR_SIGNUP=open")
	h.CreateAdmin(t, "admin@example.test")
	b := h.Browser()
	p := tu.NewPKCE()
	p.State = "st&=+/?#x" // the client state is echoed query-escaped
	ar := b.Authorize(t, p)
	res := b.Get(t, gh.Deny(t, b.StartGitHub(t, ar)))
	if res.StatusCode != http.StatusSeeOther {
		t.Fatalf("%d %s", res.StatusCode, res.Body)
	}
	loc, _ := url.Parse(res.Header.Get("Location"))
	if loc.Scheme+"://"+loc.Host+loc.Path != p.Redirect || loc.Query().Get("error") != "access_denied" ||
		loc.Query().Get("state") != p.State || loc.Query().Has("code") || len(loc.Query()) != 2 {
		t.Fatalf("Location %q", res.Header.Get("Location"))
	}
	// The auth request is finished: it cannot continue.
	if r := b.Get(t, "/auth/start/github?ar="+ar); r.StatusCode != 400 || !strings.Contains(string(r.Body), "This sign-in has expired") {
		t.Fatalf("after cancel: %d", r.StatusCode)
	}
	// Any other provider error is shown on the page and never redirects.
	ar2 := b.Authorize(t, tu.NewPKCE())
	loc2, _ := url.Parse(b.StartGitHub(t, ar2))
	r := b.Get(t, "/auth/callback/github?error=application_suspended&state="+url.QueryEscape(loc2.Query().Get("state")))
	if r.StatusCode != 400 || r.Header.Get("Location") != "" || !strings.Contains(string(r.Body), "GitHub sign-in didn&#39;t finish.") {
		t.Fatalf("%d %s", r.StatusCode, r.Body)
	}
}

func TestGitHub_CallbackChecks(t *testing.T) {
	h, gh := githubHarness(t, "REQHARBOR_SIGNUP=open")
	h.CreateAdmin(t, "admin@example.test")
	u := ghUser(3, "cb@example.test")
	expired := func(t *testing.T, r *tu.Resp) {
		t.Helper()
		if r.StatusCode != 400 || !strings.Contains(string(r.Body), "This sign-in has expired") {
			t.Fatalf("%d %s", r.StatusCode, r.Body)
		}
	}

	t.Run("cookie of another browser, then the right one", func(t *testing.T) {
		b := h.Browser()
		p := tu.NewPKCE()
		cb := gh.Authorize(t, b.StartGitHub(t, b.Authorize(t, p)), u)
		attacker := h.Browser()
		attacker.Authorize(t, tu.NewPKCE())
		expired(t, attacker.Get(t, cb))
		expired(t, h.Browser().Get(t, cb))  // no cookie at all
		tu.LoopbackCode(t, b.Get(t, cb), p) // the victim's flow was not burned
		expired(t, b.Get(t, cb))            // and the state works once
	})
	t.Run("unknown or missing state", func(t *testing.T) {
		b := h.Browser()
		b.Authorize(t, tu.NewPKCE())
		expired(t, b.Get(t, "/auth/callback/github?code=x&state=nope"))
		expired(t, b.Get(t, "/auth/callback/github?code=x"))
	})
	t.Run("token endpoint error is a provider error, logged by code only", func(t *testing.T) {
		b := h.Browser()
		loc := b.StartGitHub(t, b.Authorize(t, tu.NewPKCE()))
		q, _ := url.Parse(loc)
		r := b.Get(t, "/auth/callback/github?code=forged-code&state="+url.QueryEscape(q.Query().Get("state")))
		if r.StatusCode != http.StatusBadGateway || !strings.Contains(string(r.Body), "GitHub sign-in didn&#39;t finish.") {
			t.Fatalf("%d %s", r.StatusCode, r.Body)
		}
		if !strings.Contains(h.Logs.String(), `"reason":"token_bad_verification_code"`) || strings.Contains(h.Logs.String(), "forged-code") {
			t.Fatal("log line missing, or it carries the code")
		}
	})
	t.Run("profile API failure", func(t *testing.T) {
		gh.SetEmailsStatus(403)
		defer gh.SetEmailsStatus(0)
		b := h.Browser()
		r := b.GitHubSignIn(t, gh, b.Authorize(t, tu.NewPKCE()), ghUser(4, "api@example.test"))
		if r.StatusCode != http.StatusBadGateway || !strings.Contains(h.Logs.String(), `"reason":"api_status_403"`) {
			t.Fatalf("%d", r.StatusCode)
		}
	})
	t.Run("upstream tokens are never stored or logged; API calls send a User-Agent", func(t *testing.T) {
		h.GitHubTokens(t, gh, u)
		if len(gh.Tokens()) == 0 {
			t.Fatal("no token issued")
		}
		logs := h.Logs.String()
		for _, tok := range gh.Tokens() {
			if strings.Contains(logs, tok) {
				t.Fatal("an upstream token is in the logs")
			}
			for _, q := range []string{"SELECT COUNT(*) FROM identities WHERE email = ? OR subject = ?",
				"SELECT COUNT(*) FROM users WHERE name = ? OR avatar_url = ?",
				"SELECT COUNT(*) FROM auth_requests WHERE upstream_verifier = ? OR provider = ?"} {
				var n int
				if err := h.DB.Reader.QueryRow(q, tok, tok).Scan(&n); err != nil || n != 0 {
					t.Fatalf("%s: %d %v", q, n, err)
				}
			}
		}
		if strings.Contains(logs, gh.ClientSecret) {
			t.Fatal("the client secret is in the logs")
		}
		if h.Count(t, "auth_requests", "upstream_verifier IS NOT NULL AND code_hash IS NOT NULL") != 0 {
			t.Fatal("the upstream verifier outlived the callback")
		}
		for _, ua := range gh.Agents() {
			if ua != "reqharbor-server" {
				t.Fatalf("User-Agent %q", ua)
			}
		}
	})
}

func TestGitHub_PagesCarryTheAuthCSP(t *testing.T) {
	h, gh := githubHarness(t, "REQHARBOR_SIGNUP=open", "REQHARBOR_ADMIN_EMAIL=boss@example.test")
	b := h.Browser()
	p := tu.NewPKCE()
	p.Device = "Work laptop"
	setup := b.Get(t, "/auth/authorize?"+p.Query().Encode())
	if !strings.Contains(string(setup.Body), "Or, if this server's admin email is yours:") || !strings.Contains(string(setup.Body), "Continue with GitHub") {
		t.Fatalf("setup page without the provider: %s", setup.Body)
	}
	h.GitHubTokens(t, gh, ghUser(1, "boss@example.test"))
	ar := b.Authorize(t, p)
	q := p.Query()
	q.Set("method", "password")
	pages := map[string]*tu.Resp{
		"setup":        setup,
		"authorize":    h.Browser().Get(t, "/auth/authorize?"+p.Query().Encode()),
		"hint":         h.Browser().Get(t, "/auth/authorize?"+q.Encode()),
		"signup":       b.Get(t, "/auth/signup?ar="+ar),
		"no email":     b.GitHubSignIn(t, gh, ar, &tu.GitHubUser{ID: 2, Login: "x"}),
		"email in use": b.GitHubSignIn(t, gh, ar, ghUser(3, "boss@example.test")),
	}
	for name, r := range pages {
		if r.Header.Get("Content-Security-Policy") != web.AuthCSP || r.Header.Get("X-Robots-Tag") != "noindex" {
			t.Errorf("%s: %v", name, r.Header)
		}
		if styles := styleRe.FindAllStringSubmatch(string(r.Body), -1); len(styles) != 1 || styles[0][1] != web.AuthCSS {
			t.Errorf("%s: styles", name)
		}
	}
	if !strings.Contains(string(pages["hint"].Body), `class="button secondary" href="/auth/start/github`) {
		t.Error("method=password should make the GitHub button secondary")
	}
	if !strings.Contains(string(pages["signup"].Body), "Continue with GitHub") {
		t.Error("signup page without the provider")
	}
}

func TestGitHub_PasswordOffServerSignsUpWithGitHubOnly(t *testing.T) {
	h, gh := githubHarness(t, "REQHARBOR_PASSWORD_AUTH=false", "REQHARBOR_SIGNUP=open", "REQHARBOR_ADMIN_EMAIL=boss@example.test")
	h.GitHubTokens(t, gh, ghUser(1, "boss@example.test"))
	b := h.Browser()
	ar := b.Authorize(t, tu.NewPKCE())
	page := b.Get(t, "/auth/signup?ar="+ar)
	if page.StatusCode != 200 || strings.Contains(string(page.Body), `action="/auth/password/signup"`) || !strings.Contains(string(page.Body), "Continue with GitHub") {
		t.Fatalf("%d %s", page.StatusCode, page.Body)
	}
	login := h.Browser().Get(t, "/auth/authorize?"+tu.NewPKCE().Query().Encode())
	if strings.Contains(string(login.Body), "no sign-in method") || strings.Contains(string(login.Body), "/auth/password/login") {
		t.Fatalf("%s", login.Body)
	}
	if r := b.GitHubSignIn(t, gh, ar, ghUser(2, "two@example.test")); r.StatusCode != http.StatusSeeOther {
		t.Fatalf("%d %s", r.StatusCode, r.Body)
	}
}
