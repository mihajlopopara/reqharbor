package app_test

// S4: signup modes, invite and setup codes, REQHARBOR_MAX_USERS, REQHARBOR_ADMIN_EMAIL on the
// pages, email_in_use, CLI reset links and the one-method triggers (06-go-server.md §5.5–§5.7).

import (
	"context"
	"net/url"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/mihajlopopara/reqharbor/server/internal/accounts"
	"github.com/mihajlopopara/reqharbor/server/internal/store"

	tu "github.com/mihajlopopara/reqharbor/server/internal/testutil"
)

func TestAuth_SignupModes(t *testing.T) {
	type outcome struct {
		status int    // of the signup POST
		text   string // in the page (or "" for a 303)
		via    string // users.signup_via when created
	}
	created := func(via string) outcome { return outcome{303, "", via} }
	invalid := outcome{400, "That code isn", ""}
	closed := outcome{403, "accepting new accounts. Ask its admin.", ""}
	cases := map[string]map[string]outcome{
		"open": {
			"no code": created("open"), "invite code": created("invite"), "wrong code": invalid, "setup code": invalid,
		},
		"invite": {
			"no code": {400, "Enter the invite code", ""}, "invite code": created("invite"), "wrong code": invalid, "setup code": invalid,
		},
		"closed": {
			// Closed mode shows signup_closed whatever was typed.
			"no code": closed, "invite code": closed, "wrong code": closed, "setup code": closed,
		},
	}
	for mode, rows := range cases {
		for name, want := range rows {
			t.Run(mode+"/"+name, func(t *testing.T) {
				h := tu.New(t, tu.WithEnv("REQHARBOR_SIGNUP="+mode))
				setup := h.App.SetupCode()
				h.CreateAdmin(t, "admin@example.test")
				code := map[string]string{"no code": "", "invite code": h.Invite(t, "", 1), "wrong code": "AAAA-BBBB-CCCC-DDDD", "setup code": setup}[name]
				b := h.Browser()
				p := tu.NewPKCE()
				ar := b.Authorize(t, p)
				page := b.Get(t, "/auth/signup?ar="+ar)
				if mode == "closed" && (page.StatusCode != 403 || !strings.Contains(string(page.Body), "Signup is closed")) {
					t.Fatalf("closed signup page: %d", page.StatusCode)
				}
				if mode == "invite" && !strings.Contains(string(page.Body), `name="code"`) {
					t.Fatal("invite mode signup page has no code field")
				}
				users := h.Count(t, "users", "")
				r := b.Signup(t, ar, "Bea", "bea@example.test", tu.DefaultPassword, code)
				if r.StatusCode != want.status || (want.text != "" && !strings.Contains(string(r.Body), want.text)) {
					t.Fatalf("signup: %d %s", r.StatusCode, r.Body)
				}
				if want.via == "" {
					if h.Count(t, "users", "") != users {
						t.Fatal("a refused signup created a user")
					}
					return
				}
				tk := tu.ParseTokens(t, h.Exchange(t, tu.LoopbackCode(t, r, p), p))
				if tk.User.IsAdmin || h.Count(t, "users", "id = ? AND signup_via = ? AND email_trusted = 0", tk.User.ID, want.via) != 1 {
					t.Fatalf("user %+v not created via %s", tk.User, want.via)
				}
			})
		}
	}
}

