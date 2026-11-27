package auth

// External providers on the hosted pages (06-go-server.md §5.2 (b), §5.4): GET /auth/start/{p}
// starts the upstream round trip (state and PKCE stored on the auth request), GET
// /auth/callback/{p} finishes it, and POST /auth/dev is the dev provider's form (devauth builds
// only). Account resolution (AC-AUTH-6) is shared by every provider.

import (
	"context"
	"crypto/rand"
	"encoding/base64"
	"errors"
	"html/template"
	"net/http"
	"net/url"
	"strings"
	"time"

	"github.com/mihajlopopara/reqharbor/server/internal/accounts"
	"github.com/mihajlopopara/reqharbor/server/internal/auth/providers"
	"github.com/mihajlopopara/reqharbor/server/internal/clock"
	"github.com/mihajlopopara/reqharbor/server/internal/httpx"
	"github.com/mihajlopopara/reqharbor/server/internal/ids"
	"github.com/mihajlopopara/reqharbor/server/internal/store"
)

// ExchangeTimeout bounds the whole provider exchange (token plus profile calls).
const ExchangeTimeout = 30 * time.Second

// pageLink is a link or button on a hosted page.
type pageLink struct {
	Href    string
	Text    string
	Primary bool
}

// verifier returns a PKCE verifier: base64url of 32 random bytes (43 characters, RFC 7636).
func verifier() string {
	var b [32]byte
	if _, err := rand.Read(b[:]); err != nil {
		panic("auth: crypto/rand failed: " + err.Error())
	}
	return base64.RawURLEncoding.EncodeToString(b[:])
}

func (s *Service) devProvider() providers.Form {
	if p, ok := s.providerBy["dev"].(providers.Form); ok {
		return p
	}
	return nil
}

// providerRoute answers 404 for a provider that is not enabled, and for a form provider whose
// guard refuses the request (the dev provider from a non-loopback peer), before anything else
// runs: no canonical-origin redirect, no page, no cookie.
func (s *Service) providerRoute(h http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		p, ok := s.providerBy[r.PathValue("provider")]
		if !ok {
			httpx.NotFoundHandler.ServeHTTP(w, r)
			return
		}
		if f, isForm := p.(providers.Form); isForm && !f.Allow(r) {
			httpx.NotFoundHandler.ServeHTTP(w, r)
			return
		}
		h.ServeHTTP(w, r)
	})
}

// formRoute guards a form provider's own route the same way.
func (s *Service) formRoute(f providers.Form, h http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if !f.Allow(r) {
			httpx.NotFoundHandler.ServeHTTP(w, r)
			return
		}
		h.ServeHTTP(w, r)
	})
}

// providerLinks are the provider buttons for one auth request. The method hint makes its
// provider's button primary; without a hint every provider button is primary. A form provider
// appears only when its guard allows this request.
func (s *Service) providerLinks(r *http.Request, ar *store.AuthRequest) []pageLink {
	hint := ""
	if ar.MethodHint != nil {
		hint = *ar.MethodHint
	}
	var out []pageLink
	for _, p := range s.providers {
		text := "Continue with " + p.Label()
		if f, ok := p.(providers.Form); ok {
			if !f.Allow(r) {
				continue
			}
			text = p.Label() + " (tests only)"
		}
		out = append(out, pageLink{Href: "/auth/start/" + p.ID() + "?ar=" + ar.ID, Text: text, Primary: hint == "" || hint == p.ID()})
	}
	return out
}

// start is GET /auth/start/{provider}?ar=. A redirect provider gets a fresh upstream state and
// PKCE verifier on the auth request, then a 302 to the identity provider; a form provider renders
// its form.
func (s *Service) start(w http.ResponseWriter, r *http.Request) {
	ar := s.loadAR(w, r, r.URL.Query().Get("ar"))
	if ar == nil {
		return
	}
	switch p := s.providerBy[r.PathValue("provider")].(type) {
	case providers.Form:
		s.renderForm(w, r, http.StatusOK, p, ar, "", "", "")
	case providers.Redirector:
		state, v := random16(), verifier()
		err := s.Store.Write(r.Context(), func(tx store.WriteTx) error {
			return tx.SetAuthRequestUpstream(ar.ID, p.ID(), ids.SHA256Hex(state), v)
		})
		if errors.Is(err, store.ErrNotFound) {
			s.renderError(w, r, http.StatusBadRequest, errRequestExpired)
			return
		}
		if err != nil {
			s.internal(w, r, err)
			return
		}
		w.Header().Set("Cache-Control", "no-store")
		w.Header().Set("Location", p.AuthURL(state, v))
		w.WriteHeader(http.StatusFound)
	default:
		httpx.NotFoundHandler.ServeHTTP(w, r)
	}
}

