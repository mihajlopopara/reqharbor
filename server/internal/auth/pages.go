package auth

import (
	"bytes"
	"fmt"
	"html/template"
	"net/http"
	"net/url"
	"strconv"
	"time"

	"github.com/mihajlopopara/reqharbor/server/internal/httpx"
	"github.com/mihajlopopara/reqharbor/server/internal/store"
	"github.com/mihajlopopara/reqharbor/server/internal/web"
)

var webFiles = web.Files

var pageNames = []string{"authorize", "code", "signup", "done", "error", "forgot", "reset", "verify"}

var pages = func() map[string]*template.Template {
	m := make(map[string]*template.Template, len(pageNames))
	for _, n := range pageNames {
		m[n] = template.Must(template.New(n).ParseFS(web.Files, "templates/layout.html", "templates/"+n+".html"))
	}
	return m
}()

var (
	styleTag = template.HTML("<style>" + web.AuthCSS + "</style>")   //nolint:gosec // embedded, hashed
	resetTag = template.HTML("<script>" + web.ResetJS + "</script>") //nolint:gosec // embedded, hashed
	powTag   = template.HTML("<script>" + web.PowJS + "</script>")   //nolint:gosec // embedded, hashed
	logoTag  = template.HTML(web.LogoSVG)                            //nolint:gosec // embedded
)

type powData struct {
	Challenge  string
	Difficulty int
}

// pageData is the data of every hosted page; each template uses the fields it needs.
type pageData struct {
	Title, ServerName string
	Insecure          bool
	Style, Logo       template.HTML
	Script            template.HTML

	AR, Device, Started string
	ResumeQuery         template.URL
	Error               string
	Email, Name         string
	FocusPassword       bool
	PasswordAuth        bool
	SignupLink          bool
	Setup, NeedInvite   bool
	PoW                 *powData

	Message, Hint string
	Token         string
	Done          bool

	// Email (S9).
	Notice      string // a status line, such as "We sent a new code."
	EmailReset  bool   // /auth/forgot takes an email (SMTP configured)
	AdminSignup bool   // a fresh server offers the password signup for REQHARBOR_ADMIN_EMAIL

	// External providers (S5).
	Providers []pageLink // buttons for the enabled providers
	Continue  string     // the provider to resume after a code was entered
	Links     []pageLink // extra links on an error page
}

type pageError struct {
	Code           string
	Title, Message string
	Hint           string
}

func (s *Service) base(title string) pageData {
	return pageData{Title: title, ServerName: s.Config.ServerName, Insecure: s.public.Scheme == "http",
		Style: styleTag, Logo: logoTag}
}

// render writes a hosted page with the /auth/* CSP, noindex and no-store.
func (s *Service) render(w http.ResponseWriter, r *http.Request, status int, name string, d pageData) {
	s.renderTemplate(w, r, status, pages[name], d)
}

func (s *Service) renderTemplate(w http.ResponseWriter, r *http.Request, status int, t *template.Template, d pageData) {
	var buf bytes.Buffer
	if err := t.ExecuteTemplate(&buf, "layout", d); err != nil {
		httpx.HandleError(w, r, fmt.Errorf("render %s: %w", t.Name(), err))
		return
	}
	h := w.Header()
	h.Set("Content-Type", "text/html; charset=utf-8")
	h.Set("Content-Security-Policy", web.AuthCSP)
	h.Set("X-Robots-Tag", "noindex")
	h.Set("Cache-Control", "no-store")
	h.Set("Content-Length", strconv.Itoa(buf.Len()))
	w.WriteHeader(status)
	_, _ = w.Write(buf.Bytes())
}

func (s *Service) renderError(w http.ResponseWriter, r *http.Request, status int, e pageError) {
	d := s.base(e.Title)
	d.Message, d.Hint = e.Message, e.Hint
	s.render(w, r, status, "error", d)
}