func TestAuth_InviteCodeRules(t *testing.T) {
	h := tu.New(t)
	h.CreateAdmin(t, "admin@example.test")
	signup := func(email, code string) *tu.Resp {
		b := h.Browser()
		return b.Signup(t, b.Authorize(t, tu.NewPKCE()), "", email, tu.DefaultPassword, code)
	}
	// Single use.
	code := h.Invite(t, "", 1)
	if r := signup("one@example.test", code); r.StatusCode != 303 {
		t.Fatalf("first use %d", r.StatusCode)
	}
	if r := signup("two@example.test", code); r.StatusCode != 400 {
		t.Fatalf("second use %d", r.StatusCode)
	}
	// Restricted to an email; typed in lower case, without dashes, with look-alikes.
	code = h.Invite(t, "Three@Example.test", 2)
	if r := signup("four@example.test", code); r.StatusCode != 400 {
		t.Fatalf("other email %d", r.StatusCode)
	}
	loose := strings.ToLower(strings.ReplaceAll(code, "-", " "))
	loose = strings.NewReplacer("0", "o", "1", "l").Replace(loose)
	if r := signup("three@example.test", loose); r.StatusCode != 303 {
		t.Fatalf("restricted invite, loosely typed: %d %s", r.StatusCode, r.Body)
	}
	// Expired and revoked.
	code = h.Invite(t, "", 5)
	h.Clock.Advance(7*24*time.Hour + time.Second)
	if r := signup("five@example.test", code); r.StatusCode != 400 {
		t.Fatalf("expired %d", r.StatusCode)
	}
	code = h.Invite(t, "", 5)
	h.MustExec(t, "UPDATE one_time_tokens SET revoked_at = 1 WHERE token_hash = ?", accounts.CodeHash(code))
	if r := signup("six@example.test", code); r.StatusCode != 400 {
		t.Fatalf("revoked %d", r.StatusCode)
	}
	// POST /auth/code applies an invite before the signup form; 10 tries per auth request.
	b := h.Browser()
	ar := b.Authorize(t, tu.NewPKCE())
	for i := range 10 {
		if r := b.Code(t, ar, "WRONG"); r.StatusCode != 400 || !strings.Contains(string(r.Body), "That code isn") {
			t.Fatalf("wrong code %d: %d", i, r.StatusCode)
		}
	}
	if r := b.Code(t, ar, h.Invite(t, "", 1)); r.StatusCode != 429 || !strings.Contains(string(r.Body), "Too many attempts") {
		t.Fatalf("11th code: %d", r.StatusCode)
	}
	b = h.Browser()
	ar = b.Authorize(t, tu.NewPKCE())
	if r := b.Code(t, ar, h.Invite(t, "", 1)); r.StatusCode != 303 {
		t.Fatalf("invite via /auth/code: %d", r.StatusCode)
	}
	if r := b.Get(t, "/auth/signup?ar="+ar); strings.Contains(string(r.Body), `name="code"`) {
		t.Fatal("the signup form asks again for an applied invite")
	}
	if r := b.Signup(t, ar, "", "seven@example.test", tu.DefaultPassword, ""); r.StatusCode != 303 {
		t.Fatalf("signup after /auth/code: %d", r.StatusCode)
	}
}

func TestAuth_CodeEntryPerIPLimit(t *testing.T) {
	h := tu.New(t)
	h.CreateAdmin(t, "admin@example.test")
	b := h.Browser()
	for i := range 30 {
		if i%10 == 0 {
			b.Cookies = map[string]string{}
		}
		ar := b.Authorize(t, tu.NewPKCE())
		if r := b.Code(t, ar, "WRONG"); r.StatusCode != 400 {
			t.Fatalf("try %d: %d", i, r.StatusCode)
		}
	}
	ar := b.Authorize(t, tu.NewPKCE())
	if r := b.Code(t, ar, "WRONG"); r.StatusCode != 429 || r.Header.Get("Retry-After") == "" {
		t.Fatalf("31st try from one IP: %d", r.StatusCode)
	}
}

func TestAuth_MaxUsers(t *testing.T) {
	h := tu.New(t, tu.WithEnv("REQHARBOR_MAX_USERS=2", "REQHARBOR_SIGNUP=open"))
	h.CreateAdmin(t, "admin@example.test") // the first account is never refused
	h.CreateUser(t, "second@example.test")
	b := h.Browser()
	ar := b.Authorize(t, tu.NewPKCE())
	r := b.Signup(t, ar, "", "third@example.test", tu.DefaultPassword, "")
	if r.StatusCode != 403 || !strings.Contains(string(r.Body), "This server isn&#39;t accepting new accounts. Ask its admin.") {
		t.Fatalf("over the cap: %d %s", r.StatusCode, r.Body)
	}
	if n := h.Count(t, "users", ""); n != 2 {
		t.Fatalf("%d users", n)
	}
	// An invite does not bypass the cap, and its use is not consumed.
	code := h.Invite(t, "", 1)
	ar = b.Authorize(t, tu.NewPKCE())
	if r := b.Signup(t, ar, "", "fourth@example.test", tu.DefaultPassword, code); r.StatusCode != 403 {
		t.Fatalf("invite over the cap: %d", r.StatusCode)
	}
	if h.Count(t, "one_time_tokens", "token_hash = ? AND uses = 0", accounts.CodeHash(code)) != 1 {
		t.Fatal("a refused signup consumed the invite")
	}
}

