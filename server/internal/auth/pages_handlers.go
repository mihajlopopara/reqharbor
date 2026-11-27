package auth

import (
	"context"
	"crypto/rand"
	"encoding/base64"
	"errors"
	"net/http"
	"net/url"
	"regexp"
	"strconv"
	"strings"
	"unicode/utf8"

	"github.com/mihajlopopara/reqharbor/server/internal/accounts"
	"github.com/mihajlopopara/reqharbor/server/internal/auth/throttle"
	"github.com/mihajlopopara/reqharbor/server/internal/clock"
	"github.com/mihajlopopara/reqharbor/server/internal/config"
	"github.com/mihajlopopara/reqharbor/server/internal/ids"
	"github.com/mihajlopopara/reqharbor/server/internal/store"
)

var (
	// redirectRe is the only accepted redirect_uri: an IPv4 loopback literal, port 1024-65535,
	// path /cb (§5.2). localhost, [::1], https and oob are refused.
	redirectRe  = regexp.MustCompile(`^http://127\.0\.0\.1:(\d{4,5})/cb$`)
	challengeRe = regexp.MustCompile(`^[A-Za-z0-9_-]{43}$`)
	stateRe     = regexp.MustCompile(`^[\x21-\x7E]{1,128}$`)
	deviceRe    = regexp.MustCompile(`^[\p{L}\p{N} ()._-]*$`)
	methodRe    = regexp.MustCompile(`^[a-z][a-z0-9_-]{0,31}$`)
	arIDRe      = regexp.MustCompile(`^[A-Za-z0-9_-]{22}$`)
)

func random16() string {
	var b [16]byte
	if _, err := rand.Read(b[:]); err != nil {
		panic("auth: crypto/rand failed: " + err.Error())
	}
	return base64.RawURLEncoding.EncodeToString(b[:])
}

// validateAuthorize checks the authorize parameters (§5.2) and returns the reason for refusal.
func validateAuthorize(q url.Values) string {
	if q.Get("client_id") != ClientID {
		return "client_id must be " + ClientID + "."
	}
	m := redirectRe.FindStringSubmatch(q.Get("redirect_uri"))
	if m == nil {
		return "redirect_uri must be http://127.0.0.1:PORT/cb."
	}
	if port, _ := strconv.Atoi(m[1]); port < 1024 || port > 65535 {
		return "redirect_uri must use a port from 1024 to 65535."
	}
	if q.Get("code_challenge_method") != "S256" {
		return "code_challenge_method must be S256."
	}
	if !challengeRe.MatchString(q.Get("code_challenge")) {
		return "code_challenge must be 43 base64url characters."
	}
	if !stateRe.MatchString(q.Get("state")) {
		return "state must be 1 to 128 characters."
	}
	if d := q.Get("device_name"); utf8.RuneCountInString(d) > 64 || !deviceRe.MatchString(d) {
		return "device_name must be at most 64 letters, digits, spaces and ( ) . _ -."
	}
	if m := q.Get("method"); m != "" && !methodRe.MatchString(m) {
		return "method is not a sign-in method id."
	}
	return ""
}

func (s *Service) userCount(ctx context.Context) (int64, error) {
	var n int64
	err := s.Store.Read(ctx, func(tx store.ReadTx) error {
		var err error
		n, err = tx.UserCount()
		return err
	})
	return n, err
}

