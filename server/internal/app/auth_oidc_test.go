package app_test

// S10 Google and generic OIDC (06-go-server.md §5.4, §10 S10), against testutil.FakeIdP: an
// httptest TLS IdP with discovery, a JWKS with rotating RS256/ES256 keys, a PKCE-checking token
// endpoint that returns signed ID tokens, and userinfo. Claim checks are table-tested in
// internal/auth/providers; these cases run them through the whole browser flow.

import (
	"net/http"
	"net/http/httptest"
	"net/url"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"github.com/mihajlopopara/reqharbor/server/internal/accounts"
	"github.com/mihajlopopara/reqharbor/server/internal/web"

	tu "github.com/mihajlopopara/reqharbor/server/internal/testutil"
)

func googleHarness(t *testing.T, env ...string) (*tu.Harness, *tu.FakeIdP) {
	t.Helper()
	g := tu.NewFakeGoogle(t)
	return tu.New(t, append(g.Options(), tu.WithEnv(env...))...), g
}

// oidcHarness starts a server with the fake OIDC provider and waits until discovery succeeded.
func oidcHarness(t *testing.T, env ...string) (*tu.Harness, *tu.FakeIdP) {
	t.Helper()
	f := tu.NewFakeOIDC(t)
	h := tu.New(t, append(f.Options(), tu.WithEnv(env...))...)
	h.WaitMethod(t, "oidc")
	return h, f
}

func idpUser(sub, email string) *tu.IdPUser {
	return &tu.IdPUser{Sub: sub, Email: email, EmailVerified: true, Name: "User " + sub, Picture: "https://pics.example.test/" + sub + ".png"}
}

// signInFails runs a sign-in as u that must end on provider_error (502), logged with reason.
func signInFails(t *testing.T, h *tu.Harness, f *tu.FakeIdP, u *tu.IdPUser, label, reason string) string {
	t.Helper()
	logged := h.Logs.Count(`"reason":"` + reason + `"`)
	b := h.Browser()
	ar := b.Authorize(t, tu.NewPKCE())
	r := b.IdPSignIn(t, f, ar, u)
	if r.StatusCode != http.StatusBadGateway || !strings.Contains(string(r.Body), label+" sign-in didn&#39;t finish.") {
		t.Fatalf("%d %s", r.StatusCode, r.Body)
	}
	if h.Logs.Count(`"reason":"`+reason+`"`) != logged+1 {
		t.Fatalf("log lacks reason %s: %s", reason, h.Logs.String())
	}
	return ar
}

func TestProviders_DiscoveryOrderAndLabels(t *testing.T) {
	gh := tu.NewFakeGitHub(t)
	g := tu.NewFakeGoogle(t)
	o := tu.NewFakeOIDC(t)
	opts := append(append(append(gh.Options(), g.Options()...), o.Options()...), tu.WithEnv("REQHARBOR_OIDC_NAME=Acme SSO"))
	h := tu.New(t, opts...)
	h.WaitMethod(t, "oidc")
	if got := strings.Join(methodIDs(t, h), ","); got != "password=Email and password,github=GitHub,google=Google,oidc=Acme SSO" {
		t.Fatal(got)
	}
	h.CreateAdmin(t, "admin@example.test") // a fresh server shows the setup page instead
	page := string(h.Browser().Get(t, "/auth/authorize?"+tu.NewPKCE().Query().Encode()).Body)
	for _, s := range []string{"Continue with GitHub", "Continue with Google", "Continue with Acme SSO"} {
		if !strings.Contains(page, s) {
			t.Errorf("authorize page lacks %q", s)
		}
	}
	// Not configured: not listed and no routes.
	h2 := tu.New(t)
	for _, p := range []string{"google", "oidc"} {
		if r := h2.Call(t, "/auth/start/"+p, tu.Req{}); r.StatusCode != 404 {
			t.Fatalf("%s: %d", p, r.StatusCode)
		}
	}
}