func TestAuth_SetupCodeOnlyWhileNoUsers(t *testing.T) {
	h := tu.New(t, tu.WithEnv("REQHARBOR_SIGNUP=open"))
	setup := h.App.SetupCode()
	if len(setup) != 19 || h.Count(t, "one_time_tokens", "purpose = 'setup'") != 1 || !strings.Contains(h.Logs.String(), "enter setup code "+setup) {
		t.Fatalf("setup code %q", setup)
	}
	// Even in open mode the first account needs the setup code.
	b := h.Browser()
	p := tu.NewPKCE()
	ar := b.Authorize(t, p)
	if r := b.Get(t, "/auth/signup?ar="+ar); !strings.Contains(string(r.Body), "Finish setting up this server") {
		t.Fatalf("signup page before setup: %s", r.Body)
	}
	if r := b.Signup(t, ar, "", "first@example.test", tu.DefaultPassword, ""); r.StatusCode != 400 || h.Count(t, "users", "") != 0 {
		t.Fatalf("signup without the setup code: %d", r.StatusCode)
	}
	// An invite cannot create the first account.
	if r := b.Code(t, ar, h.Invite(t, "", 1)); r.StatusCode != 400 {
		t.Fatalf("invite before setup: %d", r.StatusCode)
	}

	// Two parallel setups with the same code produce exactly one admin.
	type flow struct {
		b  *tu.Browser
		p  tu.PKCE
		ar string
	}
	var flows []flow
	for range 2 {
		f := flow{b: h.Browser(), p: tu.NewPKCE()}
		f.ar = f.b.Authorize(t, f.p)
		if r := f.b.Code(t, f.ar, setup); r.StatusCode != 303 {
			t.Fatalf("setup code entry: %d", r.StatusCode)
		}
		flows = append(flows, f)
	}
	results := make([]*tu.Resp, len(flows))
	var wg sync.WaitGroup
	for i, f := range flows {
		wg.Go(func() {
			results[i] = f.b.Signup(t, f.ar, "", tu.RandomIP()+"@example.test", tu.DefaultPassword, "")
		})
	}
	wg.Wait()
	wins := 0
	for _, r := range results {
		if r.StatusCode == 303 {
			wins++
		}
	}
	if wins != 1 || h.Count(t, "users", "is_admin = 1 AND signup_via = 'setup'") != 1 || h.Count(t, "users", "") != 1 {
		t.Fatalf("%d setups won; users %d", wins, h.Count(t, "users", ""))
	}
	if h.Count(t, "one_time_tokens", "purpose = 'setup'") != 0 {
		t.Fatal("setup codes survive the first account")
	}
	// The code no longer works; a restart generates no new one.
	b2 := h.Browser()
	if r := b2.Code(t, b2.Authorize(t, tu.NewPKCE()), setup); r.StatusCode != 400 {
		t.Fatalf("setup code after setup: %d", r.StatusCode)
	}
	h.Restart(t)
	if h.App.SetupCode() != "" || h.Count(t, "one_time_tokens", "purpose = 'setup'") != 0 {
		t.Fatal("setup code after a restart with users")
	}
}

func TestAuth_SetupCodeRotatesAtEachStartAndFixedCode(t *testing.T) {
	h := tu.New(t)
	first := h.App.SetupCode()
	h.Restart(t)
	second := h.App.SetupCode()
	if second == "" || second == first || h.Count(t, "one_time_tokens", "purpose = 'setup'") != 1 {
		t.Fatalf("codes %q %q", first, second)
	}
	b := h.Browser()
	if r := b.Code(t, b.Authorize(t, tu.NewPKCE()), first); r.StatusCode != 400 {
		t.Fatalf("previous code: %d", r.StatusCode)
	}
	// REQHARBOR_SETUP_CODE fixes it; the log does not repeat it.
	f := tu.New(t, tu.WithEnv("REQHARBOR_SETUP_CODE=k7q4-zx2p-8m3d-1r6t"))
	if strings.Contains(f.Logs.String(), "k7q4") || !strings.Contains(f.Logs.String(), "from REQHARBOR_SETUP_CODE") {
		t.Fatal("fixed setup code logged")
	}
	fb := f.Browser()
	if r := fb.Code(t, fb.Authorize(t, tu.NewPKCE()), "K7Q4-ZX2P-8M3D-1R6T"); r.StatusCode != 303 {
		t.Fatalf("fixed code: %d", r.StatusCode)
	}
	// Setup codes expire after 24 h.
	e := tu.New(t)
	code := e.App.SetupCode()
	e.Clock.Advance(24*time.Hour + time.Second)
	eb := e.Browser()
	if r := eb.Code(t, eb.Authorize(t, tu.NewPKCE()), code); r.StatusCode != 400 {
		t.Fatalf("expired setup code: %d", r.StatusCode)
	}
}

