package auth

import (
	"context"
	"crypto/rand"
	"errors"
	"fmt"
	"math/big"
	"net/http"
	"strings"
	"time"

	"github.com/mihajlopopara/reqharbor/server/internal/accounts"
	"github.com/mihajlopopara/reqharbor/server/internal/clock"
	"github.com/mihajlopopara/reqharbor/server/internal/ids"
	"github.com/mihajlopopara/reqharbor/server/internal/mail"
	"github.com/mihajlopopara/reqharbor/server/internal/store"
)

// Email flows with SMTP configured (06-go-server.md §5.5, §5.7, S9).
const (
	// EmailCodeTTL is how long a signup code is valid; the auth request's own 10 minutes bound
	// it too.
	EmailCodeTTL = 10 * time.Minute
	// MaxEmailCodeAttempts is the number of tries per code.
	MaxEmailCodeAttempts = 5
	// MaxEmailCodesSent is the number of codes (or "you already have an account" notices) one
	// auth request may send.
	MaxEmailCodesSent = 3
	// EmailResetTTL is the lifetime of a reset link sent by email.
	EmailResetTTL = 30 * time.Minute
	// MaxWrongCodesPerEmail is the number of wrong signup codes one address may receive across
	// every auth request and network per EmailCodeGuessWindow (a token bucket: one more every
	// 2.4 h). Past it, every code for that address is refused, even the right one.
	MaxWrongCodesPerEmail = 10
	EmailCodeGuessWindow  = 24 * time.Hour
)

// Email purposes, as logged (never with the address).
const (
	purposeSignupCode    = "signup_code"
	purposeAccountExists = "account_exists"
	purposeReset         = "reset" // a reset_link or a "no password" email, decided by the sender
)

const (
	msgCodeWrong     = "That code isn't right. Check the email and try again."
	msgCodeTooMany   = "Too many wrong codes. Send a new code."
	msgCodeExpired   = "This code has expired. Send a new code."
	msgCodeNone      = "Send a new code to continue."
	msgCodeLocked    = "Too many wrong codes were entered for this email. Try again tomorrow, or ask your server admin."
	msgNoMoreCodes   = "No more codes can be sent for this sign-in. Go back to ReqHarbor and click Sign in again."
	msgCodeResent    = "We sent a new code."
	msgForgotSent    = "If an account exists for that email, we sent a link."
	msgBadLinkEmail  = "This reset link is invalid, was already used or has expired. Ask for a new one with Forgot password on the sign-in page."
	msgBadLinkAdmin  = "This reset link is invalid, was already used or has expired. Ask your server admin for a new one."
	noCodeHashPrefix = "none:"
)

// errDryRun rolls back the signup rehearsal of signupByEmail.
var errDryRun = errors.New("auth: dry run")

// emailCodeHash is the stored hash of a signup code: HMAC under the server's throttle key, bound
// to its auth request, so a copy of the database cannot be searched for the 10^6 codes.
func (s *Service) emailCodeHash(arID, code string) string {
	return s.Throttle.MAC("email-code", arID, code)
}

// deadCodeHash is stored when no code reached the address (it already has an account, or the
// email was suppressed by a cap or a full queue): no typed code hashes to it.
func deadCodeHash() string { return noCodeHashPrefix + random16() }

// guessKey keys the per-email wrong-code limit.
func guessKey(email string) string { return "email-code-guess:" + email }

// sixDigits is a uniformly random 6-digit code.
func sixDigits() string {
	n, err := rand.Int(rand.Reader, big.NewInt(1_000_000))
	if err != nil {
		panic("auth: crypto/rand failed: " + err.Error())
	}
	return fmt.Sprintf("%06d", n.Int64())
}

// normalizeEmailCode keeps the digits of a typed code ("123 456", "123-456").
func normalizeEmailCode(s string) string {
	if len(s) > 32 {
		return ""
	}
	return strings.Map(func(r rune) rune {
		if r >= '0' && r <= '9' {
			return r
		}
		if r == ' ' || r == '-' || r == '\t' {
			return -1
		}
		return 'x'
	}, s)
}

