// Package auth is sign-in for the desktop app (06-go-server.md §5): the hosted /auth/* pages
// (authorize, password login and signup, invite and setup codes, reset links), the PKCE code
// exchange and refresh at POST /v1/auth/token, logout, and per-request session auth.
package auth

import (
	"context"
	"html/template"
	"log/slog"
	"net/http"
	"net/url"
	"strings"
	"sync"
	"time"

	"github.com/mihajlopopara/reqharbor/server/internal/auth/providers"
	"github.com/mihajlopopara/reqharbor/server/internal/auth/throttle"
	"github.com/mihajlopopara/reqharbor/server/internal/clientip"
	"github.com/mihajlopopara/reqharbor/server/internal/clock"
	"github.com/mihajlopopara/reqharbor/server/internal/config"
	"github.com/mihajlopopara/reqharbor/server/internal/httpx"
	"github.com/mihajlopopara/reqharbor/server/internal/mail"
	"github.com/mihajlopopara/reqharbor/server/internal/ratelimit"
	"github.com/mihajlopopara/reqharbor/server/internal/store"
)

// ClientID is the only OAuth client: the desktop app.
const ClientID = "reqharbor-desktop"

// Lifetimes (§5.2, §5.8).
const (
	AuthRequestTTL = 10 * time.Minute
	CodeTTL        = 60 * time.Second
	AccessTTL      = time.Hour
	RefreshIdleTTL = 60 * 24 * time.Hour
	RefreshMaxTTL  = 365 * 24 * time.Hour
	GraceWindow    = 60 * time.Second
	ResetLinkTTL   = 24 * time.Hour
	SetupCodeTTL   = 24 * time.Hour
)

// Per-auth-request budgets (§5.9).
const (
	MaxPasswordAttempts = 10
	MaxCodeAttempts     = 10
)

// Method is a sign-in method shown on the pages and in discovery.
type Method struct{ ID, Label string }

// Deps are the collaborators of the auth service.
type Deps struct {
	Store    store.Store
	Clock    clock.Clock
	Config   *config.Config
	Logger   *slog.Logger
	ClientIP *clientip.Resolver
	Hasher   *Hasher
	Throttle *throttle.Throttle
	// Growth is the breaker check for account creation (a growing write); nil allows it.
	Growth func() error
	// OpenSignupFull reports whether REQHARBOR_OPEN_SIGNUP_TOTAL_BYTES is reached; open signup
	// then shows server_user_limit. nil means never.
	OpenSignupFull func() bool
	// UserCreated is called after an account was created through the pages (discovery rebuilds
	// setupRequired).
	UserCreated func()
	// HTTPClient makes the outbound provider calls; nil means providers.DefaultHTTPClient.
	HTTPClient *http.Client
	// MethodsChanged is called when the method list changes after startup (the OIDC provider
	// became available); discovery then rebuilds its document. nil means nothing to notify.
	MethodsChanged func()
	// Mail sends the signup codes and reset links when SMTP is configured (S9); nil means no
	// email: signups are created at once and resets go through the admin.
	Mail *mail.Outbox
}

// Service serves the auth routes.
type Service struct {
	Deps
	public     *url.URL
	cookieName string
	cop        *http.CrossOriginProtection

	// External providers (S5), in their fixed order, and by id.
	providers  []providers.Provider
	providerBy map[string]providers.Provider

	formPagesMu sync.Mutex
	formPages   map[string]*template.Template

	// Limiters (§5.9). Keys are prefixed with their purpose and hashed by ratelimit.
	PagesLimiter   *ratelimit.Limiter // /auth/* per IP, 120 / min
	LoginLimiter   *ratelimit.Limiter // password login per IP, 30 / min
	SignupLimiter  *ratelimit.Limiter // account creation per IP, 30 / h
	CodeLimiter    *ratelimit.Limiter // invite / setup code entry per IP, 30 / h
	TokenLimiter   *ratelimit.Limiter // POST /v1/auth/token per IP, 60 / min
	FailedExchange *ratelimit.Limiter // failed token exchanges per IP, 30 / h
	// Reset requests by email (S9): 20 / h per IP and 5 / h per email.
	ForgotIPLimiter    *ratelimit.Limiter
	ForgotEmailLimiter *ratelimit.Limiter
	// Wrong signup codes per address across every auth request and network (S9).
	CodeGuessLimiter *ratelimit.Limiter

	warnSchemeOnce sync.Once
}

