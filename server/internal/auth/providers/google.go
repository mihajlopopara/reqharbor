package providers

import (
	"github.com/mihajlopopara/reqharbor/server/internal/config"
	"golang.org/x/oauth2"
)

// Google endpoints (fixed; Google's discovery document names these). Tests point them at a fake
// IdP through config.TestOverrides.
const (
	GoogleAuthURL  = "https://accounts.google.com/o/oauth2/v2/auth"
	GoogleTokenURL = "https://oauth2.googleapis.com/token"
	GoogleJWKSURL  = "https://www.googleapis.com/oauth2/v3/certs"
)

// GoogleIssuers are the iss values Google uses in ID tokens.
var GoogleIssuers = []string{"https://accounts.google.com", "accounts.google.com"}

// GoogleScopes are the scopes Google sign-in asks for (§5.4).
var GoogleScopes = []string{"openid", "email", "profile"}

// Google is sign-in with a Google web-application OAuth client: authorization code with PKCE,
// state and nonce, prompt=select_account (QA 9a: a crafted authorize URL never signs in silently),
// and the profile from the ID token of the token response. Only a verified email
// (email_verified == true) qualifies, and it is trusted (§5.4).
type Google struct {
	*flow
}

// NewGoogle returns the Google provider for cfg. Its callback is {PUBLIC_URL}/auth/callback/google.
func NewGoogle(cfg *config.Config, d Deps) *Google {
	pick := func(override, def string) string {
		if override != "" {
			return override
		}
		return def
	}
	client := SecureClient(d.Client)
	f := &flow{
		id:    "google",
		label: "Google",
		oauth: oauth2.Config{
			ClientID:     cfg.GoogleClientID,
			ClientSecret: cfg.GoogleClientSecret,
			Endpoint: oauth2.Endpoint{
				AuthURL:   pick(cfg.Test.GoogleAuthURL, GoogleAuthURL),
				TokenURL:  pick(cfg.Test.GoogleTokenURL, GoogleTokenURL),
				AuthStyle: oauth2.AuthStyleInParams,
			},
			RedirectURL: cfg.PublicURL + "/auth/callback/google",
			Scopes:      GoogleScopes,
		},
		authParams: []oauth2.AuthCodeOption{oauth2.SetAuthURLParam("prompt", "select_account")},
		verifier: &idTokenVerifier{
			keys:     NewJWKS(pick(cfg.Test.GoogleJWKSURL, GoogleJWKSURL), client, d.Clock),
			issuers:  GoogleIssuers,
			clientID: cfg.GoogleClientID,
			now:      d.Clock.Now,
		},
		emailClaim:      "email",
		requireVerified: true,
		client:          client,
	}
	return &Google{flow: f}
}

var _ Redirector = (*Google)(nil)