// adminEmailSignup reports whether a fresh server offers the password signup for
// REQHARBOR_ADMIN_EMAIL: only with SMTP, because only a confirmed address is trusted (§5.4, §5.6).
func (s *Service) adminEmailSignup() bool {
	return s.Mail != nil && s.Config.AdminEmail != "" && s.Config.PasswordAuth
}

// methodPhrase names how an account signs in, for the emails only ("GitHub", "your email and
// password"). The pages never name it.
func (s *Service) methodPhrase(tx store.ReadTx, userID string) (string, error) {
	if _, err := tx.PasswordPHC(userID); err == nil {
		return "your email and password", nil
	} else if !errors.Is(err, store.ErrNotFound) {
		return "", err
	}
	id, err := tx.IdentityOf(userID)
	if err != nil {
		return "", err
	}
	if id.Provider == "oidc" {
		return s.Config.OIDCName, nil
	}
	return accounts.MethodLabel(tx, userID)
}

// signupByEmail is the SMTP branch of POST /auth/password/signup (§5.5). The signup rules run
// as a rolled-back rehearsal of accounts.Create (with the email trusted, as it will be once
// confirmed), so a refusal shows now rather than after the code. Then the page always moves to
// "Check your email": a new address gets a 6-digit code; an address that already has an account
// gets a notice naming its sign-in method instead, and no code. Both paths hash the password, write
// the same auth-request fields and queue one email, so neither the page nor its timing tells them
// apart.
func (s *Service) signupByEmail(w http.ResponseWriter, r *http.Request, ar *store.AuthRequest, name, email, phc string) {
	var method string
	err := s.Store.Write(r.Context(), func(tx store.WriteTx) error {
		_, err := accounts.Create(tx, s.createParams(ar, email, name, phc, true))
		if err == nil {
			return errDryRun
		}
		if errors.Is(err, accounts.ErrEmailInUse) {
			u, uerr := tx.UserByEmail(email)
			if uerr != nil {
				return uerr
			}
			if method, uerr = s.methodPhrase(tx, u.ID); uerr != nil {
				return uerr
			}
		}
		return err
	})
	exists := false
	switch {
	case errors.Is(err, errDryRun):
	case errors.Is(err, accounts.ErrEmailInUse):
		exists = true
	default:
		s.signupFailed(w, r, ar, name, email, err)
		return
	}
	s.sendSignupCode(w, r, ar, &store.PendingSignup{Email: email, Name: name, PHC: phc}, email, exists, method, "")
}

// sendSignupCode stores a new code on the auth request (and the pending signup when p is set),
// queues the email and shows the verify page. At most MaxEmailCodesSent per auth request. The code
// is stored only when its email was actually queued: a code that a cap or a full queue kept from
// the inbox could only be guessed, so a dead hash is stored instead (the page is the same).
func (s *Service) sendSignupCode(w http.ResponseWriter, r *http.Request, ar *store.AuthRequest, p *store.PendingSignup,
	email string, exists bool, method, notice string) {
	exp := clock.Millis(s.Clock.Now().Add(EmailCodeTTL))
	limited := false
	err := s.Store.Write(r.Context(), func(tx store.WriteTx) error {
		cur, err := tx.AuthRequest(ar.ID)
		if err != nil {
			return err
		}
		if cur.EmailCodesSent >= MaxEmailCodesSent {
			limited = true
			return nil
		}
		// Queued inside the transaction, so the send counts against the caps only when the
		// code is stored; the sender delivers after the commit (it is asynchronous).
		stored := deadCodeHash()
		if exists {
			s.Mail.Send(purposeAccountExists, s.accountExistsMessage(email, method))
		} else {
			code := sixDigits()
			if s.Mail.Send(purposeSignupCode, s.signupCodeMessage(email, code)) {
				stored = s.emailCodeHash(ar.ID, code)
			}
		}
		return tx.SetAuthRequestEmailCode(ar.ID, p, stored, exp)
	})
	if errors.Is(err, store.ErrNotFound) {
		s.renderError(w, r, http.StatusBadRequest, errRequestExpired)
		return
	}
	if err != nil {
		s.internal(w, r, err)
		return
	}
	if limited {
		s.renderVerify(w, r, http.StatusTooManyRequests, ar, email, msgNoMoreCodes, "")
		return
	}
	s.renderVerify(w, r, http.StatusOK, ar, email, "", notice)
}