// New returns the auth service.
func New(d Deps) *Service {
	pub, err := url.Parse(d.Config.PublicURL)
	if err != nil {
		panic("auth: invalid public URL: " + err.Error())
	}
	s := &Service{Deps: d, public: pub, cookieName: "rh_ar"}
	if pub.Scheme == "https" {
		s.cookieName = "__Secure-rh_ar"
	}
	s.cop = http.NewCrossOriginProtection()
	s.cop.SetDenyHandler(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		s.renderError(w, r, http.StatusForbidden, pageError{Code: "wrong_origin", Title: "Request blocked",
			Message: "This form was sent from another site. Open this page from " + s.Config.PublicURL + "."})
	}))
	s.providers = providers.Enabled(d.Config, providers.Deps{Client: d.HTTPClient, Clock: d.Clock, Logger: d.Logger})
	s.providerBy = make(map[string]providers.Provider, len(s.providers))
	for _, p := range s.providers {
		s.providerBy[p.ID()] = p
	}
	c := d.Clock
	s.PagesLimiter = ratelimit.New(c, d.Config.RateAuthPerMin, time.Minute)
	s.LoginLimiter = ratelimit.New(c, d.Config.RateLoginPerMin, time.Minute)
	s.SignupLimiter = ratelimit.New(c, d.Config.RateSignupPerHour, time.Hour)
	s.CodeLimiter = ratelimit.New(c, 30, time.Hour)
	s.TokenLimiter = ratelimit.New(c, d.Config.RateTokenPerMin, time.Minute)
	s.FailedExchange = ratelimit.New(c, 30, time.Hour)
	s.ForgotIPLimiter = ratelimit.New(c, 20, time.Hour)
	s.ForgotEmailLimiter = ratelimit.New(c, 5, time.Hour)
	s.CodeGuessLimiter = ratelimit.New(c, MaxWrongCodesPerEmail, EmailCodeGuessWindow)
	return s
}

func (s *Service) openSignupFull() bool { return s.OpenSignupFull != nil && s.OpenSignupFull() }

// Limiters returns every limiter, for the sweeper job.
func (s *Service) Limiters() []*ratelimit.Limiter {
	return []*ratelimit.Limiter{s.PagesLimiter, s.LoginLimiter, s.SignupLimiter, s.CodeLimiter, s.TokenLimiter, s.FailedExchange,
		s.ForgotIPLimiter, s.ForgotEmailLimiter, s.CodeGuessLimiter}
}

// Methods are the sign-in methods available now, in their fixed order (§5.1): password, then
// github, google and oidc when configured; oidc only once its discovery has succeeded. The dev
// provider is never listed.
func (s *Service) Methods() []Method {
	var out []Method
	if s.Config.PasswordAuth {
		out = append(out, Method{ID: "password", Label: "Email and password"})
	}
	for _, p := range s.providers {
		if providers.Listed(p) && providers.IsAvailable(p) {
			out = append(out, Method{ID: p.ID(), Label: p.Label()})
		}
	}
	return out
}

// HasDynamicProviders reports whether RunProviders has work (an OIDC provider is configured).
func (s *Service) HasDynamicProviders() bool {
	for _, p := range s.providers {
		if _, ok := p.(providers.Dynamic); ok {
			return true
		}
	}
	return false
}

// RunProviders makes the late providers available (OIDC discovery with retries, §5.4) and calls
// MethodsChanged each time one becomes available. It returns when every one is done or ctx ends.
func (s *Service) RunProviders(ctx context.Context) {
	var wg sync.WaitGroup
	for _, p := range s.providers {
		if d, ok := p.(providers.Dynamic); ok {
			wg.Add(1)
			go func() {
				defer wg.Done()
				d.Run(ctx, func() {
					if s.MethodsChanged != nil {
						s.MethodsChanged()
					}
				})
			}()
		}
	}
	wg.Wait()
}

// provider returns the enabled and available provider with this id.
func (s *Service) provider(id string) (providers.Provider, bool) {
	p, ok := s.providerBy[id]
	if !ok || !providers.IsAvailable(p) {
		return nil, false
	}
	return p, true
}

// Register adds the auth routes to mux.
func (s *Service) Register(mux *http.ServeMux) {
	page := func(get, post http.Handler) http.Handler {
		var m httpx.Methods
		if get != nil {
			m = append(m, httpx.Method{Name: "GET", Handler: s.pageGET(get)})
		}
		if post != nil {
			m = append(m, httpx.Method{Name: "POST", Handler: s.pagePOST(post)})
		}
		return m
	}
	mux.Handle("/auth/authorize", page(http.HandlerFunc(s.authorize), nil))
	mux.Handle("/auth/signup", page(http.HandlerFunc(s.signupPage), nil))
	mux.Handle("/auth/code", page(nil, http.HandlerFunc(s.code)))
	mux.Handle("/auth/password/login", page(nil, http.HandlerFunc(s.passwordLogin)))
	mux.Handle("/auth/password/signup", page(nil, http.HandlerFunc(s.passwordSignup)))
	mux.Handle("/auth/password/verify", page(nil, http.HandlerFunc(s.verify)))
	mux.Handle("/auth/done", page(http.HandlerFunc(s.done), nil))
	mux.Handle("/auth/forgot", page(http.HandlerFunc(s.forgot), http.HandlerFunc(s.forgot)))
	mux.Handle("/auth/reset", page(http.HandlerFunc(s.resetPage), http.HandlerFunc(s.reset)))
	mux.Handle("/auth/start/{provider}", s.providerRoute(page(http.HandlerFunc(s.start), nil)))
	mux.Handle("/auth/callback/{provider}", s.providerRoute(page(http.HandlerFunc(s.callback), nil)))
	if dev := s.devProvider(); dev != nil {
		mux.Handle(providers.DevFormPath, s.formRoute(dev, page(nil, http.HandlerFunc(s.devSubmit))))
	}

	mux.Handle("/v1/auth/token", httpx.Methods{{Name: "POST", Handler: httpx.Func(s.token)}})
	mux.Handle("/v1/auth/logout", httpx.Methods{{Name: "POST", Handler: httpx.Func(s.logout)}})
}

