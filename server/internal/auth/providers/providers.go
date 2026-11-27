// Package providers holds the external sign-in methods of the hosted pages (06-go-server.md
// §5.4): GitHub (M1), Google and generic OIDC (M3), and the dev provider, which exists only in
// binaries built with -tags devauth (dev.go). The auth package owns the flow around them: the
// auth request, the upstream state, account resolution and the loopback redirect.
package providers

import (
	"context"
	"errors"
	"log/slog"
	"net/http"
	"net/url"
	"time"

	"golang.org/x/oauth2"

	"github.com/mihajlopopara/reqharbor/server/internal/clock"
	"github.com/mihajlopopara/reqharbor/server/internal/config"
)

// Profile is what a provider says about the person who signed in.
type Profile struct {
	Provider string // identities.provider
	Issuer   string // the generic OIDC issuer; "" for github, google and dev
	Subject  string // stable id at the provider (GitHub's numeric id, the OIDC sub)
	// Email is the normalised email to use, or "" when the provider gave none that qualifies
	// (GitHub: no primary verified email).
	Email string
	// EmailTrusted follows the §5.4 table: GitHub's primary verified email, Google's verified
	// email and an OIDC email with email_verified (under REQUIRE_VERIFIED_EMAIL=true) are trusted;
	// an OIDC email under REQUIRE_VERIFIED_EMAIL=false and a dev identity never are.
	EmailTrusted bool
	Name         string
	AvatarURL    string // "" when none
}

// Provider is one external sign-in method.
type Provider interface {
	ID() string    // the {provider} path segment and identities.provider
	Label() string // the button and discovery label
}

// Redirector is a provider reached through a browser redirect to the identity provider and back
// to {PUBLIC_URL}/auth/callback/{id}.
type Redirector interface {
	Provider
	// AuthURL is the identity provider's authorization URL for one round trip. nonce is the
	// OIDC nonce of the round trip (GitHub ignores it).
	AuthURL(state, verifier, nonce string) string
	// Exchange trades the callback code for the profile. The upstream token is used for the
	// profile calls only, then dropped; it is never stored or logged. nonce is the value sent by
	// AuthURL, which the ID token must carry (Google and OIDC).
	Exchange(ctx context.Context, code, verifier, nonce string) (*Profile, error)
}

// Dynamic is a provider that becomes available only later: generic OIDC, once discovery has
// succeeded (§5.4). Until then it is not listed and its routes answer 404.
type Dynamic interface {
	Provider
	Available() bool
	// Run makes the provider available (discovery, retried with backoff) and then calls changed.
	// It returns once the provider is available, on a configuration error (logged; the provider
	// stays unavailable) or when ctx ends.
	Run(ctx context.Context, changed func())
}

// IsAvailable reports whether p can be used now.
func IsAvailable(p Provider) bool {
	d, ok := p.(Dynamic)
	return !ok || d.Available()
}

// Form is a provider that is a form on the server itself (the dev provider).
type Form interface {
	Provider
	// Allow reports whether this request may use the provider at all; otherwise the routes
	// answer 404.
	Allow(r *http.Request) bool
	// Template is the page's "content" template.
	Template() string
	// FromForm reads the profile from the posted form, or returns the message to show.
	FromForm(form url.Values) (*Profile, string)
}

// Error is a failed provider call. Code is safe to log (an OAuth error code or a short reason);
// it never contains a token.
type Error struct{ Code string }

func (e *Error) Error() string { return "provider: " + e.Code }

// ErrorCode returns the loggable code of a provider error.
func ErrorCode(err error) string {
	var pe *Error
	if errors.As(err, &pe) {
		return pe.Code
	}
	if errors.Is(err, context.DeadlineExceeded) || errors.Is(err, context.Canceled) {
		return "timeout"
	}
	return "request_failed"
}

// HTTPTimeout bounds each outbound call to a provider.
const HTTPTimeout = 15 * time.Second

// DefaultHTTPClient is the outbound client for provider calls.
func DefaultHTTPClient() *http.Client {
	return &http.Client{Timeout: HTTPTimeout}
}

// ErrInsecureRedirect is returned by an outbound call to Google or an OIDC provider that was
// redirected from https to http, or to http on a host that is not loopback (§5.4).
var ErrInsecureRedirect = errors.New("providers: redirect to a non-https URL refused")

// maxRedirects bounds the redirects of one outbound call.
const maxRedirects = 5

// SecureClient returns a copy of c that refuses any redirect to a non-https URL, except from
// http to http on a loopback host (a local test IdP), and follows at most 5 redirects. The ID
// token is trusted because it comes over TLS from the token endpoint, so a downgrade must never
// be followed (§5.4).
func SecureClient(c *http.Client) *http.Client {
	cc := *c
	prev := c.CheckRedirect
	cc.CheckRedirect = func(req *http.Request, via []*http.Request) error {
		if len(via) >= maxRedirects {
			return errors.New("providers: too many redirects")
		}
		last := via[len(via)-1]
		if req.URL.Scheme != "https" && (last.URL.Scheme == "https" || req.URL.Scheme != "http" || !config.IsLoopbackHost(req.URL.Hostname())) {
			return ErrInsecureRedirect
		}
		if prev != nil {
			return prev(req, via)
		}
		return nil
	}
	return &cc
}

// tokenError turns a failed token-endpoint call into a loggable provider error.
func tokenError(err error) error {
	var re *oauth2.RetrieveError
	switch {
	case errors.As(err, &re) && re.ErrorCode != "":
		return &Error{Code: "token_" + SafeCode(re.ErrorCode)}
	case errors.Is(err, ErrInsecureRedirect):
		return &Error{Code: "insecure_redirect"}
	case errors.Is(err, context.DeadlineExceeded), errors.Is(err, context.Canceled):
		return err
	}
	return &Error{Code: "token_request_failed"}
}

// Deps are the collaborators of the providers.
type Deps struct {
	// Client makes the outbound calls; nil means DefaultHTTPClient.
	Client *http.Client
	// Clock judges ID-token times and the JWKS cache; nil means the system clock.
	Clock clock.Clock
	// Logger receives OIDC discovery outcomes; nil means slog.Default().
	Logger *slog.Logger
}

// Enabled returns the configured providers in their fixed order (§5.1: github, google, oidc),
// followed by the dev provider when this is a devauth build with REQHARBOR_DEV_AUTH=true. The
// OIDC provider is returned unavailable; its Run performs the discovery.
func Enabled(cfg *config.Config, d Deps) []Provider {
	if d.Client == nil {
		d.Client = DefaultHTTPClient()
	}
	if d.Clock == nil {
		d.Clock = clock.System{}
	}
	if d.Logger == nil {
		d.Logger = slog.Default()
	}
	var out []Provider
	if cfg.GitHubClientID != "" && cfg.GitHubClientSecret != "" {
		out = append(out, NewGitHub(cfg, d.Client))
	}
	if cfg.GoogleClientID != "" && cfg.GoogleClientSecret != "" {
		out = append(out, NewGoogle(cfg, d))
	}
	if cfg.OIDCIssuer != "" && cfg.OIDCClientID != "" && cfg.OIDCClientSecret != "" {
		out = append(out, NewOIDC(cfg, d))
	}
	if dv := newDev(cfg); dv != nil {
		out = append(out, dv)
	}
	return out
}

// Listed reports whether discovery and the method list show the provider. The dev provider is
// never listed (§5.1).
func Listed(p Provider) bool {
	_, form := p.(Form)
	return !form
}