func (s *Service) renderVerify(w http.ResponseWriter, r *http.Request, status int, ar *store.AuthRequest, email, msg, notice string) {
	d := s.arPage("Check your email", ar)
	d.Email, d.Error, d.Notice = email, msg, notice
	s.render(w, r, status, "verify", d)
}

type verifyOutcome int

const (
	verifyCreated verifyOutcome = iota
	verifyNoCode
	verifyWrong
	verifyTooMany
	verifyExpired
	verifyLocked
)

// verify is POST /auth/password/verify: the code that confirms a pending password signup, or
// (resend=1) a new code. A correct code creates the account with email_trusted=1, so
// REQHARBOR_ADMIN_EMAIL applies (§5.6), and finishes the sign-in like a signup without SMTP.
// Guessing is bounded per code (5 tries), per auth request (3 codes) and per address across every
// auth request and network (MaxWrongCodesPerEmail per day); the last check and its count run
// inside the write transaction, which the single writer serialises.
func (s *Service) verify(w http.ResponseWriter, r *http.Request) {
	ar := s.loadAR(w, r, r.PostForm.Get("ar"))
	if ar == nil {
		return
	}
	if !s.Config.PasswordAuth {
		s.renderError(w, r, http.StatusForbidden, errPasswordOff)
		return
	}
	if s.Mail == nil || ar.PendingEmail == nil || ar.PendingPHC == nil {
		s.renderError(w, r, http.StatusBadRequest, errRequestExpired)
		return
	}
	email := *ar.PendingEmail
	if r.PostForm.Get("resend") != "" {
		var exists bool
		var method string
		err := s.Store.Read(r.Context(), func(tx store.ReadTx) error {
			u, err := tx.UserByEmail(email)
			if errors.Is(err, store.ErrNotFound) {
				return nil
			}
			if err != nil {
				return err
			}
			exists = true
			method, err = s.methodPhrase(tx, u.ID)
			return err
		})
		if err != nil {
			s.internal(w, r, err)
			return
		}
		s.sendSignupCode(w, r, ar, nil, email, exists, method, msgCodeResent)
		return
	}
	if s.Growth != nil {
		if err := s.Growth(); err != nil {
			s.renderError(w, r, http.StatusInsufficientStorage, errServerFull)
			return
		}
	}
	code := normalizeEmailCode(r.PostForm.Get("code"))
	now := clock.Millis(s.Clock.Now())
	name := ""
	var outcome verifyOutcome
	var target string
	err := s.Store.Write(r.Context(), func(tx store.WriteTx) error {
		cur, err := tx.AuthRequest(ar.ID)
		if err != nil {
			return err
		}
		if cur.EmailCodeHash == nil || cur.PendingEmail == nil || cur.PendingPHC == nil {
			outcome = verifyNoCode
			return nil
		}
		if cur.PendingName != nil {
			name = *cur.PendingName
		}
		key := guessKey(*cur.PendingEmail)
		if ok, _ := s.CodeGuessLimiter.Check(key); !ok {
			outcome = verifyLocked
			return nil
		}
		n, err := tx.IncrAuthRequestAttempts(ar.ID, "email_code_attempts")
		if err != nil {
			return err
		}
		switch {
		case n > MaxEmailCodeAttempts:
			outcome = verifyTooMany
			return tx.ClearAuthRequestEmailCode(ar.ID)
		case cur.EmailCodeExpiresAt == nil || *cur.EmailCodeExpiresAt <= now:
			outcome = verifyExpired
			return tx.ClearAuthRequestEmailCode(ar.ID)
		case len(code) != 6 || !ids.Equal(s.emailCodeHash(ar.ID, code), *cur.EmailCodeHash):
			outcome = verifyWrong
			s.CodeGuessLimiter.Allow(key)
			return nil
		}
		created, err := accounts.Create(tx, s.createParams(cur, *cur.PendingEmail, name, *cur.PendingPHC, true))
		if err != nil {
			return err
		}
		if err := tx.ClearAuthRequestPending(ar.ID); err != nil {
			return err
		}
		outcome = verifyCreated
		target, err = s.issueCode(tx, ar, created.User.ID)
		return err
	})
	if errors.Is(err, store.ErrNotFound) {
		s.renderError(w, r, http.StatusBadRequest, errRequestExpired)
		return
	}
	if err != nil {
		s.signupFailed(w, r, ar, name, email, err)
		return
	}
	switch outcome {
	case verifyCreated:
		if s.UserCreated != nil {
			s.UserCreated()
		}
		redirectSeeOther(w, target)
	case verifyWrong:
		s.renderVerify(w, r, http.StatusBadRequest, ar, email, msgCodeWrong, "")
	case verifyTooMany:
		s.renderVerify(w, r, http.StatusTooManyRequests, ar, email, msgCodeTooMany, "")
	case verifyExpired:
		s.renderVerify(w, r, http.StatusBadRequest, ar, email, msgCodeExpired, "")
	case verifyLocked:
		s.renderVerify(w, r, http.StatusTooManyRequests, ar, email, msgCodeLocked, "")
	default:
		s.renderVerify(w, r, http.StatusBadRequest, ar, email, msgCodeNone, "")
	}
}