// bucket is the rate-limit bucket of the request's client.
func (s *Service) bucket(r *http.Request) string { return clientip.Bucket(s.ClientIP.ClientIP(r)) }

func retrySeconds(d time.Duration) int64 { return int64((d + time.Second - 1) / time.Second) }

// effectiveOrigin returns the request's host (port included) and its scheme when known (§5.3):
// TLS, or X-Forwarded-Proto from a trusted proxy; "" when unknown.
func (s *Service) effectiveOrigin(r *http.Request) (host, scheme string) {
	host = r.Host
	trusted := s.ClientIP.PeerIsTrusted(r)
	if trusted {
		if fh := strings.TrimSpace(strings.Split(r.Header.Get("X-Forwarded-Host"), ",")[0]); fh != "" {
			host = fh
		}
	}
	switch {
	case r.TLS != nil:
		scheme = "https"
	case trusted:
		if p := strings.ToLower(strings.TrimSpace(strings.Split(r.Header.Get("X-Forwarded-Proto"), ",")[0])); p == "http" || p == "https" {
			scheme = p
		}
	}
	return strings.ToLower(host), scheme
}

// sameHost compares a request host with the public URL's host, ignoring the default port of the
// public scheme.
func (s *Service) sameHost(host string) bool {
	want := strings.ToLower(s.public.Host)
	if host == want {
		return true
	}
	def := ":443"
	if s.public.Scheme == "http" {
		def = ":80"
	}
	return strings.TrimSuffix(host, def) == want
}

// onCanonicalOrigin applies the canonical-origin rule. It returns false when the request is off
// origin; known reports whether the scheme was known.
func (s *Service) onCanonicalOrigin(r *http.Request) bool {
	host, scheme := s.effectiveOrigin(r)
	if !s.sameHost(host) {
		return false
	}
	if scheme != "" && scheme != s.public.Scheme {
		return false
	}
	if scheme == "" && s.public.Scheme == "https" {
		s.warnSchemeOnce.Do(func() {
			s.Logger.Warn("PUBLIC_URL is https but requests for its host arrive over plain http from an untrusted peer; add the proxy to REQHARBOR_TRUSTED_PROXIES")
		})
	}
	return true
}

func (s *Service) pageLimit(w http.ResponseWriter, r *http.Request) bool {
	if ok, retry := s.PagesLimiter.Allow("auth_pages:" + s.bucket(r)); !ok {
		s.renderRateLimited(w, r, retry)
		return false
	}
	return true
}

// pageGET wraps a hosted-page GET: canonical-origin redirect (no cookie, no auth request), then
// the per-IP page limit.
func (s *Service) pageGET(h http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if !s.onCanonicalOrigin(r) {
			target := s.Config.PublicURL + r.URL.EscapedPath()
			if r.URL.RawQuery != "" {
				target += "?" + r.URL.RawQuery
			}
			w.Header().Set("Location", target)
			w.WriteHeader(http.StatusFound)
			return
		}
		if !s.pageLimit(w, r) {
			return
		}
		h.ServeHTTP(w, r)
	})
}

// pagePOST wraps a hosted-page POST: off-origin POSTs render wrong_origin and change nothing;
// then CrossOriginProtection, the per-IP page limit and a 64 KiB form.
func (s *Service) pagePOST(h http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if !s.onCanonicalOrigin(r) {
			s.renderError(w, r, http.StatusForbidden, pageError{Code: "wrong_origin", Title: "Wrong address",
				Message: "Open this page from " + s.Config.PublicURL + "."})
			return
		}
		s.cop.Handler(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			if !s.pageLimit(w, r) {
				return
			}
			r.Body = http.MaxBytesReader(w, r.Body, 64<<10)
			if err := r.ParseForm(); err != nil {
				s.renderError(w, r, http.StatusBadRequest, pageError{Code: "bad_request", Title: "Something went wrong",
					Message: "The form could not be read. Go back and try again."})
				return
			}
			h.ServeHTTP(w, r)
		})).ServeHTTP(w, r)
	})
}