// authorize is GET /auth/authorize: validate, create the auth request, set the cookie and render
// the chooser. It never redirects to an unvalidated URI, and never auto-redirects to a provider.
func (s *Service) authorize(w http.ResponseWriter, r *http.Request) {
	q := r.URL.Query()
	if reason := validateAuthorize(q); reason != "" {
		s.renderError(w, r, http.StatusBadRequest, pageError{Code: "invalid_request", Title: "This sign-in link isn't valid",
			Message: reason + " Start again from ReqHarbor."})
		return
	}
	now := s.Clock.Now()
	cookie := random16()
	ar := store.AuthRequest{
		ID: random16(), BrowserHash: ids.SHA256Hex(cookie), ClientID: ClientID, RedirectURI: q.Get("redirect_uri"),
		CodeChallenge: q.Get("code_challenge"), ClientState: q.Get("state"), DeviceName: strings.TrimSpace(q.Get("device_name")),
		CreatedAt: clock.Millis(now), ExpiresAt: clock.Millis(now.Add(AuthRequestTTL)),
	}
	if m := q.Get("method"); m != "" {
		ar.MethodHint = &m
	}
	var users int64
	err := s.Store.Write(r.Context(), func(tx store.WriteTx) error {
		var err error
		if users, err = tx.UserCount(); err != nil {
			return err
		}
		return tx.InsertAuthRequest(ar)
	})
	if err != nil {
		s.internal(w, r, err)
		return
	}
	http.SetCookie(w, &http.Cookie{Name: s.cookieName, Value: cookie, Path: "/auth", MaxAge: int(AuthRequestTTL.Seconds()),
		HttpOnly: true, Secure: s.public.Scheme == "https", SameSite: http.SameSiteLaxMode})
	if users == 0 {
		d := s.arPage("Set up "+s.Config.ServerName, &ar)
		d.Setup = true
		if s.Config.AdminEmail != "" {
			// A trusted provider email equal to REQHARBOR_ADMIN_EMAIL also creates the admin (§5.6).
			d.Providers = s.providerLinks(r, &ar)
		}
		s.render(w, r, http.StatusOK, "code", d)
		return
	}
	s.renderLogin(w, r, http.StatusOK, &ar, "", "", nil)
}

func (s *Service) renderLogin(w http.ResponseWriter, r *http.Request, status int, ar *store.AuthRequest, email, msg string, pow *powData) {
	d := s.arPage("Sign in", ar)
	d.Email, d.Error, d.PoW = email, msg, pow
	d.FocusPassword = email != ""
	d.SignupLink = s.Config.Signup != config.SignupClosed && s.Config.PasswordAuth
	d.Providers = s.providerLinks(r, ar)
	if pow != nil {
		d.Script = powTag
	}
	s.render(w, r, status, "authorize", d)
}

func (s *Service) internal(w http.ResponseWriter, r *http.Request, err error) {
	s.Logger.Error("auth page failed", "err", err.Error())
	s.renderError(w, r, http.StatusInternalServerError, pageError{Code: "internal_error", Title: "Something went wrong",
		Message: "The server could not complete this step. Try again in a moment."})
}

// loadAR finds the auth request named by ar and checks the browser cookie (the login-CSRF fix),
// expiry and that it has not finished yet. It renders request_expired and returns nil otherwise.
func (s *Service) loadAR(w http.ResponseWriter, r *http.Request, id string) *store.AuthRequest {
	c, err := r.Cookie(s.cookieName)
	if err != nil || !arIDRe.MatchString(id) {
		s.renderError(w, r, http.StatusBadRequest, errRequestExpired)
		return nil
	}
	var ar *store.AuthRequest
	err = s.Store.Read(r.Context(), func(tx store.ReadTx) error {
		var err error
		ar, err = tx.AuthRequest(id)
		return err
	})
	if errors.Is(err, store.ErrNotFound) {
		s.renderError(w, r, http.StatusBadRequest, errRequestExpired)
		return nil
	}
	if err != nil {
		s.internal(w, r, err)
		return nil
	}
	now := clock.Millis(s.Clock.Now())
	if !ids.Equal(ids.SHA256Hex(c.Value), ar.BrowserHash) || ar.ExpiresAt <= now || ar.ConsumedAt != nil || ar.CodeHash != nil {
		s.renderError(w, r, http.StatusBadRequest, errRequestExpired)
		return nil
	}
	return ar
}

// signupPage is GET /auth/signup?ar=.
func (s *Service) signupPage(w http.ResponseWriter, r *http.Request) {
	ar := s.loadAR(w, r, r.URL.Query().Get("ar"))
	if ar == nil {
		return
	}
	s.renderSignup(w, r, http.StatusOK, ar, "", "", "")
}

