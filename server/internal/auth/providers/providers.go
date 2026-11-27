// Package providers holds the external sign-in methods of the hosted pages (06-go-server.md
// §5.4): GitHub (M1), Google and generic OIDC (M3), and the dev provider, which exists only in
// binaries built with -tags devauth (dev.go). The auth package owns the flow around them: the
// auth request, the upstream state, account resolution and the loopback redirect.
package providers

import (
	"context"
	"errors"
	"net/http"
	"net/url"
	"time"

	"github.com/mihajlopopara/reqharbor/server/internal/config"
)

// Profile is what a provider says about the person who signed in.
type Profile struct {
	Provider string // identities.provider
	Issuer   string // OIDC issuer; "" for github and dev
	Subject  string // stable id at the provider (GitHub's numeric id)
	// Email is the normalised email to use, or "" when the provider gave none that qualifies
	// (GitHub: no primary verified email).
	Email string
	// EmailTrusted follows the §5.4 table: GitHub's primary verified email is trusted; a dev
	// identity never is.
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
	// AuthURL is the identity provider's authorization URL for one round trip.
	AuthURL(state, verifier string) string
	// Exchange trades the callback code for the profile. The upstream token is used for the
	// profile calls only, then dropped; it is never stored or logged.
	Exchange(ctx context.Context, code, verifier string) (*Profile, error)
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

// Enabled returns the configured providers in their fixed order (§5.1: github, then in M3
// google and oidc), followed by the dev provider when this is a devauth build with
// REQHARBOR_DEV_AUTH=true. client makes the outbound calls; nil means DefaultHTTPClient.
func Enabled(cfg *config.Config, client *http.Client) []Provider {
	if client == nil {
		client = DefaultHTTPClient()
	}
	var out []Provider
	if cfg.GitHubClientID != "" && cfg.GitHubClientSecret != "" {
		out = append(out, NewGitHub(cfg, client))
	}
	if d := newDev(cfg); d != nil {
		out = append(out, d)
	}
	return out
}

// Listed reports whether discovery and the method list show the provider. The dev provider is
// never listed (§5.1).
func Listed(p Provider) bool {
	_, form := p.(Form)
	return !form
}
