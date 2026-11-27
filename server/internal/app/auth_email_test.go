package app_test

// S9: SMTP. Signup codes (5 attempts, 3 sends per auth request), signup with an address that
// already has an account, reset by email (trusted, untrusted and provider-only accounts), the
// forgot page, the email caps and REQHARBOR_ADMIN_EMAIL with a confirmed address
// (06-go-server.md §5.5–§5.7, §5.9, §10 S9).

import (
	"context"
	"net/http"
	"net/url"
	"regexp"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"github.com/mihajlopopara/reqharbor/server/internal/accounts"
	"github.com/mihajlopopara/reqharbor/server/internal/ids"
	"github.com/mihajlopopara/reqharbor/server/internal/store"
	"github.com/mihajlopopara/reqharbor/server/internal/web"

	tu "github.com/mihajlopopara/reqharbor/server/internal/testutil"
)

const smtpPassword = "smtp-secret-pw"

func emailHarness(t *testing.T, env ...string) *tu.Harness {
	t.Helper()
	s := tu.NewSMTPServer(t, tu.SMTPOptions{Username: "apikey", Password: smtpPassword})
	return tu.New(t, tu.WithSMTP(s), tu.WithEnv(env...))
}

func body(r *tu.Resp) string { return string(r.Body) }

// githubUser creates a provider-only (GitHub) account directly, as a GitHub sign-in would.
func githubUser(t *testing.T, h *tu.Harness, email string) string {
	t.Helper()
	var id string
	err := h.Store.Write(context.Background(), func(tx store.WriteTx) error {
		c, err := accounts.Create(tx, accounts.CreateParams{Email: email, EmailTrusted: true, CLI: true,
			Identity: &store.Identity{Provider: "github", Subject: "gh-" + email, Email: email}, Now: h.Clock.Now()})
		if err == nil {
			id = c.User.ID
		}
		return err
	})
	if err != nil {
		t.Fatal(err)
	}
	return id
}

var arIDInPageRe = regexp.MustCompile(`[A-Za-z0-9_-]{22}`)

// samePage normalises the parts of a page that differ by design (auth request id, the address the
// visitor typed) so two pages can be compared byte for byte.
func samePage(body, email string) string {
	body = resumeLinkRe.ReplaceAllString(body, `href="/auth/authorize?RESUME"`) // the app's own PKCE values
	return arIDInPageRe.ReplaceAllString(strings.ReplaceAll(body, email, "EMAIL"), "AR")
}

var resumeLinkRe = regexp.MustCompile(`href="/auth/authorize\?[^"]*"`)

func TestEmail_Discovery(t *testing.T) {
	h := emailHarness(t)
	a := h.Call(t, "/v1/server", tu.Req{}).JSON(t)["auth"].(map[string]any)
	if a["passwordReset"] != "email" || a["emailVerification"] != true {
		t.Fatalf("with SMTP: %v", a)
	}
	off := emailHarness(t, "REQHARBOR_PASSWORD_AUTH=false")
	if a := off.Call(t, "/v1/server", tu.Req{}).JSON(t)["auth"].(map[string]any); a["emailVerification"] != false {
		t.Fatalf("password auth off: %v", a)
	}
	plain := tu.New(t)
	a = plain.Call(t, "/v1/server", tu.Req{}).JSON(t)["auth"].(map[string]any)
	if a["passwordReset"] != "admin" || a["emailVerification"] != false {
		t.Fatalf("without SMTP: %v", a)
	}
}

func TestEmail_SignupCode(t *testing.T) {
	h := emailHarness(t, "REQHARBOR_SIGNUP=open")
	h.CreateAdmin(t, "admin@example.test")
	b := h.Browser()
	p := tu.NewPKCE()
	ar := b.Authorize(t, p)
	users := h.Count(t, "users", "")
	r := b.Signup(t, ar, "Bea", "Bea@Example.test", tu.DefaultPassword, "")
	if r.StatusCode != 200 || !strings.Contains(body(r), "Check your email for a 6-digit code") || !strings.Contains(body(r), "bea@example.test") {
		t.Fatalf("signup: %d %s", r.StatusCode, r.Body)
	}
	if h.Count(t, "users", "") != users || h.Count(t, "auth_requests", "id = ? AND pending_email = 'bea@example.test' AND pending_phc LIKE '$argon2id$%' AND email_codes_sent = 1", ar) != 1 {
		t.Fatal("signup with SMTP created the account early or stored no pending signup")
	}
	code := h.LatestCode(t, "bea@example.test")
	m := h.SMTP.To("bea@example.test")[0]
	if !regexp.MustCompile(`^\d{6}$`).MatchString(code) || !strings.Contains(m.Subject(), code) || !strings.Contains(m.Subject(), "reqharbor.test") ||
		!strings.Contains(m.Body, "expires in 10 minutes") || !m.TLS || m.Auth != "apikey" || m.From != "rh@acme.test" {
		t.Fatalf("code email %+v", m)
	}
	wrong := "000000"
	if code == wrong {
		wrong = "111111"
	}
	if r := b.Verify(t, ar, wrong); r.StatusCode != 400 || !strings.Contains(body(r), "That code isn&#39;t right.") {
		t.Fatalf("wrong code: %d %s", r.StatusCode, r.Body)
	}
	// Typed with a space is fine.
	r = b.Verify(t, ar, code[:3]+" "+code[3:])
	tk := tu.ParseTokens(t, h.Exchange(t, tu.LoopbackCode(t, r, p), p))
	if tk.User.Email != "bea@example.test" || tk.User.IsAdmin ||
		h.Count(t, "users", "id = ? AND email_trusted = 1 AND signup_via = 'open' AND name = 'Bea'", tk.User.ID) != 1 {
		t.Fatalf("created %+v", tk.User)
	}
	if h.Count(t, "auth_requests", "id = ? AND pending_phc IS NULL AND email_code_hash IS NULL", ar) != 1 {
		t.Fatal("the pending signup was not cleared")
	}
	// The password works.
	h.SignIn(t, "bea@example.test", tu.DefaultPassword)
	logs := h.Logs.String()
	if strings.Contains(logs, "bea@example.test") || strings.Contains(logs, smtpPassword) || strings.Contains(logs, code) {
		t.Fatal("logs carry the address, the SMTP password or the code")
	}
	if !strings.Contains(logs, `"msg":"email_sent","purpose":"signup_code"`) {
		t.Fatalf("no email_sent log: %s", logs)
	}
}

