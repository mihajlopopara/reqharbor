package accounts_test

import (
	"context"
	"errors"
	"testing"
	"time"

	"github.com/mihajlopopara/reqharbor/server/internal/accounts"
	"github.com/mihajlopopara/reqharbor/server/internal/clock"
	"github.com/mihajlopopara/reqharbor/server/internal/store"
	tu "github.com/mihajlopopara/reqharbor/server/internal/testutil"
)

func TestNormalizeAndValidate(t *testing.T) {
	if got := accounts.NormalizeEmail("  Ada@Example.TEST "); got != "ada@example.test" {
		t.Fatal(got)
	}
	for e, want := range map[string]bool{
		"a@b.example": true, "first.last+tag@sub.example.org": true, "": false, "a": false, "a@b": false,
		"@b.example": false, "a@": false, "a b@c.example": false, "<a@b.example>": false, "Ada <a@b.example>": false,
		"a@b.example\n": false,
	} {
		if got := accounts.ValidEmail(e); got != want {
			t.Errorf("ValidEmail(%q) = %v", e, got)
		}
	}
	for pw, want := range map[string]string{
		"123456789":                    "Use at least 10 characters for your password.",
		"1234567890":                   "",
		"éééééééééé":                   "",
		string(make([]byte, 257)):      "Use at most 256 characters for your password.",
		"Me@Example.test":              "Your password can't be your email address.",
		"me@example.test is not mine!": "",
	} {
		if got := accounts.PasswordProblem(pw, "me@example.test"); got != want {
			t.Errorf("PasswordProblem(%q) = %q", pw, got)
		}
	}
	if accounts.NormalizeCode(" k7q4-zx2p 8m3d-iLo6 ") != "K7Q4ZX2P8M3D1106" {
		t.Fatal(accounts.NormalizeCode(" k7q4-zx2p 8m3d-iLo6 "))
	}
	if accounts.CodeHash("K7Q4-ZX2P") != accounts.CodeHash("k7q4zx2p") {
		t.Fatal("code hash depends on spelling")
	}
	if accounts.MaskEmail("john@acme.dev") != "j***@acme.dev" || accounts.MaskEmail("x") != "***" {
		t.Fatal(accounts.MaskEmail("john@acme.dev"))
	}
}

const phc = "$argon2id$v=19$m=8192,t=3,p=2$AAAAAAAAAAAAAAAAAAAAAA$AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"

func create(t *testing.T, h *tu.Harness, p accounts.CreateParams) (*accounts.Created, error) {
	t.Helper()
	if p.PHC == "" && p.Identity == nil {
		p.PHC = phc
	}
	if p.Now.IsZero() {
		p.Now = h.Clock.Now()
	}
	var c *accounts.Created
	err := h.Store.Write(context.Background(), func(tx store.WriteTx) error {
		var err error
		c, err = accounts.Create(tx, p)
		return err
	})
	return c, err
}

func TestCreate_AdminEmailRules(t *testing.T) {
	const boss = "boss@example.test"
	// Trusted, no admin yet: admin, and the signup mode is bypassed for this creation.
	h := tu.New(t)
	c, err := create(t, h, accounts.CreateParams{Email: boss, EmailTrusted: true, SignupMode: "closed", AdminEmail: boss})
	if err != nil || !c.User.IsAdmin || c.User.SignupVia != accounts.ViaAdminEmail || !c.User.EmailTrusted {
		t.Fatalf("trusted admin email: %+v %v", c, err)
	}
	// Untrusted: never; the first account then needs the setup code.
	h = tu.New(t)
	if _, err := create(t, h, accounts.CreateParams{Email: boss, SignupMode: "open", AdminEmail: boss}); !errors.Is(err, accounts.ErrSetupRequired) {
		t.Fatalf("untrusted admin email as first account: %v", err)
	}
	// Ignored once an admin exists.
	h = tu.New(t)
	if _, err := create(t, h, accounts.CreateParams{Email: "first@example.test", CLI: true}); err != nil {
		t.Fatal(err)
	}
	c, err = create(t, h, accounts.CreateParams{Email: boss, EmailTrusted: true, SignupMode: "open", AdminEmail: boss})
	if err != nil || c.User.IsAdmin || c.User.SignupVia != accounts.ViaOpen {
		t.Fatalf("admin email with an admin present: %+v %v", c, err)
	}
	// Another trusted email is not the admin email.
	c, err = create(t, h, accounts.CreateParams{Email: "other@example.test", EmailTrusted: true, SignupMode: "open", AdminEmail: boss})
	if err != nil || c.User.IsAdmin {
		t.Fatalf("other email: %+v %v", c, err)
	}
}