var errStaleAR = errors.New("auth request does not match")

// callback is GET /auth/callback/{provider}?code&state (or ?error=…&state). The upstream state
// finds the auth request, which must belong to this browser's cookie; the state then works only
// once. access_denied goes back to the app's validated loopback URI as error=access_denied.
func (s *Service) callback(w http.ResponseWriter, r *http.Request) {
	p, ok := s.providerBy[r.PathValue("provider")].(providers.Redirector)
	if !ok {
		httpx.NotFoundHandler.ServeHTTP(w, r)
		return
	}
	q := r.URL.Query()
	state := q.Get("state")
	c, err := r.Cookie(s.cookieName)
	if err != nil || state == "" || len(state) > 256 {
		s.renderError(w, r, http.StatusBadRequest, errRequestExpired)
		return
	}
	now := clock.Millis(s.Clock.Now())
	var ar *store.AuthRequest
	err = s.Store.Write(r.Context(), func(tx store.WriteTx) error {
		a, err := tx.AuthRequestByUpstreamState(ids.SHA256Hex(state))
		if err != nil {
			return err
		}
		if !ids.Equal(ids.SHA256Hex(c.Value), a.BrowserHash) || a.ExpiresAt <= now || a.ConsumedAt != nil || a.CodeHash != nil ||
			a.Provider == nil || *a.Provider != p.ID() || a.UpstreamVerifier == nil {
			return errStaleAR
		}
		ar = a
		return tx.ClearAuthRequestUpstream(a.ID)
	})
	switch {
	case errors.Is(err, store.ErrNotFound), errors.Is(err, errStaleAR):
		s.renderError(w, r, http.StatusBadRequest, errRequestExpired)
		return
	case err != nil:
		s.internal(w, r, err)
		return
	}

	if e := q.Get("error"); e != "" {
		if e == "access_denied" {
			// The app shows "Sign-in was cancelled in the browser." The URI was validated at
			// authorize time, so this redirect is safe.
			if err := s.Store.Write(r.Context(), func(tx store.WriteTx) error { return tx.ConsumeAuthRequest(ar.ID, now) }); err != nil {
				s.internal(w, r, err)
				return
			}
			redirectSeeOther(w, ar.RedirectURI+"?error=access_denied&state="+url.QueryEscape(ar.ClientState))
			return
		}
		s.Logger.Warn("provider returned an error", "provider", p.ID(), "error", providers.SafeCode(e))
		s.renderError(w, r, http.StatusBadRequest, providerError(p))
		return
	}
	code := q.Get("code")
	if code == "" || len(code) > 512 {
		s.renderError(w, r, http.StatusBadRequest, providerError(p))
		return
	}
	ctx, cancel := context.WithTimeout(r.Context(), ExchangeTimeout)
	defer cancel()
	prof, err := p.Exchange(ctx, code, *ar.UpstreamVerifier)
	if err != nil {
		s.Logger.Warn("provider sign-in failed", "provider", p.ID(), "reason", providers.ErrorCode(err))
		s.renderError(w, r, http.StatusBadGateway, providerError(p))
		return
	}
	s.finishProvider(w, r, ar, p, prof)
}

// devSubmit is POST /auth/dev: the dev provider's form (any email and name, plus an optional
// invite or setup code when the server needs one).
func (s *Service) devSubmit(w http.ResponseWriter, r *http.Request) {
	dev := s.devProvider()
	ar := s.loadAR(w, r, r.PostForm.Get("ar"))
	if ar == nil {
		return
	}
	email, name := strings.TrimSpace(r.PostForm.Get("email")), strings.TrimSpace(r.PostForm.Get("name"))
	prof, msg := dev.FromForm(r.PostForm)
	if msg != "" {
		s.renderForm(w, r, http.StatusBadRequest, dev, ar, email, name, msg)
		return
	}
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
			s.renderForm(w, r, http.StatusBadRequest, dev, ar, email, name, msgInviteInvalid)
			return
		}
	}
	s.finishProvider(w, r, ar, dev, prof)
}