func TestEmail_FiveAttemptsPerCode(t *testing.T) {
	h := emailHarness(t, "REQHARBOR_SIGNUP=open")
	h.CreateAdmin(t, "admin@example.test")
	b := h.Browser()
	p := tu.NewPKCE()
	ar := b.Authorize(t, p)
	b.Signup(t, ar, "", "five@example.test", tu.DefaultPassword, "")
	code := h.LatestCode(t, "five@example.test")
	wrong := "000000"
	if code == wrong {
		wrong = "999999"
	}
	for i := 0; i < 5; i++ {
		if r := b.Verify(t, ar, wrong); r.StatusCode != 400 || !strings.Contains(body(r), "That code isn&#39;t right.") {
			t.Fatalf("attempt %d: %d", i+1, r.StatusCode)
		}
	}
	// The sixth try fails even with the right code, and burns it.
	if r := b.Verify(t, ar, code); r.StatusCode != 429 || !strings.Contains(body(r), "Too many wrong codes. Send a new code.") {
		t.Fatalf("sixth try: %d %s", r.StatusCode, r.Body)
	}
	if r := b.Verify(t, ar, code); r.StatusCode != 400 || !strings.Contains(body(r), "Send a new code to continue.") {
		t.Fatalf("burned code: %d", r.StatusCode)
	}
	if r := b.Resend(t, ar); r.StatusCode != 200 || !strings.Contains(body(r), "We sent a new code.") {
		t.Fatalf("resend: %d %s", r.StatusCode, r.Body)
	}
	fresh := h.LatestCode(t, "five@example.test")
	if fresh != code {
		if r := b.Verify(t, ar, code); r.StatusCode != 400 {
			t.Fatalf("the old code still works: %d", r.StatusCode)
		}
	}
	tu.LoopbackCode(t, b.Verify(t, ar, fresh), p)
	if h.Count(t, "users", "email = 'five@example.test' AND email_trusted = 1") != 1 {
		t.Fatal("not created")
	}
	// Expired: a code past its 10 minutes, and the auth request past its own.
	ar = b.Authorize(t, tu.NewPKCE())
	b.Signup(t, ar, "", "late@example.test", tu.DefaultPassword, "")
	code = h.LatestCode(t, "late@example.test")
	h.MustExec(t, "UPDATE auth_requests SET email_code_expires_at = ? WHERE id = ?", h.Clock.Now().UnixMilli(), ar)
	if r := b.Verify(t, ar, code); r.StatusCode != 400 || !strings.Contains(body(r), "This code has expired. Send a new code.") {
		t.Fatalf("expired code: %d %s", r.StatusCode, r.Body)
	}
	b.Resend(t, ar)
	code = h.LatestCode(t, "late@example.test")
	h.Clock.Advance(10*time.Minute + time.Second)
	if r := b.Verify(t, ar, code); r.StatusCode != 400 || !strings.Contains(body(r), "This sign-in has expired") {
		t.Fatalf("expired auth request: %d", r.StatusCode)
	}
	if h.Count(t, "users", "email = 'late@example.test'") != 0 {
		t.Fatal("created after expiry")
	}
}

func TestEmail_ThreeSendsPerAuthRequest(t *testing.T) {
	h := emailHarness(t, "REQHARBOR_SIGNUP=open")
	h.CreateAdmin(t, "admin@example.test")
	b := h.Browser()
	p := tu.NewPKCE()
	ar := b.Authorize(t, p)
	if r := b.Signup(t, ar, "", "three@example.test", tu.DefaultPassword, ""); r.StatusCode != 200 {
		t.Fatalf("signup %d", r.StatusCode)
	}
	for i := 0; i < 2; i++ {
		if r := b.Resend(t, ar); r.StatusCode != 200 {
			t.Fatalf("resend %d: %d", i+1, r.StatusCode)
		}
	}
	for name, r := range map[string]*tu.Resp{
		"fourth resend":        b.Resend(t, ar),
		"signup again":         b.Signup(t, ar, "", "three@example.test", tu.DefaultPassword, ""),
		"signup another email": b.Signup(t, ar, "", "other@example.test", tu.DefaultPassword, ""),
	} {
		if r.StatusCode != 429 || !strings.Contains(body(r), "No more codes can be sent for this sign-in.") {
			t.Fatalf("%s: %d %s", name, r.StatusCode, r.Body)
		}
	}
	h.FlushMail(t)
	if n := len(h.SMTP.To("three@example.test")); n != 3 {
		t.Fatalf("%d emails, want 3", n)
	}
	if n := len(h.SMTP.To("other@example.test")); n != 0 {
		t.Fatalf("%d emails to the other address", n)
	}
	// The last code sent still works.
	tu.LoopbackCode(t, b.Verify(t, ar, h.LatestCode(t, "three@example.test")), p)
}

