// Package accounts creates accounts and holds the email, password and code rules shared by the
// hosted pages and the CLI (06-go-server.md §5.5, §5.6, §8.3 accounts.Create).
package accounts

import (
	"errors"
	"fmt"
	"net/mail"
	"strings"
	"time"
	"unicode/utf8"

	"github.com/mihajlopopara/reqharbor/server/internal/clock"
	"github.com/mihajlopopara/reqharbor/server/internal/ids"
	"github.com/mihajlopopara/reqharbor/server/internal/store"
)

// Errors of Create. The hosted pages map each to its page code (§4).
var (
	ErrEmailInUse     = errors.New("accounts: email already has an account")           // email_in_use
	ErrUserLimit      = errors.New("accounts: REQHARBOR_MAX_USERS reached")            // server_user_limit
	ErrSignupClosed   = errors.New("accounts: signup is closed")                       // signup_closed
	ErrInviteInvalid  = errors.New("accounts: invite or setup code is not valid")      // invite_invalid
	ErrInviteRequired = errors.New("accounts: signup on this server needs an invite")  // invite_invalid
	ErrSetupRequired  = errors.New("accounts: the first account needs the setup code") // setup_required
)

// Signup sources stored in users.signup_via.
const (
	ViaSetup      = "setup"
	ViaInvite     = "invite"
	ViaOpen       = "open"
	ViaCLI        = "cli"
	ViaAdminEmail = "admin_email"
)

// PersonalName is the name of every personal team and space.
const PersonalName = "Personal"

// Password length limits, counted in runes (§5.5).
const (
	MinPasswordRunes = 10
	MaxPasswordRunes = 256
	MaxNameRunes     = 100
	MaxEmailBytes    = 254
)

// NormalizeEmail trims and lowercases an email address.
func NormalizeEmail(s string) string { return strings.ToLower(strings.TrimSpace(s)) }

// ValidEmail reports whether a normalised email is a plain addr-spec such as a@b.example.
func ValidEmail(email string) bool {
	if email == "" || len(email) > MaxEmailBytes || strings.ContainsAny(email, " \t\r\n<>\"(),;:\\[]") {
		return false
	}
	at := strings.LastIndexByte(email, '@')
	if at < 1 || at == len(email)-1 || !strings.Contains(email[at+1:], ".") {
		return false
	}
	a, err := mail.ParseAddress(email)
	return err == nil && a.Address == email && a.Name == ""
}

// PasswordProblem returns the reason a password is refused, or "" (§5.5: 10 to 256 runes and not
// equal to the email; no composition rules).
func PasswordProblem(password, email string) string {
	n := utf8.RuneCountInString(password)
	switch {
	case n < MinPasswordRunes:
		return fmt.Sprintf("Use at least %d characters for your password.", MinPasswordRunes)
	case n > MaxPasswordRunes:
		return fmt.Sprintf("Use at most %d characters for your password.", MaxPasswordRunes)
	case email != "" && strings.EqualFold(strings.TrimSpace(password), email):
		return "Your password can't be your email address."
	}
	return ""
}

// NormalizeCode canonicalises a setup or invite code as typed: spaces and dashes are dropped,
// letters are uppercased and the Crockford look-alikes I, L and O read as 1, 1 and 0.
func NormalizeCode(s string) string { return ids.NormalizeCode(s) }

// CodeHash is the stored hash of a setup or invite code.
func CodeHash(code string) string { return ids.SHA256Hex("code:" + NormalizeCode(code)) }

// CreateParams describes one account creation.
type CreateParams struct {
	Email        string // normalised
	Name         string
	AvatarURL    *string // from a provider profile
	EmailTrusted bool
	// Exactly one of PHC (a password account) and Identity (an external identity) is set.
	PHC      string
	Identity *store.Identity
	// GrantHash is the setup or invite code applied to the auth request (CodeHash), if any.
	GrantHash string
	// TicketHash is the team-invite signup ticket the auth request was started with (SHA-256 of
	// the rht_ ticket), if any (M2). Without a code, a usable ticket counts as an invite in invite
	// mode only and is consumed here; in open and closed mode, and when unusable, it is ignored
	// (so open signup keeps its REQHARBOR_OPEN_SIGNUP_TOTAL_BYTES class and cap).
	TicketHash string

	// SignupMode is REQHARBOR_SIGNUP for a page signup; CLI creations set CLI instead.
	SignupMode string
	CLI        bool
	CLIAdmin   bool // --admin
	Force      bool // --force: may exceed MaxUsers (CLI only)

	AdminEmail string // REQHARBOR_ADMIN_EMAIL, normalised
	MaxUsers   int64  // 0 means no cap
	// OpenSignupFull refuses an open signup (no code) with ErrUserLimit: the personal spaces of
	// open-signup accounts reached REQHARBOR_OPEN_SIGNUP_TOTAL_BYTES (§5.6, §6).
	OpenSignupFull bool
	Now            time.Time
}

// Created is the result of Create.
type Created struct {
	User     store.User
	Personal store.Personal
}