func TestGoogle_StartRedirectsWithPromptNoncePKCE(t *testing.T) {
	h, g := googleHarness(t, "REQHARBOR_SIGNUP=open")
	h.CreateAdmin(t, "admin@example.test")
	b := h.Browser()
	p := tu.NewPKCE()
	ar := b.Authorize(t, p)
	res := b.Get(t, "/auth/start/google?ar="+ar)
	if res.StatusCode != http.StatusFound || res.Header.Get("Cache-Control") != "no-store" {
		t.Fatalf("start: %d", res.StatusCode)
	}
	loc := res.Header.Get("Location")
	cb := g.Authorize(t, loc, idpUser("g-1", "a@example.test")) // checks prompt=select_account, nonce, S256, scope
	q, _ := url.Parse(loc)
	nonce := q.Query().Get("nonce")
	if h.Count(t, "auth_requests", "id = ? AND provider = 'google' AND upstream_nonce = ? AND upstream_verifier IS NOT NULL", ar, nonce) != 1 {
		t.Fatal("nonce not stored with the round trip")
	}
	tu.LoopbackCode(t, b.Get(t, cb), p)
	if h.Count(t, "auth_requests", "id = ? AND upstream_nonce IS NULL AND upstream_state_hash IS NULL", ar) != 1 {
		t.Fatal("the round trip was not cleared")
	}
	// Each round trip gets its own nonce.
	ar2 := b.Authorize(t, tu.NewPKCE())
	q2, _ := url.Parse(b.StartProvider(t, "google", ar2))
	if q2.Query().Get("nonce") == nonce {
		t.Fatal("nonce reused")
	}
}

func TestGoogle_SignUpIsTrustedAndHonoursAdminEmail(t *testing.T) {
	h, g := googleHarness(t, "REQHARBOR_ADMIN_EMAIL=Boss@Example.test")
	u := idpUser("1081", "BOSS@example.test")
	tk := h.IdPTokens(t, g, u)
	row := userByEmail(t, h, "boss@example.test")
	if !tk.User.IsAdmin || !row.Admin || !row.Trusted || row.Via != accounts.ViaAdminEmail || row.Name != "User 1081" ||
		row.Avatar == nil || *row.Avatar != u.Picture {
		t.Fatalf("%+v %+v", tk.User, row)
	}
	// Google identities have no issuer (§8.3); the subject is the sub claim.
	if h.Count(t, "identities", "provider = 'google' AND issuer = '' AND subject = '1081' AND email = 'boss@example.test'") != 1 {
		t.Fatal("identity")
	}
	// Returning: a new verified email replaces users.email.
	u.Email = "boss2@example.test"
	if again := h.IdPTokens(t, g, u); again.User.ID != tk.User.ID {
		t.Fatal("returning user resolved to another account")
	}
	if row := userByEmail(t, h, "boss2@example.test"); row.ID != tk.User.ID || !row.Trusted {
		t.Fatalf("%+v", row)
	}
	// Upstream tokens and the client secret never reach the logs or the database.
	for _, s := range append(g.Tokens(), g.ClientSecret) {
		if strings.Contains(h.Logs.String(), s) || h.Count(t, "auth_requests", "upstream_verifier = ? OR upstream_nonce = ?", s, s) != 0 {
			t.Fatal("a token or the secret leaked")
		}
	}
}

func TestGoogle_EmailMustBeVerified(t *testing.T) {
	for _, tc := range []struct {
		name     string
		verified any
	}{
		{"email_verified false", false},
		{"email_verified missing", nil},
		{"email_verified \"false\"", "false"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			h, g := googleHarness(t, "REQHARBOR_SIGNUP=open")
			h.CreateAdmin(t, "admin@example.test")
			b := h.Browser()
			ar := b.Authorize(t, tu.NewPKCE())
			u := idpUser("g-2", "unverified@example.test")
			u.EmailVerified = tc.verified
			r := b.IdPSignIn(t, g, ar, u)
			if r.StatusCode != 400 || !strings.Contains(string(r.Body), "Your Google account has no verified email.") ||
				!strings.Contains(string(r.Body), "/auth/start/google?ar="+ar) || h.Count(t, "users", "") != 1 {
				t.Fatalf("%d %s", r.StatusCode, r.Body)
			}
		})
	}
}