func TestEmail_SignupWithAnExistingAddress(t *testing.T) {
	h := emailHarness(t, "REQHARBOR_SIGNUP=open")
	h.CreateAdmin(t, "admin@example.test")
	githubUser(t, h, "gh@example.test")
	counts := func() [3]int {
		return [3]int{h.Count(t, "users", ""), h.Count(t, "password_credentials", ""), h.Count(t, "workspaces", "")}
	}
	before := counts()

	type run struct {
		signup, resend, wrong, tooMany *tu.Resp
		ar                             string
	}
	try := func(email string) run {
		b := h.Browser()
		ar := b.Authorize(t, tu.NewPKCE())
		var x run
		x.ar = ar
		x.signup = b.Signup(t, ar, "Someone", email, tu.DefaultPassword, "")
		x.resend = b.Resend(t, ar)
		for i := 0; i < 5; i++ {
			x.wrong = b.Verify(t, ar, "123456")
		}
		x.tooMany = b.Verify(t, ar, "654321")
		return x
	}
	fresh := try("fresh@example.test")
	for email, method := range map[string]string{"admin@example.test": "your email and password", "gh@example.test": "GitHub"} {
		h.FlushMail(t)
		earlier := len(h.SMTP.To(email)) // the admin's own signup code
		got := try(email)
		pairs := map[string][2]*tu.Resp{"signup": {fresh.signup, got.signup}, "resend": {fresh.resend, got.resend},
			"wrong code": {fresh.wrong, got.wrong}, "too many": {fresh.tooMany, got.tooMany}}
		for step, pr := range pairs {
			a, b := pr[0], pr[1]
			if a.StatusCode != b.StatusCode || samePage(body(a), "fresh@example.test") != samePage(body(b), email) {
				t.Fatalf("%s, %s: the page differs from a new address's:\n%d %s\n%d %s", email, step, a.StatusCode, a.Body, b.StatusCode, b.Body)
			}
		}
		for _, name := range []string{"GitHub", "password", "already"} {
			if strings.Contains(body(got.signup), name) && !strings.Contains(body(fresh.signup), name) {
				t.Fatalf("%s: the page says %q", email, name)
			}
		}
		// The same auth-request fields are written either way.
		if h.Count(t, "auth_requests", "id = ? AND pending_email = ? AND pending_phc IS NOT NULL AND email_codes_sent = 2", got.ar, email) != 1 {
			t.Fatalf("%s: auth request fields differ", email)
		}
		h.FlushMail(t)
		msgs := h.SMTP.To(email)[earlier:]
		if len(msgs) != 2 {
			t.Fatalf("%s: %d emails, want 2 (signup and resend)", email, len(msgs))
		}
		for _, m := range msgs {
			if m.Code() != "" || regexp.MustCompile(`\b\d{6}\b`).MatchString(m.Body+m.Subject()) {
				t.Fatalf("%s: the notice carries a code: %q", email, m.Body)
			}
			want := "You already have a ReqHarbor account on reqharbor.test. It signs in with " + method + "."
			if !strings.Contains(m.Body, want) || !strings.Contains(m.Body, "If you didn't just try to sign up, you can ignore this.") {
				t.Fatalf("%s: notice %q", email, m.Body)
			}
		}
	}
	if counts() != before {
		t.Fatal("a signup with an existing address created rows")
	}
	// The fresh address got codes, and the account it would create was never made.
	if n := len(h.SMTP.To("fresh@example.test")); n != 2 || h.SMTP.To("fresh@example.test")[0].Code() == "" {
		t.Fatalf("fresh: %d emails", n)
	}

	// In invite mode, the invite is checked before the email: no probing without a code.
	inv := emailHarness(t)
	inv.CreateAdmin(t, "admin@example.test")
	for _, email := range []string{"admin@example.test", "new@example.test"} {
		b := inv.Browser()
		r := b.Signup(t, b.Authorize(t, tu.NewPKCE()), "", email, tu.DefaultPassword, "")
		if r.StatusCode != 400 || !strings.Contains(body(r), "Enter the invite code") {
			t.Fatalf("%s without an invite: %d", email, r.StatusCode)
		}
	}
	inv.FlushMail(t)
	if n := len(inv.SMTP.To("admin@example.test")) + len(inv.SMTP.To("new@example.test")); n != 1 { // the admin's own signup code
		t.Fatalf("%d emails without an invite", n)
	}
}

func forgot(t *testing.T, h *tu.Harness, email string) *tu.Resp {
	t.Helper()
	b := h.Browser()
	return b.Post(t, "/auth/forgot", url.Values{"email": {email}})
}