// renderSignup shows the signup form, or the page that comes before it: the setup code while no
// user exists, signup_closed, or password sign-in turned off.
func (s *Service) renderSignup(w http.ResponseWriter, r *http.Request, status int, ar *store.AuthRequest, name, email, msg string) {
	links := s.providerLinks(r, ar)
	if !s.Config.PasswordAuth && len(links) == 0 {
		s.renderError(w, r, http.StatusForbidden, errPasswordOff)
		return
	}
	grant, err := s.grantOf(r.Context(), ar)
	if err != nil {
		s.internal(w, r, err)
		return
	}
	users, err := s.userCount(r.Context())
	if err != nil {
		s.internal(w, r, err)
		return
	}
	setup := grant != nil && grant.Purpose == store.PurposeSetup
	if users == 0 && !setup {
		d := s.arPage("Set up "+s.Config.ServerName, ar)
		d.Setup, d.Error = true, msg
		s.render(w, r, status, "code", d)
		return
	}
	if users > 0 && grant == nil && s.Config.Signup == config.SignupClosed {
		s.renderError(w, r, http.StatusForbidden, errSignupClosed)
		return
	}
	d := s.arPage("Create account", ar)
	d.Setup = setup
	d.NeedInvite = users > 0 && grant == nil && s.Config.Signup == config.SignupInvite
	d.Name, d.Email, d.Error = name, email, msg
	d.Providers = links
	s.render(w, r, status, "signup", d)
}

func (s *Service) grantOf(ctx context.Context, ar *store.AuthRequest) (*store.OneTimeToken, error) {
	if ar.GrantTokenHash == nil {
		return nil, nil
	}
	var t *store.OneTimeToken
	err := s.Store.Read(ctx, func(tx store.ReadTx) error {
		var err error
		t, err = tx.OneTimeToken(*ar.GrantTokenHash)
		return err
	})
	if errors.Is(err, store.ErrNotFound) {
		return nil, nil
	}
	return t, err
}

type codeResult int

const (
	codeOK codeResult = iota
	codeInvalid
	codeTooMany
	codeRateLimited
)

// applyCode checks an entered setup or invite code against this auth request and stores its hash
// as the request's grant. Entry is limited to 10 tries per auth request and 30 per hour per IP.
func (s *Service) applyCode(r *http.Request, ar *store.AuthRequest, code string) (codeResult, error) {
	if ok, _ := s.CodeLimiter.Allow("code:" + s.bucket(r)); !ok {
		return codeRateLimited, nil
	}
	hash := accounts.CodeHash(code)
	now := clock.Millis(s.Clock.Now())
	result := codeInvalid
	err := s.Store.Write(r.Context(), func(tx store.WriteTx) error {
		n, err := tx.IncrAuthRequestAttempts(ar.ID, "code_attempts")
		if err != nil {
			return err
		}
		if n > MaxCodeAttempts {
			result = codeTooMany
			return nil
		}
		if accounts.NormalizeCode(code) == "" {
			return nil
		}
		t, err := tx.OneTimeToken(hash)
		if errors.Is(err, store.ErrNotFound) {
			return nil
		}
		if err != nil {
			return err
		}
		users, err := tx.UserCount()
		if err != nil {
			return err
		}
		valid := t.Usable(now) && ((t.Purpose == store.PurposeSetup && users == 0) || (t.Purpose == store.PurposeSignupInvite && users > 0))
		if !valid {
			return nil
		}
		result = codeOK
		return tx.SetAuthRequestGrant(ar.ID, hash)
	})
	if err == nil && result == codeOK {
		ar.GrantTokenHash = &hash
	}
	return result, err
}

// code is POST /auth/code.
func (s *Service) code(w http.ResponseWriter, r *http.Request) {
	ar := s.loadAR(w, r, r.PostForm.Get("ar"))
	if ar == nil {
		return
	}
	// continue names the provider sign-in that asked for the code (S5); it resumes there.
	cont := r.PostForm.Get("continue")
	if _, ok := s.providerBy[cont]; !ok {
		cont = ""
	}
	res, err := s.applyCode(r, ar, r.PostForm.Get("code"))
	switch {
	case err != nil:
		s.internal(w, r, err)
	case res == codeRateLimited:
		s.renderRateLimited(w, r, 0)
	case res == codeTooMany:
		s.renderError(w, r, http.StatusTooManyRequests, errTooManyAttempts)
	case res == codeInvalid:
		users, err := s.userCount(r.Context())
		if err != nil {
			s.internal(w, r, err)
			return
		}
		d := s.arPage("Enter code", ar)
		d.Setup, d.Error, d.Continue = users == 0, msgInviteInvalid, cont
		s.render(w, r, http.StatusBadRequest, "code", d)
	case cont != "":
		redirectSeeOther(w, "/auth/start/"+cont+"?ar="+ar.ID)
	default:
		redirectSeeOther(w, "/auth/signup?ar="+ar.ID)
	}
}