// forgot is /auth/forgot (§5.7). Without SMTP it takes no email and says to ask the admin. With
// SMTP it takes an email and always answers "If an account exists for that email, we sent a
// link": a password account (trusted or not) gets a 30-minute link, a provider-only account an
// email naming its method and no link, an unknown address nothing. Limits: 20 / h per IP (a 429
// page) and 5 / h per email (silently nothing), plus the email caps. The request itself does no
// database work: the lookup, the token and the email are a job for the email sender (resetJob),
// so the response time is the same for every address.
func (s *Service) forgot(w http.ResponseWriter, r *http.Request) {
	d := s.base("Forgot password")
	if s.Mail == nil {
		s.render(w, r, http.StatusOK, "forgot", d)
		return
	}
	d.EmailReset = true
	if r.Method != http.MethodPost {
		s.render(w, r, http.StatusOK, "forgot", d)
		return
	}
	email := accounts.NormalizeEmail(r.PostForm.Get("email"))
	if !accounts.ValidEmail(email) {
		d.Error = msgBadEmail
		s.render(w, r, http.StatusBadRequest, "forgot", d)
		return
	}
	if ok, retry := s.ForgotIPLimiter.Allow("forgot:" + s.bucket(r)); !ok {
		s.renderRateLimited(w, r, retry)
		return
	}
	if ok, _ := s.ForgotEmailLimiter.Allow("forgot:" + email); ok {
		s.Mail.Later(purposeReset, s.resetJob(email))
	} else {
		s.Logger.Info("email_suppressed", "purpose", purposeReset, "cap", "per_email_reset_requests")
	}
	d.Done = true
	s.render(w, r, http.StatusOK, "forgot", d)
}