func TestEmail_ResetByEmail(t *testing.T) {
	h := emailHarness(t, "REQHARBOR_SIGNUP=open")
	h.CreateAdmin(t, "admin@example.test")
	trusted := h.CreateUser(t, "trusted@example.test")
	squatted := h.CreateUser(t, "owner@example.test")
	// As if created before SMTP was configured: a squatter's untrusted account.
	h.MustExec(t, "UPDATE users SET email_trusted = 0 WHERE id = ?", squatted.User.ID)

	page := h.Browser().Get(t, "/auth/forgot")
	if page.StatusCode != 200 || !strings.Contains(body(page), `name="email"`) || strings.Contains(body(page), "Ask your server admin") {
		t.Fatalf("forgot page: %s", page.Body)
	}
	for email, tk := range map[string]tu.Tokens{"trusted@example.test": trusted, "owner@example.test": squatted} {
		r := forgot(t, h, email)
		if r.StatusCode != 200 || !strings.Contains(body(r), "If an account exists for that email, we sent a link.") {
			t.Fatalf("%s: %d %s", email, r.StatusCode, r.Body)
		}
		h.FlushMail(t)
		msgs := h.SMTP.To(email)
		m := msgs[len(msgs)-1]
		token := m.ResetToken()
		if token == "" || !strings.Contains(m.Body, "https://reqharbor.test/auth/reset#t="+token) || !strings.Contains(m.Body, "30 minutes") {
			t.Fatalf("%s: reset email %q", email, m.Body)
		}
		if h.Count(t, "one_time_tokens", "purpose = 'reset_password' AND user_id = ? AND email = ? AND expires_at = ?",
			tk.User.ID, email, h.Clock.Now().Add(30*time.Minute).UnixMilli()) != 1 {
			t.Fatalf("%s: token row", email)
		}
		b := h.Browser()
		r = b.Post(t, "/auth/reset", url.Values{"t": {token}, "password": {"a brand new password"}})
		if r.StatusCode != 200 || !strings.Contains(body(r), "Password changed") {
			t.Fatalf("%s: reset %d %s", email, r.StatusCode, r.Body)
		}
		if h.Count(t, "users", "id = ? AND email_trusted = 1", tk.User.ID) != 1 {
			t.Fatalf("%s: not trusted after a reset by email", email)
		}
		if res := h.Refresh(t, tk.Refresh); res.StatusCode != 400 {
			t.Fatalf("%s: a session survived the reset", email)
		}
		h.SignIn(t, email, "a brand new password")
	}
	// The link works for 30 minutes.
	forgot(t, h, "trusted@example.test")
	h.FlushMail(t)
	msgs := h.SMTP.To("trusted@example.test")
	token := msgs[len(msgs)-1].ResetToken()
	h.Clock.Advance(30*time.Minute + time.Second)
	r := h.Browser().Post(t, "/auth/reset", url.Values{"t": {token}, "password": {"yet another password"}})
	if r.StatusCode != 400 || !strings.Contains(body(r), "Ask for a new one with Forgot password") {
		t.Fatalf("expired link: %d %s", r.StatusCode, r.Body)
	}
	// A CLI link still never sets email_trusted.
	h.MustExec(t, "UPDATE users SET email_trusted = 0 WHERE id = ?", squatted.User.ID)
	var link string
	if err := h.Store.Write(context.Background(), func(tx store.WriteTx) error {
		var err error
		link, err = accounts.NewResetLink(tx, h.Config.PublicURL, "owner@example.test", time.Hour, h.Clock.Now())
		return err
	}); err != nil {
		t.Fatal(err)
	}
	h.Browser().Post(t, "/auth/reset", url.Values{"t": {strings.SplitN(link, "#t=", 2)[1]}, "password": {"cli password 123"}})
	if h.Count(t, "users", "id = ? AND email_trusted = 0", squatted.User.ID) != 1 {
		t.Fatal("a CLI link set email_trusted")
	}
}

func TestEmail_ForgotIsIdenticalInEveryCase(t *testing.T) {
	h := emailHarness(t, "REQHARBOR_SIGNUP=open")
	h.CreateAdmin(t, "admin@example.test")
	gh := githubUser(t, h, "gh@example.test")
	h.CreateUser(t, "untrusted@example.test")
	h.MustExec(t, "UPDATE users SET email_trusted = 0 WHERE email = 'untrusted@example.test'")
	var first *tu.Resp
	for _, email := range []string{"nobody@example.test", "admin@example.test", "gh@example.test", "untrusted@example.test", "NOBODY2@Example.test"} {
		r := forgot(t, h, email)
		if first == nil {
			first = r
		}
		if r.StatusCode != 200 || body(r) != body(first) {
			t.Fatalf("%s: the page differs:\n%s\n%s", email, r.Body, first.Body)
		}
		for _, word := range []string{"GitHub", "password", email} {
			if strings.Contains(strings.SplitN(body(r), "<h1>", 2)[1], word) && word != "password" {
				t.Fatalf("%s: the page names %q", email, word)
			}
		}
	}
	h.FlushMail(t)
	if n := len(h.SMTP.To("nobody@example.test")) + len(h.SMTP.To("nobody2@example.test")); n != 0 {
		t.Fatalf("%d emails to unknown addresses", n)
	}
	// Provider-only: an email naming the method, and no token.
	msgs := h.SMTP.To("gh@example.test")
	if len(msgs) != 1 || msgs[0].ResetToken() != "" ||
		!strings.Contains(msgs[0].Body, "Your account on reqharbor.test signs in with GitHub, so there is no password to reset.") {
		t.Fatalf("provider-only email %+v", msgs)
	}
	if h.Count(t, "one_time_tokens", "user_id = ?", gh) != 0 {
		t.Fatal("a reset token for a provider-only account")
	}
	// A bad address is refused the same way for everyone; it never reaches the lookup.
	if r := forgot(t, h, "not-an-email"); r.StatusCode != 400 || !strings.Contains(body(r), "Enter a valid email address.") {
		t.Fatalf("bad address: %d", r.StatusCode)
	}
}

func TestEmail_ForgotLimits(t *testing.T) {
	h := emailHarness(t)
	h.CreateAdmin(t, "admin@example.test")
	// 5 / h per email: the sixth is silently not sent (the page is unchanged).
	for i := 0; i < 6; i++ {
		if r := forgot(t, h, "admin@example.test"); r.StatusCode != 200 {
			t.Fatalf("request %d: %d", i+1, r.StatusCode)
		}
	}
	h.FlushMail(t)
	if n := h.Count(t, "one_time_tokens", "purpose = 'reset_password'"); n != 5 {
		t.Fatalf("%d reset tokens, want 5", n)
	}
	if !strings.Contains(h.Logs.String(), `"cap":"per_email_reset_requests"`) {
		t.Fatal("no log of the per-email limit")
	}
	// 20 / h per IP.
	b := h.Browser()
	for i := 0; i < 20; i++ {
		if r := b.Post(t, "/auth/forgot", url.Values{"email": {"x" + string(rune('a'+i)) + "@example.test"}}); r.StatusCode != 200 {
			t.Fatalf("request %d: %d", i+1, r.StatusCode)
		}
	}
	if r := b.Post(t, "/auth/forgot", url.Values{"email": {"last@example.test"}}); r.StatusCode != 429 || r.Header.Get("Retry-After") == "" {
		t.Fatalf("21st from one IP: %d", r.StatusCode)
	}
}