func providerError(p providers.Provider) pageError {
	return pageError{Code: "provider_error", Title: "Sign-in didn't finish",
		Message: p.Label() + " sign-in didn't finish. Go back to ReqHarbor and click Sign in again."}
}

// emailInUse describes the account that already owns an incoming email.
type emailInUse struct {
	method  string // its sign-in method label
	trusted bool   // its own email is trusted
}

// finishProvider resolves the account for a provider profile (§5.4 account resolution) and, on
// success, mints the code and sends the browser to the app's loopback URI.
//
//   - Returning user (a hit on identities(provider, issuer, subject); never matched by email):
//     name and avatar are refreshed, users.email changes only to a trusted email no other user
//     owns (email_trusted becomes 1), an untrusted email is only recorded on the identity, and
//     is_admin is never touched.
//   - New identity without an email: no_verified_email. With an email another account uses:
//     email_in_use. Nothing is created in either case.
//   - Otherwise the signup rules of accounts.Create apply, with email_trusted from the provider.
func (s *Service) finishProvider(w http.ResponseWriter, r *http.Request, ar *store.AuthRequest, p providers.Provider, prof *providers.Profile) {
	now := s.Clock.Now()
	ms := clock.Millis(now)
	var avatar *string
	if prof.AvatarURL != "" {
		a := prof.AvatarURL
		avatar = &a
	}

	returning, disabled := false, false
	var target string
	err := s.Store.Write(r.Context(), func(tx store.WriteTx) error {
		id, err := tx.IdentityBySubject(prof.Provider, prof.Issuer, prof.Subject)
		if errors.Is(err, store.ErrNotFound) {
			return nil
		}
		if err != nil {
			return err
		}
		returning = true
		u, err := tx.UserByID(id.UserID)
		if err != nil {
			return err
		}
		if u.DisabledAt != nil {
			disabled = true
			return nil
		}
		if err := tx.TouchIdentity(prof.Provider, prof.Issuer, prof.Subject, prof.Email, ms); err != nil {
			return err
		}
		name := u.Name
		if prof.Name != "" {
			name = prof.Name
		}
		if err := tx.SetUserProfile(u.ID, name, avatar, ms); err != nil {
			return err
		}
		if prof.EmailTrusted && prof.Email != "" {
			if prof.Email == u.Email {
				if !u.EmailTrusted {
					if err := tx.SetUserEmail(u.ID, u.Email, true, ms); err != nil {
						return err
					}
				}
			} else if _, err := tx.UserByEmail(prof.Email); errors.Is(err, store.ErrNotFound) {
				if err := tx.SetUserEmail(u.ID, prof.Email, true, ms); err != nil {
					return err
				}
			} else if err != nil {
				return err
			}
		}
		target, err = s.issueCode(tx, ar, u.ID)
		return err
	})
	switch {
	case err != nil:
		s.internal(w, r, err)
		return
	case disabled:
		s.renderError(w, r, http.StatusForbidden, pageError{Code: "account_disabled", Title: "Account disabled", Message: msgDisabled})
		return
	case returning:
		redirectSeeOther(w, target)
		return
	}

	// A new identity.
	if prof.Email == "" {
		d := s.arPage("No verified email", ar)
		d.Message = "Your " + p.Label() + " account has no verified email. Add one and try again."
		d.Links = []pageLink{{Href: "/auth/start/" + p.ID() + "?ar=" + ar.ID, Text: "Try again"}}
		s.render(w, r, http.StatusBadRequest, "error", d)
		return
	}
	if ok, retry := s.SignupLimiter.Allow("signup:" + s.bucket(r)); !ok {
		s.renderRateLimited(w, r, retry)
		return
	}
	if s.Growth != nil {
		if err := s.Growth(); err != nil {
			s.renderError(w, r, http.StatusInsufficientStorage, errServerFull)
			return
		}
	}
	grant := ""
	if ar.GrantTokenHash != nil {
		grant = *ar.GrantTokenHash
	}
	var inUse *emailInUse
	err = s.Store.Write(r.Context(), func(tx store.WriteTx) error {
		if u, err := tx.UserByEmail(prof.Email); err == nil {
			label, err := accounts.MethodLabel(tx, u.ID)
			if err != nil {
				return err
			}
			inUse = &emailInUse{method: label, trusted: u.EmailTrusted}
			return nil
		} else if !errors.Is(err, store.ErrNotFound) {
			return err
		}
		created, err := accounts.Create(tx, accounts.CreateParams{
			Email: prof.Email, Name: prof.Name, AvatarURL: avatar, EmailTrusted: prof.EmailTrusted,
			Identity:  &store.Identity{Provider: prof.Provider, Issuer: prof.Issuer, Subject: prof.Subject, Email: prof.Email},
			GrantHash: grant, SignupMode: s.Config.Signup, AdminEmail: s.Config.AdminEmail, MaxUsers: s.Config.MaxUsers, OpenSignupFull: s.openSignupFull(), Now: now,
		})
		if err != nil {
			return err
		}
		target, err = s.issueCode(tx, ar, created.User.ID)
		return err
	})
	_, isForm := p.(providers.Form)
	codePage := func(setup bool, msg string) {
		if isForm {
			s.renderForm(w, r, http.StatusBadRequest, p.(providers.Form), ar, prof.Email, prof.Name, msg)
			return
		}
		d := s.arPage("Enter code", ar)
		d.Setup, d.Error, d.Continue = setup, msg, p.ID()
		s.render(w, r, http.StatusBadRequest, "code", d)
	}
	switch {
	case err == nil && inUse != nil:
		s.renderEmailInUse(w, r, ar, prof, inUse)
	case err == nil:
		if s.UserCreated != nil {
			s.UserCreated()
		}
		redirectSeeOther(w, target)
	case errors.Is(err, accounts.ErrEmailInUse):
		s.renderEmailInUse(w, r, ar, &providers.Profile{}, &emailInUse{})
	case errors.Is(err, accounts.ErrInviteRequired):
		codePage(false, msgInviteRequired)
	case errors.Is(err, accounts.ErrInviteInvalid):
		codePage(s.noUsers(r.Context()), msgInviteInvalid)
	case errors.Is(err, accounts.ErrSetupRequired):
		codePage(true, msgSetupRequired)
	case errors.Is(err, accounts.ErrUserLimit):
		s.renderError(w, r, http.StatusForbidden, errUserLimit)
	case errors.Is(err, accounts.ErrSignupClosed):
		s.renderError(w, r, http.StatusForbidden, errSignupClosed)
	default:
		s.internal(w, r, err)
	}
}