func TestAuth_AdminEmailOnThePages(t *testing.T) {
	// A password signup without SMTP is untrusted, so the admin email never makes it admin, and
	// it does not replace the setup code for the first account either.
	h := tu.New(t, tu.WithEnv("REQHARBOR_ADMIN_EMAIL=Boss@Example.test", "REQHARBOR_SIGNUP=open"))
	b := h.Browser()
	ar := b.Authorize(t, tu.NewPKCE())
	if r := b.Signup(t, ar, "", "boss@example.test", tu.DefaultPassword, ""); r.StatusCode != 400 || h.Count(t, "users", "") != 0 {
		t.Fatalf("admin email without the setup code: %d", r.StatusCode)
	}
	admin := h.CreateAdmin(t, "owner@example.test")
	if !admin.User.IsAdmin {
		t.Fatal("setup did not make an admin")
	}
	boss := h.CreateUser(t, "boss@example.test")
	if boss.User.IsAdmin || h.Count(t, "users", "email = 'boss@example.test' AND is_admin = 0 AND email_trusted = 0") != 1 {
		t.Fatalf("untrusted admin-email signup became admin: %+v", boss.User)
	}
	h.Restart(t)
	if !strings.Contains(h.Logs.String(), "REQHARBOR_ADMIN_EMAIL is ignored: an admin already exists") {
		t.Fatal("no ADMIN_EMAIL notice once an admin exists")
	}
}

func TestAuth_AdminEmailDemotionSticks(t *testing.T) {
	h := tu.New(t, tu.WithEnv("REQHARBOR_ADMIN_EMAIL=boss@example.test"))
	phc, err := h.App.Hasher.Hash(tu.DefaultPassword)
	if err != nil {
		t.Fatal(err)
	}
	// A trusted creation (as the CLI or a provider makes) while no admin exists.
	var id string
	err = h.Store.Write(context.Background(), func(tx store.WriteTx) error {
		c, err := accounts.Create(tx, accounts.CreateParams{Email: "boss@example.test", EmailTrusted: true, PHC: phc,
			SignupMode: "invite", AdminEmail: h.Config.AdminEmail, MaxUsers: 500, Now: h.Clock.Now()})
		if err != nil {
			return err
		}
		id = c.User.ID
		if !c.User.IsAdmin || c.User.SignupVia != accounts.ViaAdminEmail {
			t.Errorf("trusted admin email: %+v", c.User)
		}
		return tx.SetUserAdmin(id, false, 1) // user set-admin --off
	})
	if err != nil {
		t.Fatal(err)
	}
	tk := h.SignIn(t, "boss@example.test", tu.DefaultPassword)
	if tk.User.ID != id || tk.User.IsAdmin || h.Count(t, "users", "id = ? AND is_admin = 0", id) != 1 {
		t.Fatalf("demoted admin is admin again after signing in: %+v", tk.User)
	}
}

func TestAuth_SignupWithExistingEmail(t *testing.T) {
	h := tu.New(t, tu.WithEnv("REQHARBOR_SIGNUP=open"))
	h.CreateAdmin(t, "admin@example.test")
	counts := func() [4]int {
		return [4]int{h.Count(t, "users", ""), h.Count(t, "teams", ""), h.Count(t, "workspaces", ""), h.Count(t, "password_credentials", "")}
	}
	before := counts()
	code := h.Invite(t, "", 1)
	b := h.Browser()
	ar := b.Authorize(t, tu.NewPKCE())
	r := b.Signup(t, ar, "", " ADMIN@example.test", tu.DefaultPassword+"!", code)
	body := string(r.Body)
	if r.StatusCode != 409 || !strings.Contains(body, "An account with this email already exists. Sign in, or ask your server admin.") {
		t.Fatalf("existing email: %d %s", r.StatusCode, body)
	}
	for _, method := range []string{"password", "Password", "GitHub", "Google"} {
		if strings.Contains(strings.SplitN(body, "<form", 2)[0], method) {
			t.Fatalf("the page names the sign-in method %q", method)
		}
	}
	if counts() != before || h.Count(t, "one_time_tokens", "token_hash = ? AND uses = 0", accounts.CodeHash(code)) != 1 {
		t.Fatal("email_in_use created rows or consumed the invite")
	}
}