func TestEmail_CapsSuppressSendsWithoutChangingThePage(t *testing.T) {
	// Per recipient, 5 / h across every purpose.
	h := emailHarness(t, "REQHARBOR_SIGNUP=open")
	h.CreateAdmin(t, "admin@example.test") // 1: the admin's own signup code
	h.FlushMail(t)
	b := h.Browser()
	ar := b.Authorize(t, tu.NewPKCE())
	b.Signup(t, ar, "", "admin@example.test", tu.DefaultPassword, "") // 2: account_exists
	b.Resend(t, ar)                                                   // 3
	forgot(t, h, "admin@example.test")                                // 4: reset_link
	ok := forgot(t, h, "admin@example.test")                          // 5
	capped := forgot(t, h, "admin@example.test")                      // 6: suppressed
	if capped.StatusCode != ok.StatusCode || body(capped) != body(ok) {
		t.Fatal("a suppressed send changed the page")
	}
	h.FlushMail(t)
	if n := len(h.SMTP.To("admin@example.test")); n != 5 {
		t.Fatalf("%d emails to one address, want 5", n)
	}
	logs := h.Logs.String()
	if !strings.Contains(logs, `"msg":"email_suppressed","purpose":"reset","cap":"per_recipient"`) || strings.Contains(logs, "admin@example.test") {
		t.Fatalf("logs: %s", logs)
	}
	// A suppressed signup code: the page is the verify page all the same.
	b2 := h.Browser()
	ar2 := b2.Authorize(t, tu.NewPKCE())
	if r := b2.Signup(t, ar2, "", "admin@example.test", tu.DefaultPassword, ""); r.StatusCode != 200 || !strings.Contains(body(r), "Check your email") {
		t.Fatalf("capped signup: %d", r.StatusCode)
	}

	// The whole server, 2 / h here.
	g := emailHarness(t, "REQHARBOR_SIGNUP=open", "REQHARBOR_RATE_EMAIL_PER_HOUR=2", "REQHARBOR_SETUP_CODE=K7Q4-ZX2P-8M3D-1R6T")
	g.CreateAdmin(t, "admin@example.test") // 1
	var pages []*tu.Resp
	for _, email := range []string{"one@example.test", "two@example.test"} { // 2, then suppressed
		b := g.Browser()
		pages = append(pages, b.Signup(t, b.Authorize(t, tu.NewPKCE()), "", email, tu.DefaultPassword, ""))
	}
	if pages[0].StatusCode != pages[1].StatusCode || samePage(body(pages[0]), "one@example.test") != samePage(body(pages[1]), "two@example.test") {
		t.Fatal("the server cap changed the page")
	}
	g.FlushMail(t)
	if len(g.SMTP.To("one@example.test")) != 1 || len(g.SMTP.To("two@example.test")) != 0 {
		t.Fatalf("server cap: %d messages", len(g.SMTP.Messages()))
	}
	if !strings.Contains(g.Logs.String(), `"msg":"email_suppressed","purpose":"signup_code","cap":"server"`) {
		t.Fatal("no server-cap log")
	}
}

func TestEmail_AdminEmailBecomesAdminOnlyWhileNoAdminExists(t *testing.T) {
	env := []string{"REQHARBOR_ADMIN_EMAIL=Boss@Example.test", "REQHARBOR_SIGNUP=open"}
	signup := func(h *tu.Harness, email string) tu.Tokens {
		t.Helper()
		b := h.Browser()
		p := tu.NewPKCE()
		ar := b.Authorize(t, p)
		r := b.Signup(t, ar, "", email, tu.DefaultPassword, "")
		if r.StatusCode != 200 {
			t.Fatalf("signup %s: %d %s", email, r.StatusCode, r.Body)
		}
		return tu.ParseTokens(t, h.Exchange(t, tu.LoopbackCode(t, b.Verify(t, ar, h.LatestCode(t, email)), p), p))
	}

	// A fresh server: the setup page offers the admin-email signup, which needs the confirmed
	// address; any other address still needs the setup code.
	h := emailHarness(t, env...)
	b := h.Browser()
	p := tu.NewPKCE()
	page := b.Get(t, "/auth/authorize?"+p.Query().Encode())
	ar := tu.AR(t, page)
	if !strings.Contains(body(page), "Create the account with the admin email") {
		t.Fatalf("setup page: %s", page.Body)
	}
	if r := b.Get(t, "/auth/signup?ar="+ar); r.StatusCode != 200 || !strings.Contains(body(r), "Create the admin account") || !strings.Contains(body(r), "admin email") {
		t.Fatalf("admin signup page: %d %s", r.StatusCode, r.Body)
	}
	if r := b.Signup(t, ar, "", "other@example.test", tu.DefaultPassword, ""); r.StatusCode != 400 || !strings.Contains(body(r), "setup code") {
		t.Fatalf("another address on a fresh server: %d", r.StatusCode)
	}
	if r := b.Signup(t, ar, "", "boss@example.test", tu.DefaultPassword, ""); r.StatusCode != 200 || !strings.Contains(body(r), "Check your email") {
		t.Fatalf("admin email signup: %d", r.StatusCode)
	}
	r := b.Verify(t, ar, h.LatestCode(t, "boss@example.test"))
	boss := tu.ParseTokens(t, h.Exchange(t, tu.LoopbackCode(t, r, p), p))
	if !boss.User.IsAdmin || h.Count(t, "users", "id = ? AND signup_via = 'admin_email' AND email_trusted = 1", boss.User.ID) != 1 {
		t.Fatalf("fresh server: %+v", boss.User)
	}
	if h.App.SetupCode() != "" && h.Count(t, "one_time_tokens", "purpose = 'setup'") != 0 {
		t.Fatal("setup codes left after the first account")
	}

	// An admin exists: the admin email is just an account.
	h = emailHarness(t, env...)
	h.CreateAdmin(t, "owner@example.test")
	if tk := signup(h, "boss@example.test"); tk.User.IsAdmin {
		t.Fatal("admin email became admin while an admin exists")
	}

	// Accounts exist but no admin (the only one was demoted): it becomes admin.
	h = emailHarness(t, env...)
	owner := h.CreateAdmin(t, "owner@example.test")
	h.MustExec(t, "UPDATE users SET is_admin = 0 WHERE id = ?", owner.User.ID)
	if tk := signup(h, "boss@example.test"); !tk.User.IsAdmin || h.Count(t, "users", "email = 'boss@example.test' AND signup_via = 'admin_email'") != 1 {
		t.Fatalf("no admin exists: %+v", tk.User)
	}

	// Without SMTP the fresh server never offers it.
	plain := tu.New(t, tu.WithEnv(env...))
	pb := plain.Browser()
	if page := pb.Get(t, "/auth/authorize?"+tu.NewPKCE().Query().Encode()); strings.Contains(body(page), "admin email") {
		t.Fatal("the admin-email signup is offered without SMTP")
	}
}