// REQHARBOR_ADMIN_EMAIL bypasses the signup mode only: REQHARBOR_MAX_USERS still applies unless
// this is the first account (§5.6).
func TestCreate_AdminEmailRespectsMaxUsers(t *testing.T) {
	const boss = "boss@example.test"
	h := tu.New(t)
	// Two users and no admin (the CLI's first user is demoted).
	a, err := create(t, h, accounts.CreateParams{Email: "a@example.test", CLI: true})
	if err != nil {
		t.Fatal(err)
	}
	if _, err := create(t, h, accounts.CreateParams{Email: "b@example.test", CLI: true}); err != nil {
		t.Fatal(err)
	}
	if err := h.Store.Write(context.Background(), func(tx store.WriteTx) error {
		return tx.SetUserAdmin(a.User.ID, false, clock.Millis(h.Clock.Now()))
	}); err != nil {
		t.Fatal(err)
	}
	if _, err := create(t, h, accounts.CreateParams{Email: boss, EmailTrusted: true, SignupMode: "closed", AdminEmail: boss, MaxUsers: 2}); !errors.Is(err, accounts.ErrUserLimit) {
		t.Fatalf("admin email over the cap: %v", err)
	}
	c, err := create(t, h, accounts.CreateParams{Email: boss, EmailTrusted: true, SignupMode: "closed", AdminEmail: boss, MaxUsers: 3})
	if err != nil || !c.User.IsAdmin || c.User.SignupVia != accounts.ViaAdminEmail {
		t.Fatalf("admin email under the cap: %+v %v", c, err)
	}
	// The first account is never refused by the cap.
	h = tu.New(t)
	if c, err := create(t, h, accounts.CreateParams{Email: boss, EmailTrusted: true, SignupMode: "closed", AdminEmail: boss, MaxUsers: 1}); err != nil || !c.User.IsAdmin {
		t.Fatalf("first account via admin email: %+v %v", c, err)
	}
}

func TestCreate_CLI(t *testing.T) {
	h := tu.New(t, tu.WithEnv("REQHARBOR_MAX_USERS=2"))
	first, err := create(t, h, accounts.CreateParams{Email: "a@example.test", EmailTrusted: true, CLI: true, MaxUsers: 2})
	if err != nil || !first.User.IsAdmin || first.User.SignupVia != accounts.ViaCLI {
		t.Fatalf("first CLI user: %+v %v", first, err)
	}
	second, err := create(t, h, accounts.CreateParams{Email: "b@example.test", EmailTrusted: true, CLI: true, MaxUsers: 2})
	if err != nil || second.User.IsAdmin {
		t.Fatalf("second: %+v %v", second, err)
	}
	if _, err := create(t, h, accounts.CreateParams{Email: "c@example.test", CLI: true, MaxUsers: 2}); !errors.Is(err, accounts.ErrUserLimit) {
		t.Fatalf("over the cap: %v", err)
	}
	third, err := create(t, h, accounts.CreateParams{Email: "c@example.test", CLI: true, CLIAdmin: true, Force: true, MaxUsers: 2})
	if err != nil || !third.User.IsAdmin {
		t.Fatalf("--force --admin: %+v %v", third, err)
	}
	if _, err := create(t, h, accounts.CreateParams{Email: "c@example.test", CLI: true, Force: true}); !errors.Is(err, accounts.ErrEmailInUse) {
		t.Fatalf("duplicate: %v", err)
	}
	// Each account owns one personal team, membership and space.
	for _, c := range []*accounts.Created{first, second, third} {
		if h.Count(t, "team_members m JOIN teams t ON t.id = m.team_id JOIN workspaces w ON w.team_id = t.id",
			"m.user_id = ? AND m.role = 'owner' AND t.personal = 1 AND t.owner_user_id = m.user_id AND w.id = ? AND w.name = 'Personal'",
			c.User.ID, c.Personal.WorkspaceID) != 1 {
			t.Fatalf("personal space of %s", c.User.Email)
		}
	}
}