// Create runs §8.3 accounts.Create inside the caller's write transaction: signup rules and the
// grant token, the user, its sign-in method, and its personal team, membership and space. This is
// the only place REQHARBOR_ADMIN_EMAIL is evaluated.
func Create(tx store.WriteTx, p CreateParams) (*Created, error) {
	if (p.PHC == "") == (p.Identity == nil) {
		return nil, errors.New("accounts: exactly one of PHC and Identity must be set")
	}
	now := clock.Millis(p.Now)
	count, err := tx.UserCount()
	if err != nil {
		return nil, err
	}
	first := count == 0

	isAdmin := false
	via := ViaOpen
	adminEmail := false
	if p.AdminEmail != "" && p.EmailTrusted && p.Email == p.AdminEmail {
		exists, err := tx.AdminExists()
		if err != nil {
			return nil, err
		}
		adminEmail = !exists
	}

	// 1. The grant token (setup or invite), consumed in this transaction.
	var grant *store.OneTimeToken
	if p.GrantHash != "" {
		grant, err = tx.OneTimeToken(p.GrantHash)
		if errors.Is(err, store.ErrNotFound) {
			return nil, ErrInviteInvalid
		}
		if err != nil {
			return nil, err
		}
		switch {
		case !grant.Usable(now):
			return nil, ErrInviteInvalid
		case grant.Purpose == store.PurposeSetup && !first:
			return nil, ErrInviteInvalid // the setup code works only while no user exists
		case grant.Purpose != store.PurposeSetup && grant.Purpose != store.PurposeSignupInvite:
			return nil, ErrInviteInvalid
		case grant.Email != nil && *grant.Email != "" && NormalizeEmail(*grant.Email) != p.Email:
			return nil, ErrInviteInvalid
		}
		ok, err := tx.UseOneTimeToken(p.GrantHash, now)
		if err != nil {
			return nil, err
		}
		if !ok {
			return nil, ErrInviteInvalid
		}
	}

	switch {
	case p.CLI:
		via = ViaCLI
		isAdmin = p.CLIAdmin || first
		if !first && !p.Force && p.MaxUsers > 0 && count >= p.MaxUsers {
			return nil, ErrUserLimit
		}
	case grant != nil && grant.Purpose == store.PurposeSetup:
		via, isAdmin = ViaSetup, true
	case adminEmail:
		// Bypasses the signup mode for this one creation, not REQHARBOR_MAX_USERS (§5.6: only the
		// first account and CLI --force are exempt from the cap).
		if !first && p.MaxUsers > 0 && count >= p.MaxUsers {
			return nil, ErrUserLimit
		}
		via, isAdmin = ViaAdminEmail, true
	case first:
		return nil, ErrSetupRequired
	default:
		ticket := false
		if grant == nil && p.TicketHash != "" && p.SignupMode == "invite" {
			if ticket, err = tx.UseSignupTicket(p.TicketHash, now); err != nil {
				return nil, err
			}
		}
		if ticket {
			via = ViaInvite
		} else if grant != nil {
			if p.SignupMode != "open" && p.SignupMode != "invite" {
				return nil, ErrSignupClosed
			}
			via = ViaInvite
		} else {
			switch p.SignupMode {
			case "open":
				via = ViaOpen
				if p.OpenSignupFull {
					return nil, ErrUserLimit
				}
			case "invite":
				return nil, ErrInviteRequired
			default:
				return nil, ErrSignupClosed
			}
		}
		if p.MaxUsers > 0 && count >= p.MaxUsers {
			return nil, ErrUserLimit
		}
	}
	if adminEmail {
		isAdmin = true
	}

	// email_in_use is checked here, inside the transaction; UNIQUE(users.email) is the backstop.
	if _, err := tx.UserByEmail(p.Email); err == nil {
		return nil, ErrEmailInUse
	} else if !errors.Is(err, store.ErrNotFound) {
		return nil, err
	}

	u := store.User{
		ID: ids.ULID(p.Now), Email: p.Email, EmailTrusted: p.EmailTrusted, SignupVia: via, Name: p.Name,
		AvatarURL: p.AvatarURL, IsAdmin: isAdmin, CreatedAt: now, UpdatedAt: now,
	}
	if err := tx.InsertUser(u); err != nil {
		return nil, err
	}
	if p.PHC != "" {
		err = tx.InsertPassword(u.ID, p.PHC, now)
	} else {
		id := *p.Identity
		id.UserID, id.CreatedAt, id.LastLoginAt = u.ID, now, now
		err = tx.InsertIdentity(id)
	}
	if err != nil {
		return nil, err
	}
	personal := store.Personal{TeamID: ids.ULID(p.Now), WorkspaceID: ids.ULID(p.Now), Name: PersonalName, CreatedAt: now}
	if err := tx.InsertPersonal(u.ID, personal); err != nil {
		return nil, err
	}
	if first {
		// Setup codes are only for the first account; drop every one that is left.
		if err := tx.DeleteOneTimeTokens(store.PurposeSetup); err != nil {
			return nil, err
		}
	}
	return &Created{User: u, Personal: personal}, nil
}

// MaskEmail shows the first character of the local part and the domain: j***@acme.dev.
func MaskEmail(email string) string {
	at := strings.LastIndexByte(email, '@')
	if at < 1 {
		return "***"
	}
	r, _ := utf8.DecodeRuneInString(email)
	return string(r) + "***" + email[at:]
}