func TestEmail_WithoutSMTPVerifyIsRefused(t *testing.T) {
	h := tu.New(t, tu.WithEnv("REQHARBOR_SIGNUP=open"))
	h.CreateAdmin(t, "admin@example.test")
	b := h.Browser()
	ar := b.Authorize(t, tu.NewPKCE())
	if r := b.Verify(t, ar, "123456"); r.StatusCode != 400 || !strings.Contains(body(r), "This sign-in has expired") {
		t.Fatalf("verify without SMTP: %d", r.StatusCode)
	}
	if r := b.Get(t, "/auth/forgot"); strings.Contains(body(r), `name="email"`) {
		t.Fatal("the forgot page takes an email without SMTP")
	}
	// With SMTP, verify on a request with no pending signup is refused the same way.
	e := emailHarness(t)
	e.CreateAdmin(t, "admin@example.test")
	eb := e.Browser()
	if r := eb.Verify(t, eb.Authorize(t, tu.NewPKCE()), "123456"); r.StatusCode != 400 || !strings.Contains(body(r), "This sign-in has expired") {
		t.Fatalf("verify with no pending signup: %d", r.StatusCode)
	}
}

func TestEmail_PagesCarryTheAuthCSP(t *testing.T) {
	h := emailHarness(t, "REQHARBOR_SIGNUP=open")
	h.CreateAdmin(t, "admin@example.test")
	b := h.Browser()
	ar := b.Authorize(t, tu.NewPKCE())
	pages := map[string]*tu.Resp{
		"verify":      b.Signup(t, ar, "", "csp@example.test", tu.DefaultPassword, ""),
		"verify fail": b.Verify(t, ar, "1"),
		"forgot":      b.Get(t, "/auth/forgot"),
		"forgot sent": b.Post(t, "/auth/forgot", url.Values{"email": {"csp@example.test"}}),
	}
	for name, r := range pages {
		if r.Header.Get("Content-Security-Policy") != web.AuthCSP || r.Header.Get("Cache-Control") != "no-store" || r.Header.Get("X-Robots-Tag") != "noindex" {
			t.Errorf("%s: headers %v", name, r.Header)
		}
		if strings.Contains(body(r), "<script") {
			t.Errorf("%s: a script", name)
		}
	}
	// Cross-origin POSTs are refused before anything is sent.
	r := b.PostWith(t, "/auth/forgot", url.Values{"email": {"admin@example.test"}}, map[string]string{"Sec-Fetch-Site": "cross-site"})
	if r.StatusCode != http.StatusForbidden {
		t.Fatalf("cross-site forgot: %d", r.StatusCode)
	}
}

// QA S9 pass 1 regressions.

func TestEmail_ASuppressedCodeNeverVerifies(t *testing.T) {
	// The QA probe: with the per-recipient cap spent, a sixth signup for the admin email on a
	// fresh server must not leave a code that could be guessed into the admin account.
	h := emailHarness(t, "REQHARBOR_ADMIN_EMAIL=boss@example.test", "REQHARBOR_SIGNUP=closed")
	for i := 0; i < 5; i++ {
		b := h.Browser()
		ar := tu.AR(t, b.Get(t, "/auth/authorize?"+tu.NewPKCE().Query().Encode()))
		if r := b.Signup(t, ar, "", "boss@example.test", tu.DefaultPassword, ""); r.StatusCode != 200 {
			t.Fatalf("signup %d: %d", i, r.StatusCode)
		}
	}
	h.FlushMail(t)
	sent := len(h.SMTP.To("boss@example.test"))
	b := h.Browser()
	p := tu.NewPKCE()
	ar := tu.AR(t, b.Get(t, "/auth/authorize?"+p.Query().Encode()))
	page := b.Signup(t, ar, "", "boss@example.test", "attacker password 1", "")
	if page.StatusCode != 200 || !strings.Contains(body(page), "Check your email") {
		t.Fatalf("6th signup: %d", page.StatusCode)
	}
	h.FlushMail(t)
	if n := len(h.SMTP.To("boss@example.test")); n != sent {
		t.Fatalf("the 6th code was sent: %d -> %d", sent, n)
	}
	var stored string
	if err := h.DB.Reader.QueryRow("SELECT email_code_hash FROM auth_requests WHERE id = ?", ar).Scan(&stored); err != nil {
		t.Fatal(err)
	}
	if !strings.HasPrefix(stored, "none:") {
		t.Fatalf("a code that was never emailed is live: %q", stored)
	}
	for _, guess := range []string{"000000", "123456", "999999"} {
		if r := b.Verify(t, ar, guess); r.StatusCode != 400 {
			t.Fatalf("guess %s: %d", guess, r.StatusCode)
		}
	}
	if h.Count(t, "users", "") != 0 {
		t.Fatal("an account was created")
	}
	// The same for a full queue: the signup's Send reports it was not queued, which is the path
	// tested above (mail tests cover that a full queue returns false).
}