// Every ID-token check refuses the sign-in with provider_error (502) and a loggable reason, and
// creates nothing.
func TestGoogle_IDTokenChecks(t *testing.T) {
	h, g := googleHarness(t, "REQHARBOR_SIGNUP=open")
	h.CreateAdmin(t, "admin@example.test")
	unpublished := tu.NewIdPKey(t, "rsa-unpublished", "RS256")
	now := h.Clock.Now()
	for _, tc := range []struct {
		name   string
		mutate func(u *tu.IdPUser)
		reason string
	}{
		{"wrong issuer", func(u *tu.IdPUser) { u.Claims = map[string]any{"iss": "https://evil.example.com"} }, "iss_mismatch"},
		{"wrong audience", func(u *tu.IdPUser) { u.Claims = map[string]any{"aud": "someone-else"} }, "aud_mismatch"},
		{"expired", func(u *tu.IdPUser) { u.Claims = map[string]any{"exp": now.Add(-time.Hour).Unix()} }, "expired"},
		{"nonce of another round trip", func(u *tu.IdPUser) { u.Claims = map[string]any{"nonce": "replayed"} }, "nonce_mismatch"},
		{"no nonce", func(u *tu.IdPUser) { u.Claims = map[string]any{"nonce": nil} }, "nonce_missing"},
		{"alg none", func(u *tu.IdPUser) { u.Header = map[string]any{"alg": "none"} }, "id_token_alg"},
		{"HS256 with the client secret", func(u *tu.IdPUser) { u.Header = map[string]any{"alg": "HS256"} }, "id_token_alg"},
		{"signed with an unpublished key", func(u *tu.IdPUser) { u.Key = unpublished }, "jwks_unknown_kid"},
		{"unpublished key under a published kid", func(u *tu.IdPUser) {
			u.Key, u.Header = unpublished, map[string]any{"kid": "rsa-1"}
		}, "id_token_signature"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			users := h.Count(t, "users", "")
			u := idpUser("g-"+tc.reason, tc.reason+"@example.test")
			tc.mutate(u)
			ar := signInFails(t, h, g, u, "Google", tc.reason)
			if h.Count(t, "users", "") != users || h.Count(t, "auth_requests", "id = ? AND code_hash IS NOT NULL", ar) != 0 {
				t.Fatal("created rows or minted a code")
			}
		})
	}
	// Google's second issuer form is accepted.
	u := idpUser("g-alt", "alt@example.test")
	u.Claims = map[string]any{"iss": "accounts.google.com"}
	h.IdPTokens(t, g, u)
}

func TestGoogle_AccessDeniedGoesBackToTheLoopback(t *testing.T) {
	h, g := googleHarness(t, "REQHARBOR_SIGNUP=open")
	h.CreateAdmin(t, "admin@example.test")
	b := h.Browser()
	p := tu.NewPKCE()
	ar := b.Authorize(t, p)
	res := b.Get(t, g.Deny(t, b.StartProvider(t, "google", ar)))
	loc, _ := url.Parse(res.Header.Get("Location"))
	if res.StatusCode != http.StatusSeeOther || loc.Query().Get("error") != "access_denied" || loc.Query().Get("state") != p.State || len(loc.Query()) != 2 {
		t.Fatalf("%d %q", res.StatusCode, res.Header.Get("Location"))
	}
}