// issueCode mints the 60-second authorization code on the auth request and returns the loopback
// redirect (§5.2).
func (s *Service) issueCode(tx store.WriteTx, ar *store.AuthRequest, userID string) (string, error) {
	code := ids.Token("")
	exp := clock.Millis(s.Clock.Now().Add(CodeTTL))
	if err := tx.SetAuthRequestCode(ar.ID, userID, ids.SHA256Hex(code), exp); err != nil {
		return "", err
	}
	return ar.RedirectURI + "?code=" + url.QueryEscape(code) + "&state=" + url.QueryEscape(ar.ClientState), nil
}

func redirectSeeOther(w http.ResponseWriter, target string) {
	w.Header().Set("Location", target)
	w.WriteHeader(http.StatusSeeOther)
}

// passwordLogin is POST /auth/password/login (§5.5 order: origin and CSRF; the per-IP and
// per-auth-request limits; the (email, IP bucket) throttle; the account-wide proof of work; the
// pair throttle again, atomically recording the attempt; then argon2, real or dummy). Known and
// unknown emails take the identical path.
func (s *Service) passwordLogin(w http.ResponseWriter, r *http.Request) {
	ar := s.loadAR(w, r, r.PostForm.Get("ar"))
	if ar == nil {
		return
	}
	if !s.Config.PasswordAuth {
		s.renderError(w, r, http.StatusForbidden, errPasswordOff)
		return
	}
	bucket := s.bucket(r)
	if ok, retry := s.LoginLimiter.Allow("login:" + bucket); !ok {
		s.renderRateLimited(w, r, retry)
		return
	}
	var attempts int64
	if err := s.Store.Write(r.Context(), func(tx store.WriteTx) error {
		var err error
		attempts, err = tx.IncrAuthRequestAttempts(ar.ID, "password_attempts")
		return err
	}); err != nil {
		s.internal(w, r, err)
		return
	}
	if attempts > MaxPasswordAttempts {
		s.renderError(w, r, http.StatusTooManyRequests, errTooManyAttempts)
		return
	}
	email := accounts.NormalizeEmail(r.PostForm.Get("email"))
	password := r.PostForm.Get("password")
	if len(email) > accounts.MaxEmailBytes || len(password) > 4*accounts.MaxPasswordRunes {
		s.renderLogin(w, r, http.StatusBadRequest, ar, "", msgBadLogin, nil)
		return
	}

	var dec throttle.Decision
	var user *store.User
	var phc string
	err := s.Store.Read(r.Context(), func(tx store.ReadTx) error {
		var err error
		if dec, err = s.Throttle.Check(tx, email, bucket); err != nil {
			return err
		}
		u, err := tx.UserByEmail(email)
		if errors.Is(err, store.ErrNotFound) {
			return nil
		}
		if err != nil {
			return err
		}
		p, err := tx.PasswordPHC(u.ID)
		if errors.Is(err, store.ErrNotFound) {
			return nil // a provider-only account behaves like an unknown email here
		}
		if err != nil {
			return err
		}
		user, phc = u, p
		return nil
	})
	if err != nil {
		s.internal(w, r, err)
		return
	}
	if dec.Locked {
		s.renderLogin(w, r, http.StatusBadRequest, ar, email, throttledMessage(dec.RetryMinutes()), nil)
		return
	}
	if dec.Difficulty > 0 && !s.Throttle.VerifyProof(ar.ID, email, r.PostForm.Get("pow"), dec.Difficulty) {
		pow := &powData{Challenge: s.Throttle.NewChallenge(ar.ID, email, dec.Difficulty), Difficulty: dec.Difficulty}
		s.renderLogin(w, r, http.StatusBadRequest, ar, email, "There were many failed sign-ins for this email. Wait while your browser finishes a check, then enter your password again.", pow)
		return
	}

	// The pair decision again, now atomic with counting this attempt (Throttle.Begin): Check ran
	// in a read transaction, so concurrent attempts could all have passed it.
	if err := s.Store.Write(r.Context(), func(tx store.WriteTx) error {
		var err error
		dec, err = s.Throttle.Begin(tx, email, bucket)
		return err
	}); err != nil {
		s.internal(w, r, err)
		return
	}
	if dec.Locked {
		s.renderLogin(w, r, http.StatusBadRequest, ar, email, throttledMessage(dec.RetryMinutes()), nil)
		return
	}

	ok, rehash := false, false
	if user != nil {
		ok, rehash = s.Hasher.Verify(phc, password)
	} else {
		s.Hasher.VerifyDummy(password)
	}
	if !ok {
		s.Throttle.Failed(email)
		s.renderLogin(w, r, http.StatusBadRequest, ar, email, msgBadLogin, nil)
		return
	}
	if user.DisabledAt != nil {
		// The password was right, so the attempt Begin counted is undone as for a sign-in.
		if err := s.Store.Write(r.Context(), func(tx store.WriteTx) error { return s.Throttle.Succeed(tx, email, bucket) }); err != nil {
			s.internal(w, r, err)
			return
		}
		s.renderLogin(w, r, http.StatusForbidden, ar, email, msgDisabled, nil)
		return
	}
	var newPHC string
	if rehash {
		if newPHC, err = s.Hasher.Hash(password); err != nil {
			s.internal(w, r, err)
			return
		}
	}
	var target string
	err = s.Store.Write(r.Context(), func(tx store.WriteTx) error {
		if err := s.Throttle.Succeed(tx, email, bucket); err != nil {
			return err
		}
		if newPHC != "" {
			if err := tx.SetPassword(user.ID, newPHC, clock.Millis(s.Clock.Now())); err != nil {
				return err
			}
		}
		var err error
		target, err = s.issueCode(tx, ar, user.ID)
		return err
	})
	if err != nil {
		s.internal(w, r, err)
		return
	}
	redirectSeeOther(w, target)
}