func TestAuth_SignupValidation(t *testing.T) {
	h := tu.New(t, tu.WithEnv("REQHARBOR_SIGNUP=open"))
	h.CreateAdmin(t, "admin@example.test")
	for name, c := range map[string]struct{ name, email, password, want string }{
		"bad email":      {"", "not-an-email", tu.DefaultPassword, "Enter a valid email address."},
		"short password": {"", "x@example.test", "123456789", "at least 10 characters"},
		"long password":  {"", "x@example.test", strings.Repeat("p", 257), "at most 256 characters"},
		"password=email": {"", "longer.name@example.test", "Longer.Name@example.test", "can&#39;t be your email address"},
		"long name":      {strings.Repeat("n", 101), "x@example.test", tu.DefaultPassword, "at most 100 characters for your name"},
	} {
		b := h.Browser()
		r := b.Signup(t, b.Authorize(t, tu.NewPKCE()), c.name, c.email, c.password, "")
		if r.StatusCode != 400 || !strings.Contains(string(r.Body), c.want) {
			t.Errorf("%s: %d %s", name, r.StatusCode, r.Body)
		}
	}
	// 10 characters counted as runes.
	b := h.Browser()
	if r := b.Signup(t, b.Authorize(t, tu.NewPKCE()), "", "runes@example.test", "éééééééééé", ""); r.StatusCode != 303 {
		t.Fatalf("10-rune password: %d", r.StatusCode)
	}
	if h.Count(t, "users", "") != 2 {
		t.Fatal("refused signups created users")
	}
}

func TestAuth_ResetByCLILink(t *testing.T) {
	h := tu.New(t)
	tk := h.CreateAdmin(t, "admin@example.test")
	link := func() string {
		var l string
		if err := h.Store.Write(context.Background(), func(tx store.WriteTx) error {
			var err error
			l, err = accounts.NewResetLink(tx, h.Config.PublicURL, "admin@example.test", 24*time.Hour, h.Clock.Now())
			return err
		}); err != nil {
			t.Fatal(err)
		}
		return l
	}
	l := link()
	prefix := "https://reqharbor.test/auth/reset#t="
	if !strings.HasPrefix(l, prefix) {
		t.Fatalf("link %q", l)
	}
	token := strings.TrimPrefix(l, prefix)
	b := h.Browser()
	if r := b.Get(t, "/auth/reset"); r.StatusCode != 200 || !strings.Contains(string(r.Body), `id="t" value=""`) {
		t.Fatalf("reset page %d", r.StatusCode)
	}
	// Throttle rows for the email are cleared by the reset.
	other := h.Browser()
	other.Login(t, other.Authorize(t, tu.NewPKCE()), "admin@example.test", "wrong password 1")
	if h.Count(t, "login_throttle", "") != 1 {
		t.Fatal("no throttle row")
	}
	if r := b.Post(t, "/auth/reset", url.Values{"t": {token}, "password": {"short"}}); r.StatusCode != 400 || !strings.Contains(string(r.Body), "at least 10") {
		t.Fatalf("weak password: %d", r.StatusCode)
	}
	r := b.Post(t, "/auth/reset", url.Values{"t": {token}, "password": {"a brand new password"}})
	if r.StatusCode != 200 || !strings.Contains(string(r.Body), "Password changed") || !strings.Contains(string(r.Body), "Return to ReqHarbor and sign in.") {
		t.Fatalf("reset: %d %s", r.StatusCode, r.Body)
	}
	if h.Count(t, "login_throttle", "") != 0 {
		t.Fatal("reset left throttle rows")
	}
	// Every session is revoked, email_trusted is unchanged (a CLI link may go anywhere).
	if r := h.Refresh(t, tk.Refresh); r.StatusCode != 400 {
		t.Fatalf("session survived the reset: %d", r.StatusCode)
	}
	if h.Count(t, "users", "email = 'admin@example.test' AND email_trusted = 0") != 1 {
		t.Fatal("a CLI reset link changed email_trusted")
	}
	// Single use.
	if r := b.Post(t, "/auth/reset", url.Values{"t": {token}, "password": {"another new password"}}); r.StatusCode != 400 {
		t.Fatalf("reuse: %d", r.StatusCode)
	}
	// The new password works and the old one does not.
	h.SignIn(t, "admin@example.test", "a brand new password")
	ob := h.Browser()
	if r := ob.Login(t, ob.Authorize(t, tu.NewPKCE()), "admin@example.test", tu.DefaultPassword); r.StatusCode != 400 {
		t.Fatalf("old password: %d", r.StatusCode)
	}
	// Valid for 24 h.
	l = link()
	h.Clock.Advance(24*time.Hour + time.Second)
	if r := b.Post(t, "/auth/reset", url.Values{"t": {strings.TrimPrefix(l, prefix)}, "password": {"yet another password"}}); r.StatusCode != 400 {
		t.Fatalf("expired link: %d", r.StatusCode)
	}
	// /auth/forgot takes no email and says to ask the admin.
	if r := b.Post(t, "/auth/forgot", url.Values{"email": {"admin@example.test"}}); r.StatusCode != 200 || !strings.Contains(string(r.Body), "Ask your server admin for a reset link.") {
		t.Fatalf("forgot: %d", r.StatusCode)
	}
}