// OIDC is hidden until discovery succeeds, then listed without a restart (late enablement).
func TestOIDC_LateEnablement(t *testing.T) {
	f := tu.NewFakeOIDC(t)
	f.FailDiscovery(1 << 30)
	h := tu.New(t, append(f.Options(), tu.WithEnv("REQHARBOR_SIGNUP=open"))...)
	deadline := time.Now().Add(5 * time.Second)
	for f.Hits("discovery") < 2 && time.Now().Before(deadline) {
		time.Sleep(2 * time.Millisecond)
	}
	if h.HasMethod(t, "oidc") {
		t.Fatal("listed before discovery succeeded")
	}
	if r := h.Call(t, "/auth/start/oidc?ar=x", tu.Req{}); r.StatusCode != 404 {
		t.Fatalf("start before discovery: %d", r.StatusCode)
	}
	if strings.Contains(string(h.Browser().Get(t, "/auth/authorize?"+tu.NewPKCE().Query().Encode()).Body), "Single sign-on") {
		t.Fatal("button shown before discovery")
	}
	f.FailDiscovery(0)
	h.WaitMethod(t, "oidc")
	if !strings.Contains(h.Logs.String(), "OIDC discovery failed; retrying") || !strings.Contains(h.Logs.String(), "OIDC sign-in enabled") {
		t.Fatal(h.Logs.String())
	}
	h.CreateAdmin(t, "admin@example.test")
	tk := h.IdPTokens(t, f, idpUser("o-1", "late@example.test"))
	if h.Count(t, "identities", "provider = 'oidc' AND issuer = ? AND subject = 'o-1' AND user_id = ?", f.Issuer, tk.User.ID) != 1 {
		t.Fatal("identity not keyed by issuer and sub")
	}
}

// A discovery document that can never work disables the provider at once, without retries.
func TestOIDC_DiscoveryConfigErrorsDisableTheProvider(t *testing.T) {
	for _, tc := range []struct {
		name   string
		mutate func(base string, doc map[string]any)
		log    string
	}{
		{"issuer mismatch", func(_ string, d map[string]any) { d["issuer"] = "https://other.example.com/realms/acme" }, "differs from REQHARBOR_OIDC_ISSUER"},
		{"http token_endpoint", func(_ string, d map[string]any) { d["token_endpoint"] = "http://idp.example.com/token" }, "token_endpoint"},
		{"http authorization_endpoint", func(_ string, d map[string]any) { d["authorization_endpoint"] = "http://idp.example.com/auth" }, "authorization_endpoint"},
		{"http userinfo_endpoint", func(_ string, d map[string]any) { d["userinfo_endpoint"] = "http://idp.example.com/userinfo" }, "userinfo_endpoint"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			f := tu.NewFakeOIDC(t)
			f.SetDoc(func(d map[string]any) { tc.mutate(f.Server.URL, d) })
			h := tu.New(t, f.Options()...)
			deadline := time.Now().Add(5 * time.Second)
			for !strings.Contains(h.Logs.String(), "OIDC sign-in disabled") && time.Now().Before(deadline) {
				time.Sleep(2 * time.Millisecond)
			}
			logs := h.Logs.String()
			if !strings.Contains(logs, "OIDC sign-in disabled") || !strings.Contains(logs, tc.log) || f.Hits("discovery") != 1 {
				t.Fatalf("hits %d, logs %s", f.Hits("discovery"), logs)
			}
			if h.HasMethod(t, "oidc") || h.Call(t, "/auth/start/oidc?ar=x", tu.Req{}).StatusCode != 404 {
				t.Fatal("provider enabled")
			}
		})
	}
}

// The token call refuses a redirect from the https token endpoint to http.
func TestOIDC_TokenRedirectToHTTPIsRefused(t *testing.T) {
	h, f := oidcHarness(t, "REQHARBOR_SIGNUP=open")
	h.CreateAdmin(t, "admin@example.test")
	var hits atomic.Int32
	plain := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		hits.Add(1)
		_, _ = w.Write([]byte(`{"access_token":"x","token_type":"Bearer"}`))
	}))
	defer plain.Close()
	f.RedirectToken(plain.URL + "/token")
	signInFails(t, h, f, idpUser("o-r", "r@example.test"), "Single sign-on", "insecure_redirect")
	if hits.Load() != 0 || h.Count(t, "users", "") != 1 {
		t.Fatal("the http endpoint was called or an account was created")
	}
}