func TestEmail_CodesAreKeyedHashes(t *testing.T) {
	h := emailHarness(t, "REQHARBOR_SIGNUP=open")
	h.CreateAdmin(t, "admin@example.test")
	b := h.Browser()
	ar := b.Authorize(t, tu.NewPKCE())
	b.Signup(t, ar, "", "keyed@example.test", tu.DefaultPassword, "")
	code := h.LatestCode(t, "keyed@example.test")
	var stored string
	if err := h.DB.Reader.QueryRow("SELECT email_code_hash FROM auth_requests WHERE id = ?", ar).Scan(&stored); err != nil {
		t.Fatal(err)
	}
	if stored == hashOf("email-code:"+ar+":"+code) || stored == ids.SHA256Hex("email-code:"+ar+":"+code) || len(stored) != 64 {
		t.Fatalf("the stored code hash is an unkeyed SHA-256: %q", stored)
	}
}

func TestEmail_ACodeWorksOnlyOnItsOwnAuthRequest(t *testing.T) {
	h := emailHarness(t, "REQHARBOR_SIGNUP=open")
	h.CreateAdmin(t, "admin@example.test")
	b := h.Browser()
	ar1 := b.Authorize(t, tu.NewPKCE())
	b.Signup(t, ar1, "", "two@example.test", tu.DefaultPassword, "")
	code1 := h.LatestCode(t, "two@example.test")
	ar2 := b.Authorize(t, tu.NewPKCE())
	b.Signup(t, ar2, "", "two@example.test", tu.DefaultPassword, "")
	if code2 := h.LatestCode(t, "two@example.test"); code2 == code1 {
		t.Skip("the two codes happen to be equal")
	}
	if r := b.Verify(t, ar2, code1); r.StatusCode != 400 || !strings.Contains(body(r), "That code isn&#39;t right.") {
		t.Fatalf("request 1's code on request 2: %d", r.StatusCode)
	}
	if h.Count(t, "users", "email = 'two@example.test'") != 0 {
		t.Fatal("created with another request's code")
	}
}

func TestEmail_WrongCodesAreLimitedPerEmailAcrossRequestsAndNetworks(t *testing.T) {
	h := emailHarness(t, "REQHARBOR_SIGNUP=open", "REQHARBOR_RATE_EMAIL_PER_RECIPIENT_PER_HOUR=100")
	h.CreateAdmin(t, "admin@example.test")
	// One wrong code from each of ten auth requests, each from its own network, for a new and an
	// existing address alike.
	var lastNew, lastExisting *tu.Resp
	for i := 0; i < 10; i++ {
		for _, email := range []string{"victim@example.test", "admin@example.test"} {
			b := h.Browser() // a fresh random IP
			ar := b.Authorize(t, tu.NewPKCE())
			b.Signup(t, ar, "", email, tu.DefaultPassword, "")
			wrong := "000000"
			if email == "victim@example.test" && h.LatestCode(t, email) == wrong {
				wrong = "111111"
			}
			r := b.Verify(t, ar, wrong)
			if r.StatusCode != 400 || !strings.Contains(body(r), "That code isn&#39;t right.") {
				t.Fatalf("%s wrong code %d: %d", email, i+1, r.StatusCode)
			}
		}
	}
	// Now even the right code is refused, from a new auth request on a new network; the page is
	// the same for the existing address.
	for _, email := range []string{"victim@example.test", "admin@example.test"} {
		b := h.Browser()
		ar := b.Authorize(t, tu.NewPKCE())
		b.Signup(t, ar, "", email, tu.DefaultPassword, "")
		code := "123456"
		if email == "victim@example.test" {
			code = h.LatestCode(t, email)
		}
		r := b.Verify(t, ar, code)
		if r.StatusCode != 429 || !strings.Contains(body(r), "Too many wrong codes were entered for this email.") {
			t.Fatalf("%s after 10 wrong codes: %d %s", email, r.StatusCode, r.Body)
		}
		if email == "victim@example.test" {
			lastNew = r
		} else {
			lastExisting = r
		}
	}
	if samePage(body(lastNew), "victim@example.test") != samePage(body(lastExisting), "admin@example.test") {
		t.Fatal("the locked page differs between a new and an existing address")
	}
	if h.Count(t, "users", "email = 'victim@example.test'") != 0 {
		t.Fatal("created while locked")
	}
	// Another address is unaffected, and the lock eases with time (one more try per 2.4 h).
	h.CreateUser(t, "other@example.test")
	h.Clock.Advance(3 * time.Hour)
	b := h.Browser()
	p := tu.NewPKCE()
	ar := b.Authorize(t, p)
	b.Signup(t, ar, "", "victim@example.test", tu.DefaultPassword, "")
	tu.LoopbackCode(t, b.Verify(t, ar, h.LatestCode(t, "victim@example.test")), p)
}

func TestEmail_TheAdminEmailCannotBeGuessedIntoAdmin(t *testing.T) {
	h := emailHarness(t, "REQHARBOR_ADMIN_EMAIL=boss@example.test", "REQHARBOR_SIGNUP=closed", "REQHARBOR_RATE_EMAIL_PER_RECIPIENT_PER_HOUR=100")
	// Ten wrong codes across requests and networks, then the right code is refused too.
	for i := 0; i < 10; i++ {
		b := h.Browser()
		ar := tu.AR(t, b.Get(t, "/auth/authorize?"+tu.NewPKCE().Query().Encode()))
		b.Signup(t, ar, "", "boss@example.test", "attacker password 1", "")
		code := h.LatestCode(t, "boss@example.test")
		wrong := "000000"
		if code == wrong {
			wrong = "111111"
		}
		if r := b.Verify(t, ar, wrong); r.StatusCode != 400 {
			t.Fatalf("guess %d: %d", i+1, r.StatusCode)
		}
	}
	b := h.Browser()
	ar := tu.AR(t, b.Get(t, "/auth/authorize?"+tu.NewPKCE().Query().Encode()))
	b.Signup(t, ar, "", "boss@example.test", "attacker password 1", "")
	if r := b.Verify(t, ar, h.LatestCode(t, "boss@example.test")); r.StatusCode != 429 {
		t.Fatalf("the right code after ten wrong ones: %d", r.StatusCode)
	}
	if h.Count(t, "users", "") != 0 {
		t.Fatal("an admin account was created")
	}
}