func TestCreate_GrantsAndModes(t *testing.T) {
	h := tu.New(t)
	setup := h.App.SetupCode()
	if _, err := create(t, h, accounts.CreateParams{Email: "a@example.test", SignupMode: "open"}); !errors.Is(err, accounts.ErrSetupRequired) {
		t.Fatalf("no setup code: %v", err)
	}
	if _, err := create(t, h, accounts.CreateParams{Email: "a@example.test", SignupMode: "open", GrantHash: accounts.CodeHash("WRONG")}); !errors.Is(err, accounts.ErrInviteInvalid) {
		t.Fatalf("wrong code: %v", err)
	}
	c, err := create(t, h, accounts.CreateParams{Email: "a@example.test", SignupMode: "closed", GrantHash: accounts.CodeHash(setup)})
	if err != nil || !c.User.IsAdmin || c.User.SignupVia != accounts.ViaSetup {
		t.Fatalf("setup: %+v %v", c, err)
	}
	inv := h.Invite(t, "only@example.test", 1)
	for mode, want := range map[string]error{"closed": accounts.ErrSignupClosed, "invite": accounts.ErrInviteRequired} {
		if _, err := create(t, h, accounts.CreateParams{Email: "x@example.test", SignupMode: mode}); !errors.Is(err, want) {
			t.Fatalf("%s without a code: %v", mode, err)
		}
	}
	if _, err := create(t, h, accounts.CreateParams{Email: "x@example.test", SignupMode: "invite", GrantHash: accounts.CodeHash(inv)}); !errors.Is(err, accounts.ErrInviteInvalid) {
		t.Fatalf("restricted invite, other email: %v", err)
	}
	if _, err := create(t, h, accounts.CreateParams{Email: "only@example.test", SignupMode: "closed", GrantHash: accounts.CodeHash(inv)}); !errors.Is(err, accounts.ErrSignupClosed) {
		t.Fatalf("invite in closed mode: %v", err)
	}
	c, err = create(t, h, accounts.CreateParams{Email: "only@example.test", SignupMode: "invite", GrantHash: accounts.CodeHash(inv)})
	if err != nil || c.User.SignupVia != accounts.ViaInvite || c.User.IsAdmin {
		t.Fatalf("invite: %+v %v", c, err)
	}
	if _, err := create(t, h, accounts.CreateParams{Email: "again@example.test", SignupMode: "invite", GrantHash: accounts.CodeHash(inv)}); !errors.Is(err, accounts.ErrInviteInvalid) {
		t.Fatalf("used invite: %v", err)
	}
	c, err = create(t, h, accounts.CreateParams{Email: "open@example.test", SignupMode: "open"})
	if err != nil || c.User.SignupVia != accounts.ViaOpen {
		t.Fatalf("open: %+v %v", c, err)
	}
	if _, err := create(t, h, accounts.CreateParams{Email: "x@example.test", SignupMode: "open", MaxUsers: 3}); !errors.Is(err, accounts.ErrUserLimit) {
		t.Fatalf("MaxUsers: %v", err)
	}
	if _, err := create(t, h, accounts.CreateParams{Email: "x@example.test", PHC: phc, Identity: &store.Identity{Provider: "dev", Subject: "1"}}); err == nil {
		t.Fatal("both PHC and Identity accepted")
	}
}