// aud as a string, as an array containing client_id, as an array without it, several audiences
// without azp and with a wrong azp (§5.4 audience rules).
func TestOIDC_Audience(t *testing.T) {
	h, f := oidcHarness(t, "REQHARBOR_SIGNUP=open")
	h.CreateAdmin(t, "admin@example.test")
	for i, tc := range []struct {
		name   string
		claims map[string]any
		reason string // "" = signs in
	}{
		{"string", map[string]any{"aud": f.ClientID}, ""},
		{"array containing client_id", map[string]any{"aud": []string{f.ClientID}}, ""},
		{"several with azp", map[string]any{"aud": []string{f.ClientID, "account"}, "azp": f.ClientID}, ""},
		{"array without client_id", map[string]any{"aud": []string{"account"}}, "aud_mismatch"},
		{"several without azp", map[string]any{"aud": []string{f.ClientID, "account"}}, "azp_missing"},
		{"several with a wrong azp", map[string]any{"aud": []string{f.ClientID, "account"}, "azp": "account"}, "azp_mismatch"},
		{"one audience with a wrong azp", map[string]any{"azp": "account"}, "azp_mismatch"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			u := idpUser("aud-"+string(rune('a'+i)), "aud"+string(rune('a'+i))+"@example.test")
			u.Claims = tc.claims
			if tc.reason == "" {
				h.IdPTokens(t, f, u)
				return
			}
			signInFails(t, h, f, u, "Single sign-on", tc.reason)
		})
	}
}

// When the ID token has no email, the userinfo endpoint supplies it, and its sub must match.
func TestOIDC_UserinfoFallback(t *testing.T) {
	h, f := oidcHarness(t, "REQHARBOR_SIGNUP=open")
	h.CreateAdmin(t, "admin@example.test")
	u := idpUser("o-ui", "ui@example.test")
	u.NoEmailInIDToken = true
	h.IdPTokens(t, f, u)
	if row := userByEmail(t, h, "ui@example.test"); !row.Trusted || f.Hits("userinfo") != 1 {
		t.Fatalf("%+v, userinfo hits %d", row, f.Hits("userinfo"))
	}
	// An email in the ID token needs no userinfo call.
	h.IdPTokens(t, f, idpUser("o-direct", "direct@example.test"))
	if f.Hits("userinfo") != 1 {
		t.Fatal("userinfo called although the ID token had the email")
	}
	evil := idpUser("o-victim", "victim@example.test")
	evil.NoEmailInIDToken, evil.UserinfoSub = true, "o-attacker"
	signInFails(t, h, f, evil, "Single sign-on", "userinfo_sub_mismatch")
	if h.Count(t, "users", "email = 'victim@example.test'") != 0 {
		t.Fatal("account created from a mismatched userinfo")
	}
}

func TestOIDC_RequireVerifiedEmail(t *testing.T) {
	h, f := oidcHarness(t, "REQHARBOR_SIGNUP=open")
	h.CreateAdmin(t, "admin@example.test")
	u := idpUser("o-v", "v@example.test")
	u.EmailVerified = nil // an IdP such as Entra ID never sends email_verified
	b := h.Browser()
	r := b.IdPSignIn(t, f, b.Authorize(t, tu.NewPKCE()), u)
	if r.StatusCode != 400 || !strings.Contains(string(r.Body), "Your Single sign-on account has no verified email.") || h.Count(t, "users", "") != 1 {
		t.Fatalf("%d %s", r.StatusCode, r.Body)
	}
	// A returning identity still signs in without a verified email, and nothing but the profile changes.
	u.EmailVerified = true
	first := h.IdPTokens(t, f, u)
	u.EmailVerified, u.Email = false, "changed@example.test"
	if again := h.IdPTokens(t, f, u); again.User.ID != first.User.ID {
		t.Fatal("another account")
	}
	if row := userByEmail(t, h, "v@example.test"); row.ID != first.User.ID || !row.Trusted {
		t.Fatalf("%+v", row)
	}
}