// resetJob looks email up and returns the reset email for it, issuing a 30-minute token for a
// password account; nil for an unknown address. It runs in the email sender.
func (s *Service) resetJob(email string) func(ctx context.Context) (*mail.Message, error) {
	return func(ctx context.Context) (*mail.Message, error) {
		var msg *mail.Message
		now := s.Clock.Now()
		err := s.Store.Write(ctx, func(tx store.WriteTx) error {
			u, err := tx.UserByEmail(email)
			if errors.Is(err, store.ErrNotFound) {
				return nil
			}
			if err != nil {
				return err
			}
			if _, err := tx.PasswordPHC(u.ID); errors.Is(err, store.ErrNotFound) {
				method, err := s.methodPhrase(tx, u.ID)
				if err != nil {
					return err
				}
				m := s.noPasswordMessage(u.Email, method)
				msg = &m
				return nil
			} else if err != nil {
				return err
			}
			token := ids.Token("")
			uid, sentTo := u.ID, u.Email
			// email records where the link went: a reset through it proves control of the address
			// and sets email_trusted (§5.7). CLI links leave it NULL.
			if err := tx.InsertOneTimeToken(store.OneTimeToken{
				TokenHash: ids.SHA256Hex(token), ID: ids.Code(2), Purpose: store.PurposeResetPassword, UserID: &uid, Email: &sentTo,
				MaxUses: 1, CreatedAt: clock.Millis(now), ExpiresAt: clock.Millis(now.Add(EmailResetTTL)),
			}); err != nil {
				return err
			}
			m := s.resetLinkMessage(u.Email, s.Config.PublicURL+"/auth/reset#t="+token)
			msg = &m
			return nil
		})
		return msg, err
	}
}

// The emails. Plain text; the method is named only here, never on a page.

func (s *Service) signupCodeMessage(to, code string) mail.Message {
	return mail.Message{To: to, Subject: code + " is your ReqHarbor code for " + s.Config.ServerName, Text: fmt.Sprintf(
		"Enter this code in your browser to finish creating your ReqHarbor account on %s:\n\n"+
			"    %s\n\n"+
			"The code expires in 10 minutes.\n\n"+
			"If you didn't just try to create an account, you can ignore this email. No account is created without the code.\n\n"+
			"%s\n", s.Config.ServerName, code, s.Config.PublicURL)}
}

func (s *Service) accountExistsMessage(to, method string) mail.Message {
	hint := ""
	if method == "your email and password" {
		hint = " If you forgot the password, use \"Forgot password?\" on the sign-in page."
	}
	return mail.Message{To: to, Subject: "Your ReqHarbor account on " + s.Config.ServerName, Text: fmt.Sprintf(
		"You already have a ReqHarbor account on %s. It signs in with %s.%s\n\n"+
			"If you didn't just try to sign up, you can ignore this.\n\n"+
			"%s\n", s.Config.ServerName, method, hint, s.Config.PublicURL)}
}

func (s *Service) resetLinkMessage(to, link string) mail.Message {
	return mail.Message{To: to, Subject: "Reset your ReqHarbor password on " + s.Config.ServerName, Text: fmt.Sprintf(
		"Someone asked to reset the password of your ReqHarbor account on %s. To choose a new password, open this link:\n\n"+
			"%s\n\n"+
			"The link works once, for 30 minutes. Changing the password signs you out on every device.\n\n"+
			"If you didn't ask for this, you can ignore this email. Your password stays the same.\n", s.Config.ServerName, link)}
}

func (s *Service) noPasswordMessage(to, method string) mail.Message {
	return mail.Message{To: to, Subject: "Your ReqHarbor account on " + s.Config.ServerName, Text: fmt.Sprintf(
		"Someone asked to reset the password of your ReqHarbor account on %s.\n\n"+
			"Your account on %s signs in with %s, so there is no password to reset. Sign in with %s instead.\n\n"+
			"If you didn't ask for this, you can ignore this email.\n\n"+
			"%s\n", s.Config.ServerName, s.Config.ServerName, method, method, s.Config.PublicURL)}
}