func (s *Service) renderRateLimited(w http.ResponseWriter, r *http.Request, retry time.Duration) {
	w.Header().Set("Retry-After", strconv.FormatInt(max(1, retrySeconds(retry)), 10))
	s.renderError(w, r, http.StatusTooManyRequests, pageError{Code: "rate_limited", Title: "Too many requests",
		Message: "Too many requests came from your network. Wait a minute and try again."})
}

// Page errors with fixed wording (§4 hosted page codes).
var (
	errRequestExpired = pageError{Code: "request_expired", Title: "This sign-in has expired",
		Message: "This sign-in request has expired, was already used, or was opened in a different browser. Go back to ReqHarbor and click Sign in again."}
	errSignupClosed = pageError{Code: "signup_closed", Title: "Signup is closed",
		Message: "This server isn't accepting new accounts. Ask its admin."}
	errUserLimit = pageError{Code: "server_user_limit", Title: "No new accounts",
		Message: "This server isn't accepting new accounts. Ask its admin."}
	errPasswordOff = pageError{Code: "signup_closed", Title: "Email sign-in is off",
		Message: "Email and password sign-in is turned off on this server. Ask its admin."}
	errTooManyAttempts = pageError{Code: "rate_limited", Title: "Too many attempts",
		Message: "Too many attempts for this sign-in. Go back to ReqHarbor and click Sign in again."}
	errServerFull = pageError{Code: "server_full", Title: "Server is full",
		Message: "This server is out of storage space and can't create accounts right now. Try again later or ask its admin."}
)

// Messages shown in forms.
const (
	msgBadLogin      = "Email or password is incorrect."
	msgInviteInvalid = "That code isn't valid. Check it, or ask your server admin for a new one."
	// msgInviteEmail: an email-bound invite needs an email this server has confirmed.
	msgInviteEmail    = "This invite is for one email address, and this sign-in can't confirm that you own it. Ask your server admin for an invite without an email."
	msgInviteRequired = "Enter the invite code your server admin gave you."
	msgEmailInUse     = "An account with this email already exists. Sign in, or ask your server admin."
	msgSetupRequired  = "This server hasn't been set up yet. Enter the setup code from the server log first."
	msgBadEmail       = "Enter a valid email address."
	msgDisabled       = "This account is disabled. Ask your server admin."
)

func throttledMessage(minutes int) string {
	unit := "minutes"
	if minutes == 1 {
		unit = "minute"
	}
	return fmt.Sprintf("%s Try again in %d %s.", msgBadLogin, minutes, unit)
}

// started is "just now" or "N minutes ago".
func started(createdAt int64, now time.Time) string {
	d := now.Sub(time.UnixMilli(createdAt))
	m := int(d / time.Minute)
	switch {
	case m < 1:
		return "just now"
	case m == 1:
		return "1 minute ago"
	}
	return strconv.Itoa(m) + " minutes ago"
}

// resumeQuery rebuilds the authorize query of an auth request, so "back to sign in" starts a fresh
// request for the same app sign-in (same challenge, state and loopback URI).
func resumeQuery(ar *store.AuthRequest) template.URL {
	q := url.Values{}
	q.Set("client_id", ar.ClientID)
	q.Set("redirect_uri", ar.RedirectURI)
	q.Set("code_challenge", ar.CodeChallenge)
	q.Set("code_challenge_method", "S256")
	q.Set("state", ar.ClientState)
	if ar.DeviceName != "" {
		q.Set("device_name", ar.DeviceName)
	}
	if ar.MethodHint != nil {
		q.Set("method", *ar.MethodHint)
	}
	return template.URL(q.Encode()) //nolint:gosec // every value is query-escaped
}

// arPage fills the fields every page of one auth request shows.
func (s *Service) arPage(title string, ar *store.AuthRequest) pageData {
	d := s.base(title)
	d.AR, d.Device = ar.ID, ar.DeviceName
	d.Started = started(ar.CreatedAt, s.Clock.Now())
	d.ResumeQuery = resumeQuery(ar)
	d.PasswordAuth = s.Config.PasswordAuth
	return d
}