// passwordSignup is POST /auth/password/signup (§5.5 order: origin and CSRF; limits; the invite
// code when signup is invite; REQHARBOR_MAX_USERS; then the email). Without SMTP (M1) the account
// is created at once with email_trusted=0.
func (s *Service) passwordSignup(w http.ResponseWriter, r *http.Request) {
	ar := s.loadAR(w, r, r.PostForm.Get("ar"))
	if ar == nil {
		return
	}
	if !s.Config.PasswordAuth {
		s.renderError(w, r, http.StatusForbidden, errPasswordOff)
		return
	}
	if ok, retry := s.SignupLimiter.Allow("signup:" + s.bucket(r)); !ok {
		s.renderRateLimited(w, r, retry)
		return
	}
	name := strings.TrimSpace(r.PostForm.Get("name"))
	email := accounts.NormalizeEmail(r.PostForm.Get("email"))
	password := r.PostForm.Get("password")
	again := func(status int, msg string) { s.renderSignup(w, r, status, ar, name, email, msg) }

	if code := r.PostForm.Get("code"); strings.TrimSpace(code) != "" {
		res, err := s.applyCode(r, ar, code)
		switch {
		case err != nil:
			s.internal(w, r, err)
			return
		case res == codeRateLimited:
			s.renderRateLimited(w, r, 0)
			return
		case res == codeTooMany:
			s.renderError(w, r, http.StatusTooManyRequests, errTooManyAttempts)
			return
		case res == codeInvalid:
			again(http.StatusBadRequest, msgInviteInvalid)
			return
		}
	}
	if utf8.RuneCountInString(name) > accounts.MaxNameRunes {
		again(http.StatusBadRequest, "Use at most 100 characters for your name.")
		return
	}
	if !accounts.ValidEmail(email) {
		again(http.StatusBadRequest, msgBadEmail)
		return
	}
	if p := accounts.PasswordProblem(password, email); p != "" {
		again(http.StatusBadRequest, p)
		return
	}
	if s.Growth != nil {
		if err := s.Growth(); err != nil {
			s.renderError(w, r, http.StatusInsufficientStorage, errServerFull)
			return
		}
	}
	phc, err := s.Hasher.Hash(password)
	if err != nil {
		s.internal(w, r, err)
		return
	}
	grant := ""
	if ar.GrantTokenHash != nil {
		grant = *ar.GrantTokenHash
	}
	var target string
	err = s.Store.Write(r.Context(), func(tx store.WriteTx) error {
		created, err := accounts.Create(tx, accounts.CreateParams{
			Email: email, Name: name, EmailTrusted: false, PHC: phc, GrantHash: grant,
			SignupMode: s.Config.Signup, AdminEmail: s.Config.AdminEmail, MaxUsers: s.Config.MaxUsers, OpenSignupFull: s.openSignupFull(), Now: s.Clock.Now(),
		})
		if err != nil {
			return err
		}
		target, err = s.issueCode(tx, ar, created.User.ID)
		return err
	})
	switch {
	case err == nil:
		if s.UserCreated != nil {
			s.UserCreated()
		}
		redirectSeeOther(w, target)
	case errors.Is(err, accounts.ErrEmailInUse):
		again(http.StatusConflict, msgEmailInUse)
	case errors.Is(err, accounts.ErrInviteInvalid):
		again(http.StatusBadRequest, msgInviteInvalid)
	case errors.Is(err, accounts.ErrInviteRequired):
		again(http.StatusBadRequest, msgInviteRequired)
	case errors.Is(err, accounts.ErrSetupRequired):
		again(http.StatusBadRequest, msgSetupRequired)
	case errors.Is(err, accounts.ErrUserLimit):
		s.renderError(w, r, http.StatusForbidden, errUserLimit)
	case errors.Is(err, accounts.ErrSignupClosed):
		s.renderError(w, r, http.StatusForbidden, errSignupClosed)
	default:
		s.internal(w, r, err)
	}
}