// noUsers reports whether the server still has no account (the code page then asks for the
// setup code).
func (s *Service) noUsers(ctx context.Context) bool {
	n, err := s.userCount(ctx)
	return err == nil && n == 0
}

// renderEmailInUse is the email_in_use page (§5.4). Only when the incoming email is trusted (the
// visitor just proved control of it) does the page show the masked address and the existing
// method, and, when the existing account's own email is untrusted (a possible squatter), point
// to Forgot password. Otherwise it says only that the email is taken. Nothing was created.
func (s *Service) renderEmailInUse(w http.ResponseWriter, r *http.Request, ar *store.AuthRequest, prof *providers.Profile, e *emailInUse) {
	d := s.arPage("Email already in use", ar)
	d.Links = []pageLink{{Href: "/auth/authorize?" + string(d.ResumeQuery), Text: "Back to sign in"}}
	if prof.EmailTrusted && prof.Email != "" && e.method != "" {
		d.Message = accounts.MaskEmail(prof.Email) + " already signs in with " + e.method + ". Sign in with " + e.method + " instead."
		if !e.trusted {
			d.Hint = "If this is your address, use Forgot password to claim the account."
			d.Links = append(d.Links, pageLink{Href: "/auth/forgot", Text: "Forgot password?"})
		}
	} else {
		d.Message = "This email is already used by another account. Ask your server admin."
	}
	s.render(w, r, http.StatusConflict, "error", d)
}

// renderForm renders a form provider's page.
func (s *Service) renderForm(w http.ResponseWriter, r *http.Request, status int, f providers.Form, ar *store.AuthRequest, email, name, msg string) {
	d := s.arPage(f.Label(), ar)
	d.Email, d.Name, d.Error = email, name, msg
	s.renderTemplate(w, r, status, s.formPage(f), d)
}

// formPage parses a form provider's template with the layout (once per provider).
func (s *Service) formPage(f providers.Form) *template.Template {
	s.formPagesMu.Lock()
	defer s.formPagesMu.Unlock()
	if t, ok := s.formPages[f.ID()]; ok {
		return t
	}
	t := template.Must(template.Must(template.New(f.ID()).ParseFS(webFiles, "templates/layout.html")).Parse(f.Template()))
	if s.formPages == nil {
		s.formPages = map[string]*template.Template{}
	}
	s.formPages[f.ID()] = t
	return t
}