func TestSetupCodeAndInviteHelpers(t *testing.T) {
	h := tu.New(t)
	var code string
	err := h.Store.Write(context.Background(), func(tx store.WriteTx) error {
		var err error
		code, err = accounts.NewSetupCode(tx, "", time.Hour, h.Clock.Now())
		if err != nil || len(code) != 19 {
			t.Errorf("setup code %q %v", code, err)
		}
		if _, err := accounts.NewInvite(tx, "not an email", 1, time.Hour, h.Clock.Now()); err == nil {
			t.Error("invite for an invalid email")
		}
		if _, err := accounts.NewInvite(tx, "", 0, time.Hour, h.Clock.Now()); err == nil {
			t.Error("invite with 0 uses")
		}
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}
	if h.Count(t, "one_time_tokens", "purpose = 'setup'") != 1 {
		t.Fatal("a new setup code must replace the old one")
	}
	if code == h.App.SetupCode() {
		t.Fatal("the code did not change")
	}
	if _, err := create(t, h, accounts.CreateParams{Email: "admin@example.test", SignupMode: "invite", GrantHash: accounts.CodeHash(h.App.SetupCode())}); !errors.Is(err, accounts.ErrInviteInvalid) {
		t.Fatalf("replaced setup code: %v", err)
	}
	if _, err := create(t, h, accounts.CreateParams{Email: "admin@example.test", SignupMode: "invite", GrantHash: accounts.CodeHash(code)}); err != nil {
		t.Fatal(err)
	}
	err = h.Store.Write(context.Background(), func(tx store.WriteTx) error {
		_, err := accounts.NewSetupCode(tx, "", time.Hour, h.Clock.Now())
		return err
	})
	if !errors.Is(err, accounts.ErrHasUsers) {
		t.Fatalf("setup code with users: %v", err)
	}
}

// TestCreate_OpenSignupFullRefusesOnlyOpenSignups: REQHARBOR_OPEN_SIGNUP_TOTAL_BYTES reached
// refuses a code-less open signup with server_user_limit, never an invited or CLI account.
func TestCreate_OpenSignupFullRefusesOnlyOpenSignups(t *testing.T) {
	h := tu.New(t)
	if _, err := create(t, h, accounts.CreateParams{Email: "admin@example.test", SignupMode: "open", GrantHash: accounts.CodeHash(h.App.SetupCode()), OpenSignupFull: true}); err != nil {
		t.Fatalf("setup while full: %v", err)
	}
	if _, err := create(t, h, accounts.CreateParams{Email: "open@example.test", SignupMode: "open", OpenSignupFull: true}); !errors.Is(err, accounts.ErrUserLimit) {
		t.Fatalf("open signup while full: %v", err)
	}
	if n := h.Count(t, "users", "email = ?", "open@example.test"); n != 0 {
		t.Fatalf("refused open signup created %d rows", n)
	}
	inv := h.Invite(t, "", 1)
	c, err := create(t, h, accounts.CreateParams{Email: "inv@example.test", SignupMode: "open", GrantHash: accounts.CodeHash(inv), OpenSignupFull: true})
	if err != nil || c.User.SignupVia != accounts.ViaInvite {
		t.Fatalf("invite while full: %+v %v", c, err)
	}
	if c, err := create(t, h, accounts.CreateParams{Email: "cli@example.test", CLI: true, OpenSignupFull: true}); err != nil || c.User.SignupVia != accounts.ViaCLI {
		t.Fatalf("CLI while full: %+v %v", c, err)
	}
	if c, err := create(t, h, accounts.CreateParams{Email: "open@example.test", SignupMode: "open"}); err != nil || c.User.SignupVia != accounts.ViaOpen {
		t.Fatalf("open signup when not full: %+v %v", c, err)
	}
}