// done is GET /auth/done, where the app's loopback listener sends the browser.
func (s *Service) done(w http.ResponseWriter, r *http.Request) {
	s.render(w, r, http.StatusOK, "done", s.base("Signed in"))
}

// forgot is /auth/forgot without SMTP: it takes no email (§5.7).
func (s *Service) forgot(w http.ResponseWriter, r *http.Request) {
	s.render(w, r, http.StatusOK, "forgot", s.base("Forgot password"))
}

func (s *Service) resetPage(w http.ResponseWriter, r *http.Request) {
	d := s.base("Reset password")
	d.Script = resetTag
	s.render(w, r, http.StatusOK, "reset", d)
}

// reset is POST /auth/reset: a single-use reset token sets the password, deletes every throttle
// row of the email and revokes every session of the user. A CLI link never sets email_trusted.
func (s *Service) reset(w http.ResponseWriter, r *http.Request) {
	token := strings.TrimSpace(r.PostForm.Get("t"))
	password := r.PostForm.Get("password")
	fail := func(status int, msg string) {
		d := s.base("Reset password")
		d.Script, d.Token, d.Error = resetTag, token, msg
		s.render(w, r, status, "reset", d)
	}
	const msgBadLink = "This reset link is invalid, was already used or has expired. Ask your server admin for a new one."
	if token == "" || len(token) > 128 {
		fail(http.StatusBadRequest, msgBadLink)
		return
	}
	hash := ids.SHA256Hex(token)
	now := clock.Millis(s.Clock.Now())
	var user *store.User
	err := s.Store.Read(r.Context(), func(tx store.ReadTx) error {
		t, err := tx.OneTimeToken(hash)
		if err != nil {
			return err
		}
		if t.Purpose != store.PurposeResetPassword || t.UserID == nil || !t.Usable(now) {
			return store.ErrNotFound
		}
		user, err = tx.UserByID(*t.UserID)
		return err
	})
	if errors.Is(err, store.ErrNotFound) {
		fail(http.StatusBadRequest, msgBadLink)
		return
	}
	if err != nil {
		s.internal(w, r, err)
		return
	}
	if p := accounts.PasswordProblem(password, user.Email); p != "" {
		fail(http.StatusBadRequest, p)
		return
	}
	phc, err := s.Hasher.Hash(password)
	if err != nil {
		s.internal(w, r, err)
		return
	}
	used := false
	err = s.Store.Write(r.Context(), func(tx store.WriteTx) error {
		ok, err := tx.UseOneTimeToken(hash, now)
		if err != nil || !ok {
			return err
		}
		used = true
		if err := tx.SetPassword(user.ID, phc, now); err != nil {
			return err
		}
		if _, err := tx.RevokeUserSessions(user.ID, now); err != nil {
			return err
		}
		return s.Throttle.Clear(tx, user.Email)
	})
	if err != nil {
		s.internal(w, r, err)
		return
	}
	if !used {
		fail(http.StatusBadRequest, msgBadLink)
		return
	}
	d := s.base("Password changed")
	d.Done = true
	s.render(w, r, http.StatusOK, "reset", d)
}