// REQUIRE_VERIFIED_EMAIL=false: accounts are created untrusted, an email equal to ADMIN_EMAIL does
// not make an admin, and a returning user's changed email does not update users.email.
func TestOIDC_UnverifiedEmailsAreNeverTrusted(t *testing.T) {
	t.Run("ADMIN_EMAIL does not apply", func(t *testing.T) {
		h, f := oidcHarness(t, "REQHARBOR_OIDC_REQUIRE_VERIFIED_EMAIL=false", "REQHARBOR_ADMIN_EMAIL=boss@example.test", "REQHARBOR_SIGNUP=open")
		b := h.Browser()
		ar := b.Authorize(t, tu.NewPKCE())
		r := b.IdPSignIn(t, f, ar, idpUser("o-boss", "boss@example.test")) // even with email_verified true
		if r.StatusCode != 400 || !strings.Contains(string(r.Body), "Finish setting up this server") || !strings.Contains(string(r.Body), `name="continue" value="oidc"`) ||
			h.Count(t, "users", "") != 0 {
			t.Fatalf("%d %s", r.StatusCode, r.Body)
		}
		// With verification required the same verified email is trusted and becomes admin.
		h2, f2 := oidcHarness(t, "REQHARBOR_ADMIN_EMAIL=boss@example.test")
		if tk := h2.IdPTokens(t, f2, idpUser("o-boss", "boss@example.test")); !tk.User.IsAdmin || !userByEmail(t, h2, "boss@example.test").Trusted {
			t.Fatal("verified admin email not honoured")
		}
	})
	t.Run("untrusted account; returning email change is recorded on the identity only", func(t *testing.T) {
		h, f := oidcHarness(t, "REQHARBOR_OIDC_REQUIRE_VERIFIED_EMAIL=false", "REQHARBOR_SIGNUP=open")
		h.CreateAdmin(t, "admin@example.test")
		u := idpUser("o-u", "u@example.test")
		u.EmailVerified = nil
		first := h.IdPTokens(t, f, u)
		row := userByEmail(t, h, "u@example.test")
		if row.Trusted || row.Admin || row.ID != first.User.ID {
			t.Fatalf("%+v", row)
		}
		u.Email, u.EmailVerified = "new@example.test", true // even "verified" is not trusted in this mode
		if again := h.IdPTokens(t, f, u); again.User.ID != first.User.ID {
			t.Fatal("another account")
		}
		if row := userByEmail(t, h, "u@example.test"); row.ID != first.User.ID || row.Trusted {
			t.Fatalf("users.email changed: %+v", row)
		}
		if got := identityEmail(t, h, "oidc", "o-u"); got != "new@example.test" {
			t.Fatalf("identity email %q", got)
		}
	})
	t.Run("email in use: the page does not confirm the owner", func(t *testing.T) {
		h, f := oidcHarness(t, "REQHARBOR_OIDC_REQUIRE_VERIFIED_EMAIL=false", "REQHARBOR_SIGNUP=open")
		h.CreateAdmin(t, "admin@example.test")
		h.CreateUser(t, "jane@acme.dev")
		b := h.Browser()
		r := b.IdPSignIn(t, f, b.Authorize(t, tu.NewPKCE()), idpUser("o-jane", "jane@acme.dev"))
		if r.StatusCode != http.StatusConflict || !strings.Contains(string(r.Body), "This email is already used by another account. Ask your server admin.") ||
			strings.Contains(string(r.Body), "j***@acme.dev") {
			t.Fatalf("%d %s", r.StatusCode, r.Body)
		}
	})
}

func TestOIDC_EmailInUseNamesTheConfiguredLabel(t *testing.T) {
	h, f := oidcHarness(t, "REQHARBOR_SIGNUP=open", "REQHARBOR_OIDC_NAME=Acme SSO")
	h.CreateAdmin(t, "admin@example.test")
	h.IdPTokens(t, f, idpUser("o-first", "jane@acme.dev"))
	b := h.Browser()
	r := b.IdPSignIn(t, f, b.Authorize(t, tu.NewPKCE()), idpUser("o-second", "jane@acme.dev"))
	if r.StatusCode != http.StatusConflict || r.Header.Get("Content-Security-Policy") != web.AuthCSP ||
		!strings.Contains(string(r.Body), "j***@acme.dev already signs in with Acme SSO. Sign in with Acme SSO instead.") {
		t.Fatalf("%d %s", r.StatusCode, r.Body)
	}
}

