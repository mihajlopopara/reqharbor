package providers

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"strconv"
	"strings"

	"golang.org/x/oauth2"

	"github.com/mihajlopopara/reqharbor/server/internal/accounts"
	"github.com/mihajlopopara/reqharbor/server/internal/config"
)

// GitHub endpoints. Tests point them at fake servers through config.TestOverrides.
const (
	GitHubAuthURL  = "https://github.com/login/oauth/authorize"
	GitHubTokenURL = "https://github.com/login/oauth/access_token"
	GitHubAPIURL   = "https://api.github.com"
)

// GitHubScopes are the OAuth App scopes (§5.4).
var GitHubScopes = []string{"read:user", "user:email"}

// UserAgent is sent on every GitHub API call (GitHub refuses requests without one).
const UserAgent = "reqharbor-server"

// maxProfileBytes bounds a profile response.
const maxProfileBytes = 1 << 20

// GitHub is sign-in with a GitHub OAuth App: authorization code with PKCE, then the profile from
// GET /user and the primary verified email from GET /user/emails (§5.4).
type GitHub struct {
	oauth  oauth2.Config
	api    string
	client *http.Client
}

// NewGitHub returns the GitHub provider for cfg. Its callback is {PUBLIC_URL}/auth/callback/github.
func NewGitHub(cfg *config.Config, client *http.Client) *GitHub {
	pick := func(override, def string) string {
		if override != "" {
			return override
		}
		return def
	}
	return &GitHub{
		oauth: oauth2.Config{
			ClientID:     cfg.GitHubClientID,
			ClientSecret: cfg.GitHubClientSecret,
			Endpoint: oauth2.Endpoint{
				AuthURL:   pick(cfg.Test.GitHubAuthURL, GitHubAuthURL),
				TokenURL:  pick(cfg.Test.GitHubTokenURL, GitHubTokenURL),
				AuthStyle: oauth2.AuthStyleInParams,
			},
			RedirectURL: cfg.PublicURL + "/auth/callback/github",
			Scopes:      GitHubScopes,
		},
		api:    strings.TrimSuffix(pick(cfg.Test.GitHubAPIURL, GitHubAPIURL), "/"),
		client: client,
	}
}

func (*GitHub) ID() string    { return "github" }
func (*GitHub) Label() string { return "GitHub" }

// AuthURL is GitHub's authorize URL with the state and the S256 challenge of verifier.
func (g *GitHub) AuthURL(state, verifier string) string {
	return g.oauth.AuthCodeURL(state, oauth2.S256ChallengeOption(verifier))
}

type githubUser struct {
	ID        json.Number `json:"id"`
	Login     string      `json:"login"`
	Name      string      `json:"name"`
	AvatarURL string      `json:"avatar_url"`
}

// GitHubEmail is one entry of GET /user/emails.
type GitHubEmail struct {
	Email    string `json:"email"`
	Primary  bool   `json:"primary"`
	Verified bool   `json:"verified"`
}

// Exchange trades the code (with the PKCE verifier and the client secret) for a token, reads the
// profile with it and drops it.
func (g *GitHub) Exchange(ctx context.Context, code, verifier string) (*Profile, error) {
	ctx = context.WithValue(ctx, oauth2.HTTPClient, g.client)
	tok, err := g.oauth.Exchange(ctx, code, oauth2.VerifierOption(verifier))
	if err != nil {
		var re *oauth2.RetrieveError
		if errors.As(err, &re) && re.ErrorCode != "" {
			return nil, &Error{Code: "token_" + SafeCode(re.ErrorCode)}
		}
		if errors.Is(err, context.DeadlineExceeded) || errors.Is(err, context.Canceled) {
			return nil, err
		}
		return nil, &Error{Code: "token_request_failed"}
	}
	var u githubUser
	if err := g.get(ctx, tok.AccessToken, "/user", &u); err != nil {
		return nil, err
	}
	id, err := strconv.ParseInt(u.ID.String(), 10, 64)
	if err != nil || id <= 0 {
		return nil, &Error{Code: "user_without_id"}
	}
	var emails []GitHubEmail
	if err := g.get(ctx, tok.AccessToken, "/user/emails", &emails); err != nil {
		return nil, err
	}
	p := &Profile{Provider: "github", Subject: strconv.FormatInt(id, 10), Name: strings.TrimSpace(u.Name)}
	if p.Name == "" {
		p.Name = u.Login
	}
	if strings.HasPrefix(u.AvatarURL, "https://") && len(u.AvatarURL) <= 2048 {
		p.AvatarURL = u.AvatarURL
	}
	if e := PrimaryVerifiedEmail(emails); e != "" {
		p.Email, p.EmailTrusted = e, true
	}
	return p, nil
}

// PrimaryVerifiedEmail is the normalised email that is both primary and verified, or "".
func PrimaryVerifiedEmail(emails []GitHubEmail) string {
	for _, e := range emails {
		if e.Primary && e.Verified {
			if n := accounts.NormalizeEmail(e.Email); accounts.ValidEmail(n) {
				return n
			}
			return ""
		}
	}
	return ""
}

func (g *GitHub) get(ctx context.Context, token, path string, dst any) error {
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, g.api+path, nil)
	if err != nil {
		return err
	}
	req.Header.Set("Authorization", "Bearer "+token)
	req.Header.Set("Accept", "application/vnd.github+json")
	req.Header.Set("X-GitHub-Api-Version", "2022-11-28")
	req.Header.Set("User-Agent", UserAgent)
	res, err := g.client.Do(req)
	if err != nil {
		if errors.Is(err, context.DeadlineExceeded) || errors.Is(err, context.Canceled) {
			return err
		}
		return &Error{Code: "api_request_failed"}
	}
	defer res.Body.Close()
	if res.StatusCode != http.StatusOK {
		_, _ = io.Copy(io.Discard, io.LimitReader(res.Body, maxProfileBytes))
		return &Error{Code: fmt.Sprintf("api_status_%d", res.StatusCode)}
	}
	dec := json.NewDecoder(io.LimitReader(res.Body, maxProfileBytes))
	dec.UseNumber()
	if err := dec.Decode(dst); err != nil {
		return &Error{Code: "api_bad_response"}
	}
	return nil
}

// SafeCode keeps an upstream OAuth error code loggable: [a-z0-9_], at most 40 characters.
func SafeCode(s string) string {
	var b strings.Builder
	for _, r := range strings.ToLower(s) {
		if b.Len() >= 40 {
			break
		}
		if (r >= 'a' && r <= 'z') || (r >= '0' && r <= '9') || r == '_' {
			b.WriteRune(r)
		}
	}
	if b.Len() == 0 {
		return "unknown"
	}
	return b.String()
}

// CallbackURL is the redirect URL to register on the OAuth App.
func (g *GitHub) CallbackURL() string { return g.oauth.RedirectURL }

var _ Redirector = (*GitHub)(nil)
