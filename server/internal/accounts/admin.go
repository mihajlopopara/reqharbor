package accounts

import (
	"errors"
	"fmt"
	"time"

	"github.com/mihajlopopara/reqharbor/server/internal/clock"
	"github.com/mihajlopopara/reqharbor/server/internal/ids"
	"github.com/mihajlopopara/reqharbor/server/internal/store"
)

// ErrNoUser is returned by the admin operations for an unknown email.
var ErrNoUser = errors.New("no account with this email")

// ErrHasUsers refuses a setup code once an account exists.
var ErrHasUsers = errors.New("this server already has accounts; setup codes work only before the first one")

// Admin operations shared by `serve` and the CLI (06-go-server.md §6 CLI). Each runs inside the
// caller's write transaction.

// NewSetupCode replaces every setup code with a fresh one valid for ttl (or with fixed, the
// REQHARBOR_SETUP_CODE value) while no user exists. It returns the code to show the admin.
func NewSetupCode(tx store.WriteTx, fixed string, ttl time.Duration, now time.Time) (string, error) {
	n, err := tx.UserCount()
	if err != nil {
		return "", err
	}
	if n > 0 {
		return "", ErrHasUsers
	}
	if err := tx.DeleteOneTimeTokens(store.PurposeSetup); err != nil {
		return "", err
	}
	code := fixed
	if code == "" {
		code = ids.Code(4)
	}
	ms := clock.Millis(now)
	return code, tx.InsertOneTimeToken(store.OneTimeToken{
		TokenHash: CodeHash(code), ID: shortID(), Purpose: store.PurposeSetup, MaxUses: 1,
		CreatedAt: ms, ExpiresAt: clock.Millis(now.Add(ttl)),
	})
}

// shortID is the public id of a one-time token, for `invite list` and `invite revoke`.
func shortID() string { return ids.Code(2) }

// Invite is a created server invite.
type Invite struct {
	ID, Code string
	Expires  time.Time
}

// NewInvite creates a signup invite code (`invite create`). email, when set, restricts it.
func NewInvite(tx store.WriteTx, email string, uses int64, ttl time.Duration, now time.Time) (*Invite, error) {
	if uses < 1 {
		return nil, fmt.Errorf("--uses must be at least 1")
	}
	var restrict *string
	if email != "" {
		e := NormalizeEmail(email)
		if !ValidEmail(e) {
			return nil, fmt.Errorf("%q is not a valid email address", email)
		}
		restrict = &e
	}
	code := ids.Code(4)
	inv := &Invite{ID: shortID(), Code: code, Expires: now.Add(ttl)}
	ms := clock.Millis(now)
	err := tx.InsertOneTimeToken(store.OneTimeToken{
		TokenHash: CodeHash(code), ID: inv.ID, Purpose: store.PurposeSignupInvite, Email: restrict, MaxUses: uses,
		CreatedAt: ms, ExpiresAt: clock.Millis(inv.Expires),
	})
	return inv, err
}

// UserByEmail finds a user or returns ErrNoUser.
func UserByEmail(tx store.ReadTx, email string) (*store.User, error) {
	u, err := tx.UserByEmail(NormalizeEmail(email))
	if errors.Is(err, store.ErrNotFound) {
		return nil, ErrNoUser
	}
	return u, err
}

// MethodLabel names how an account signs in, for CLI messages ("Email and password", "GitHub").
func MethodLabel(tx store.ReadTx, userID string) (string, error) {
	id, err := tx.IdentityOf(userID)
	if errors.Is(err, store.ErrNotFound) {
		return "Email and password", nil
	}
	if err != nil {
		return "", err
	}
	switch id.Provider {
	case "github":
		return "GitHub", nil
	case "google":
		return "Google", nil
	case "oidc":
		return "single sign-on", nil
	case "dev":
		return "Dev sign-in", nil
	}
	return id.Provider, nil
}

// NewResetLink issues a single-use password-reset token for a password account and returns the
// link {publicURL}/auth/reset#t=<token> (`user reset-link`). Provider-only accounts are refused.
func NewResetLink(tx store.WriteTx, publicURL, email string, ttl time.Duration, now time.Time) (string, error) {
	u, err := UserByEmail(tx, email)
	if err != nil {
		return "", err
	}
	if _, err := tx.PasswordPHC(u.ID); errors.Is(err, store.ErrNotFound) {
		label, lerr := MethodLabel(tx, u.ID)
		if lerr != nil {
			return "", lerr
		}
		return "", fmt.Errorf("this account signs in with %s, so it has no password to reset", label)
	} else if err != nil {
		return "", err
	}
	token := ids.Token("")
	ms := clock.Millis(now)
	uid := u.ID
	if err := tx.InsertOneTimeToken(store.OneTimeToken{
		TokenHash: ids.SHA256Hex(token), ID: shortID(), Purpose: store.PurposeResetPassword, UserID: &uid, MaxUses: 1,
		CreatedAt: ms, ExpiresAt: clock.Millis(now.Add(ttl)),
	}); err != nil {
		return "", err
	}
	return publicURL + "/auth/reset#t=" + token, nil
}