func TestEmail_ForgotDoesNoDatabaseWorkOnTheRequest(t *testing.T) {
	// The sender is held, so whatever the request itself does is all that happens until RunMail.
	var reads2, writes2 atomic.Int64
	s2 := tu.NewSMTPServer(t, tu.SMTPOptions{})
	q := tu.New(t, tu.WithSMTP(s2), tu.WithEnv("REQHARBOR_SIGNUP=open"), tu.WithoutMailSender(),
		tu.WithStoreWrap(func(st store.Store) store.Store { return &countingStore{Store: st, reads: &reads2, writes: &writes2} }))
	q.MustExec(t, "INSERT INTO users (id, email, email_trusted, signup_via, name, is_admin, created_at, updated_at) VALUES ('u1', 'pw@example.test', 1, 'cli', '', 1, 1, 1)")
	q.MustExec(t, "INSERT INTO password_credentials (user_id, phc, updated_at) VALUES ('u1', '$argon2id$v=19$m=8192,t=3,p=2$AA$AA', 1)")
	q.MustExec(t, "INSERT INTO users (id, email, email_trusted, signup_via, name, is_admin, created_at, updated_at) VALUES ('u2', 'gh@example.test', 1, 'cli', '', 0, 1, 1)")
	q.MustExec(t, "INSERT INTO identities (provider, issuer, subject, user_id, email, created_at, last_login_at) VALUES ('github', '', '9', 'u2', 'gh@example.test', 1, 1)")
	var first *tu.Resp
	for _, email := range []string{"pw@example.test", "nobody@example.test", "gh@example.test"} {
		r0, w0 := reads2.Load(), writes2.Load()
		r := forgot(t, q, email)
		if dr, dw := reads2.Load()-r0, writes2.Load()-w0; dr != 0 || dw != 0 {
			t.Fatalf("%s: the request did %d reads and %d writes", email, dr, dw)
		}
		if first == nil {
			first = r
		}
		if r.StatusCode != 200 || body(r) != body(first) {
			t.Fatalf("%s: page differs", email)
		}
	}
	if q.Count(t, "one_time_tokens", "purpose = 'reset_password'") != 0 {
		t.Fatal("a token was issued on the request path")
	}
	// The sender then does the work.
	q.RunMail(t)
	q.FlushMail(t)
	if q.Count(t, "one_time_tokens", "purpose = 'reset_password' AND user_id = 'u1'") != 1 ||
		len(s2.To("pw@example.test")) != 1 || len(s2.To("gh@example.test")) != 1 || len(s2.To("nobody@example.test")) != 0 {
		t.Fatalf("after the sender ran: %d messages", len(s2.Messages()))
	}
}

// countingStore counts transactions.
type countingStore struct {
	store.Store
	reads, writes *atomic.Int64
}

func (c *countingStore) Read(ctx context.Context, fn func(store.ReadTx) error) error {
	c.reads.Add(1)
	return c.Store.Read(ctx, fn)
}

func (c *countingStore) Write(ctx context.Context, fn func(store.WriteTx) error) error {
	c.writes.Add(1)
	return c.Store.Write(ctx, fn)
}

func TestEmail_ResetTrustsOnlyTheAddressTheLinkWentTo(t *testing.T) {
	h := emailHarness(t, "REQHARBOR_SIGNUP=open")
	h.CreateAdmin(t, "admin@example.test")
	u := h.CreateUser(t, "moved@example.test")
	h.MustExec(t, "UPDATE users SET email_trusted = 0 WHERE id = ?", u.User.ID)
	forgot(t, h, "moved@example.test")
	h.FlushMail(t)
	msgs := h.SMTP.To("moved@example.test")
	token := msgs[len(msgs)-1].ResetToken()
	// The account's address changes after the link went out (an email change, or an admin fix).
	h.MustExec(t, "UPDATE users SET email = 'new-address@example.test' WHERE id = ?", u.User.ID)
	r := h.Browser().Post(t, "/auth/reset", url.Values{"t": {token}, "password": {"a brand new password"}})
	if r.StatusCode != 200 {
		t.Fatalf("reset: %d %s", r.StatusCode, r.Body)
	}
	if h.Count(t, "users", "id = ? AND email = 'new-address@example.test' AND email_trusted = 0", u.User.ID) != 1 {
		t.Fatal("a link sent to the old address made the new one trusted (or changed the email)")
	}
}

func TestEmail_AResetRevokesTheOtherResetLinks(t *testing.T) {
	h := emailHarness(t, "REQHARBOR_SIGNUP=open")
	h.CreateAdmin(t, "admin@example.test")
	forgot(t, h, "admin@example.test")
	forgot(t, h, "admin@example.test")
	h.FlushMail(t)
	var tokens []string
	for _, m := range h.SMTP.To("admin@example.test") {
		if tk := m.ResetToken(); tk != "" {
			tokens = append(tokens, tk)
		}
	}
	if len(tokens) != 2 {
		t.Fatalf("%d links", len(tokens))
	}
	if r := h.Browser().Post(t, "/auth/reset", url.Values{"t": {tokens[1]}, "password": {"a brand new password"}}); r.StatusCode != 200 {
		t.Fatalf("reset: %d", r.StatusCode)
	}
	if r := h.Browser().Post(t, "/auth/reset", url.Values{"t": {tokens[0]}, "password": {"another new password"}}); r.StatusCode != 400 {
		t.Fatalf("the older link still works: %d", r.StatusCode)
	}
	h.SignIn(t, "admin@example.test", "a brand new password")
}