func TestAuth_OneMethodTriggers(t *testing.T) {
	h := tu.New(t)
	tk := h.CreateAdmin(t, "admin@example.test")
	err := h.Store.Write(context.Background(), func(tx store.WriteTx) error {
		return tx.InsertIdentity(store.Identity{Provider: "github", Subject: "1", UserID: tk.User.ID, CreatedAt: 1, LastLoginAt: 1})
	})
	if err == nil || !strings.Contains(err.Error(), "account already uses a password") {
		t.Fatalf("identity on a password account: %v", err)
	}
	var gh string
	err = h.Store.Write(context.Background(), func(tx store.WriteTx) error {
		c, err := accounts.Create(tx, accounts.CreateParams{Email: "gh@example.test", EmailTrusted: true, CLI: true,
			Identity: &store.Identity{Provider: "github", Subject: "42", Email: "gh@example.test"}, Now: h.Clock.Now()})
		if err == nil {
			gh = c.User.ID
		}
		return err
	})
	if err != nil {
		t.Fatal(err)
	}
	err = h.Store.Write(context.Background(), func(tx store.WriteTx) error { return tx.InsertPassword(gh, "$argon2id$x", 1) })
	if err == nil || !strings.Contains(err.Error(), "account already uses an external identity") {
		t.Fatalf("password on a provider account: %v", err)
	}
	// A provider-only account gets no CLI reset link, and fails a password login like an unknown email.
	err = h.Store.Write(context.Background(), func(tx store.WriteTx) error {
		_, err := accounts.NewResetLink(tx, h.Config.PublicURL, "gh@example.test", time.Hour, h.Clock.Now())
		return err
	})
	if err == nil || !strings.Contains(err.Error(), "signs in with GitHub") {
		t.Fatalf("reset link for a GitHub account: %v", err)
	}
	b := h.Browser()
	if r := b.Login(t, b.Authorize(t, tu.NewPKCE()), "gh@example.test", tu.DefaultPassword); r.StatusCode != 400 || !strings.Contains(string(r.Body), "Email or password is incorrect.") {
		t.Fatalf("password login for a GitHub account: %d", r.StatusCode)
	}
}

func TestAuth_DiscoverySetupRequiredFollowsAccountsMadeElsewhere(t *testing.T) {
	h := tu.New(t)
	if h.Call(t, "/v1/server", tu.Req{}).JSON(t)["auth"].(map[string]any)["setupRequired"] != true {
		t.Fatal("setupRequired")
	}
	// The CLI (another process) creates the first account.
	err := h.Store.Write(context.Background(), func(tx store.WriteTx) error {
		_, err := accounts.Create(tx, accounts.CreateParams{Email: "cli@example.test", EmailTrusted: true, PHC: "$argon2id$v=19$m=8192,t=3,p=2$AA$AA", CLI: true, Now: h.Clock.Now()})
		return err
	})
	if err != nil {
		t.Fatal(err)
	}
	if h.Call(t, "/v1/server", tu.Req{}).JSON(t)["auth"].(map[string]any)["setupRequired"] != false {
		t.Fatal("setupRequired after the CLI created an account")
	}
	// PASSWORD_AUTH=false lists no method.
	off := tu.New(t, tu.WithEnv("REQHARBOR_PASSWORD_AUTH=false"))
	if m := off.Call(t, "/v1/server", tu.Req{}).JSON(t)["auth"].(map[string]any)["methods"].([]any); len(m) != 0 {
		t.Fatalf("methods %v", m)
	}
}