// The JWKS is cached; a token signed with a new key (rotation) refetches it.
func TestOIDC_KeyRotation(t *testing.T) {
	h, f := oidcHarness(t, "REQHARBOR_SIGNUP=open")
	h.CreateAdmin(t, "admin@example.test")
	u := idpUser("o-rot", "rot@example.test")
	h.IdPTokens(t, f, u)
	h.IdPTokens(t, f, u)
	if n := f.Hits("jwks"); n != 1 {
		t.Fatalf("jwks fetched %d times, want 1", n)
	}
	k2 := tu.NewIdPKey(t, "ec-rotated", "ES256")
	old := tu.NewIdPKey(t, "rsa-old", "RS256")
	f.Rotate(k2, k2, old)
	h.Clock.Advance(2 * time.Minute)
	h.IdPTokens(t, f, u)
	if n := f.Hits("jwks"); n != 2 {
		t.Fatalf("jwks fetched %d times, want 2", n)
	}
}

// Scopes, the label and the email claim are configurable; a custom email claim is trusted only
// when it equals the verified email claim.
func TestOIDC_ScopesLabelAndEmailClaim(t *testing.T) {
	f := tu.NewFakeOIDC(t)
	f.Scope = "openid email groups"
	h := tu.New(t, append(f.Options(), tu.WithEnv("REQHARBOR_SIGNUP=open", "REQHARBOR_OIDC_SCOPES=openid,email groups",
		"REQHARBOR_OIDC_NAME=Contoso", "REQHARBOR_OIDC_EMAIL_CLAIM=upn"))...)
	h.WaitMethod(t, "oidc")
	h.CreateAdmin(t, "admin@example.test")
	if got := strings.Join(methodIDs(t, h), ","); got != "password=Email and password,oidc=Contoso" {
		t.Fatal(got)
	}
	same := idpUser("o-upn", "ada@contoso.test")
	same.Claims = map[string]any{"upn": "Ada@Contoso.test"}
	h.IdPTokens(t, f, same) // the authorize URL is checked for the configured scope
	if row := userByEmail(t, h, "ada@contoso.test"); !row.Trusted {
		t.Fatalf("%+v", row)
	}
	other := idpUser("o-upn2", "bob@contoso.test")
	other.Claims = map[string]any{"upn": "ceo@contoso.test"}
	b := h.Browser()
	r := b.IdPSignIn(t, f, b.Authorize(t, tu.NewPKCE()), other)
	if r.StatusCode != 400 || !strings.Contains(string(r.Body), "Your Contoso account has no verified email.") {
		t.Fatalf("%d %s", r.StatusCode, r.Body)
	}
}

// The client authenticates at the token endpoint with client_secret_basic by default and with
// client_secret_post when the provider supports only that.
func TestOIDC_ClientAuthentication(t *testing.T) {
	h, f := oidcHarness(t, "REQHARBOR_SIGNUP=open")
	h.CreateAdmin(t, "admin@example.test")
	f.RequireClientAuth("basic")
	h.IdPTokens(t, f, idpUser("o-basic", "basic@example.test"))

	f2 := tu.NewFakeOIDC(t)
	f2.SetDoc(func(d map[string]any) { d["token_endpoint_auth_methods_supported"] = []string{"client_secret_post"} })
	f2.RequireClientAuth("post")
	h2 := tu.New(t, append(f2.Options(), tu.WithEnv("REQHARBOR_SIGNUP=open"))...)
	h2.WaitMethod(t, "oidc")
	h2.CreateAdmin(t, "admin@example.test")
	h2.IdPTokens(t, f2, idpUser("o-post", "post@example.test"))
}

// A setup or invite code asked for during an OIDC sign-in resumes the OIDC round trip.
func TestOIDC_InviteCodeResumesTheProvider(t *testing.T) {
	h, f := oidcHarness(t)
	h.CreateAdmin(t, "admin@example.test")
	b := h.Browser()
	p := tu.NewPKCE()
	ar := b.Authorize(t, p)
	u := idpUser("o-inv", "inv@example.test")
	if r := b.IdPSignIn(t, f, ar, u); r.StatusCode != 400 || !strings.Contains(string(r.Body), `name="continue" value="oidc"`) {
		t.Fatalf("%d %s", r.StatusCode, r.Body)
	}
	r := b.Post(t, "/auth/code", url.Values{"ar": {ar}, "code": {h.Invite(t, "", 1)}, "continue": {"oidc"}})
	if r.StatusCode != http.StatusSeeOther || r.Header.Get("Location") != "/auth/start/oidc?ar="+ar {
		t.Fatalf("%d %q", r.StatusCode, r.Header.Get("Location"))
	}
	tu.ParseTokens(t, h.Exchange(t, tu.LoopbackCode(t, b.IdPSignIn(t, f, ar, u), p), p))
	if row := userByEmail(t, h, "inv@example.test"); row.Via != accounts.ViaInvite || !row.Trusted {
		t.Fatalf("%+v", row)
	}
}

// An email-bound invite (invite create --email) needs an email this server trusts: a password
// signup without SMTP and an OIDC sign-in under REQUIRE_VERIFIED_EMAIL=false cannot use it, and
// the refusal leaves the invite unused; a verified provider email can.
func TestInvite_EmailBoundNeedsATrustedEmail(t *testing.T) {
	const msg = "this sign-in can&#39;t confirm that you own it"
	h, f := oidcHarness(t, "REQHARBOR_OIDC_REQUIRE_VERIFIED_EMAIL=false")
	h.CreateAdmin(t, "admin@example.test")
	inv := h.Invite(t, "bound@example.test", 1)

	b := h.Browser()
	r := b.Signup(t, b.Authorize(t, tu.NewPKCE()), "Bound", "bound@example.test", "correct horse battery", inv)
	if r.StatusCode != 400 || !strings.Contains(string(r.Body), msg) {
		t.Fatalf("password signup without SMTP: %d %s", r.StatusCode, r.Body)
	}

	b = h.Browser()
	ar := b.Authorize(t, tu.NewPKCE())
	u := idpUser("o-bound", "bound@example.test")
	if r := b.IdPSignIn(t, f, ar, u); r.StatusCode != 400 || !strings.Contains(string(r.Body), `name="continue" value="oidc"`) {
		t.Fatalf("%d %s", r.StatusCode, r.Body)
	}
	if r := b.Post(t, "/auth/code", url.Values{"ar": {ar}, "code": {inv}, "continue": {"oidc"}}); r.StatusCode != http.StatusSeeOther {
		t.Fatalf("code: %d %s", r.StatusCode, r.Body)
	}
	if r := b.IdPSignIn(t, f, ar, u); r.StatusCode != 400 || !strings.Contains(string(r.Body), msg) {
		t.Fatalf("untrusted OIDC: %d %s", r.StatusCode, r.Body)
	}
	if h.Count(t, "users", "email = 'bound@example.test'") != 0 || h.Count(t, "one_time_tokens", "uses > 0 AND purpose = 'signup_invite'") != 0 {
		t.Fatal("account created or invite consumed")
	}

	// A trusted (Google, verified) email of that address uses it.
	g := tu.NewFakeGoogle(t)
	h2 := tu.New(t, g.Options()...)
	h2.CreateAdmin(t, "admin@example.test")
	inv2 := h2.Invite(t, "bound@example.test", 1)
	b2 := h2.Browser()
	p := tu.NewPKCE()
	ar2 := b2.Authorize(t, p)
	gu := idpUser("g-bound", "bound@example.test")
	b2.IdPSignIn(t, g, ar2, gu)
	if r := b2.Post(t, "/auth/code", url.Values{"ar": {ar2}, "code": {inv2}, "continue": {"google"}}); r.StatusCode != http.StatusSeeOther {
		t.Fatalf("code: %d", r.StatusCode)
	}
	tu.ParseTokens(t, h2.Exchange(t, tu.LoopbackCode(t, b2.IdPSignIn(t, g, ar2, gu), p), p))
	if row := userByEmail(t, h2, "bound@example.test"); row.Via != accounts.ViaInvite || !row.Trusted {
		t.Fatalf("%+v", row)
	}
}
