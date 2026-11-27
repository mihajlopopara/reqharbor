package providers

// Generic OpenID Connect (06-go-server.md §5.4, S10), and the authorization-code flow it shares
// with Google: PKCE, state and nonce, the ID token from the token response (signature and claims
// checked, idtoken.go), the userinfo fallback for the email, and the §5.4 email trust rules.

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net/http"
	"net/url"
	"strings"
	"sync/atomic"
	"time"
	"unicode/utf8"

	"golang.org/x/oauth2"

	"github.com/mihajlopopara/reqharbor/server/internal/accounts"
	"github.com/mihajlopopara/reqharbor/server/internal/clock"
	"github.com/mihajlopopara/reqharbor/server/internal/config"
)

// flow is the OIDC authorization-code flow of one configured client.
type flow struct {
	id, label  string
	oauth      oauth2.Config
	authParams []oauth2.AuthCodeOption // extra authorize parameters (Google: prompt)
	verifier   *idTokenVerifier
	// profileIssuer is identities.issuer: the generic OIDC issuer, "" for Google (§8.3).
	profileIssuer string
	// userinfo is the userinfo endpoint, "" when the IdP has none.
	userinfo string
	// emailClaim names the claim holding the email.
	emailClaim string
	// requireVerified: only an email with email_verified == true qualifies, and it is trusted.
	// Without it (REQHARBOR_OIDC_REQUIRE_VERIFIED_EMAIL=false) any email qualifies and none is
	// trusted (§5.4).
	requireVerified bool
	client          *http.Client
}

func (f *flow) ID() string    { return f.id }
func (f *flow) Label() string { return f.label }

// CallbackURL is the redirect URI to register at the identity provider.
func (f *flow) CallbackURL() string { return f.oauth.RedirectURL }

// AuthURL is the authorization URL with the state, the S256 challenge of verifier and the nonce.
func (f *flow) AuthURL(state, verifier, nonce string) string {
	opts := append([]oauth2.AuthCodeOption{oauth2.S256ChallengeOption(verifier), oauth2.SetAuthURLParam("nonce", nonce)}, f.authParams...)
	return f.oauth.AuthCodeURL(state, opts...)
}

// Exchange trades the code for tokens, verifies the ID token for nonce and builds the profile.
func (f *flow) Exchange(ctx context.Context, code, verifier, nonce string) (*Profile, error) {
	if nonce == "" {
		return nil, &Error{Code: "nonce_missing"}
	}
	ctx = context.WithValue(ctx, oauth2.HTTPClient, f.client)
	tok, err := f.oauth.Exchange(ctx, code, oauth2.VerifierOption(verifier))
	if err != nil {
		return nil, tokenError(err)
	}
	raw, _ := tok.Extra("id_token").(string)
	if raw == "" {
		return nil, &Error{Code: "no_id_token"}
	}
	claims, err := f.verifier.verify(ctx, raw, nonce)
	if err != nil {
		return nil, err
	}
	src := claims.Raw
	if _, ok := src[f.emailClaim]; !ok && f.userinfo != "" {
		ui, err := f.fetchUserinfo(ctx, tok.AccessToken)
		if err != nil {
			return nil, err
		}
		if sub, _ := ui["sub"].(string); sub != claims.Subject {
			return nil, &Error{Code: "userinfo_sub_mismatch"}
		}
		src = ui
	}
	p := &Profile{Provider: f.id, Issuer: f.profileIssuer, Subject: claims.Subject}
	if email, verified := EmailFromClaims(src, f.emailClaim); email != "" {
		switch {
		case !f.requireVerified:
			p.Email = email // accepted, never trusted
		case verified:
			p.Email, p.EmailTrusted = email, true
		}
	}
	p.Name = nameFromClaims(claims.Raw)
	if p.Name == "" {
		p.Name = nameFromClaims(src)
	}
	if pic, _ := claims.Raw["picture"].(string); strings.HasPrefix(pic, "https://") && len(pic) <= 2048 {
		p.AvatarURL = pic
	}
	return p, nil
}

// EmailFromClaims returns the normalised email of claim ("" when absent or not an address) and
// whether it is verified: email_verified is true (or the string "true", as some IdPs send it),
// and, for a claim other than "email", its value equals the email claim, because email_verified
// vouches for that claim only.
func EmailFromClaims(c map[string]any, claim string) (string, bool) {
	v, _ := c[claim].(string)
	email := accounts.NormalizeEmail(v)
	if !accounts.ValidEmail(email) {
		return "", false
	}
	verified := false
	switch ev := c["email_verified"].(type) {
	case bool:
		verified = ev
	case string:
		verified = strings.EqualFold(ev, "true")
	}
	if verified && claim != "email" {
		e, _ := c["email"].(string)
		verified = accounts.NormalizeEmail(e) == email
	}
	return email, verified
}

// nameFromClaims is name, else given_name family_name, else preferred_username, cut to
// accounts.MaxNameRunes.
func nameFromClaims(c map[string]any) string {
	get := func(k string) string { s, _ := c[k].(string); return strings.TrimSpace(s) }
	n := get("name")
	if n == "" {
		n = strings.TrimSpace(get("given_name") + " " + get("family_name"))
	}
	if n == "" {
		n = get("preferred_username")
	}
	if r := []rune(n); len(r) > accounts.MaxNameRunes {
		n = strings.TrimSpace(string(r[:accounts.MaxNameRunes]))
	}
	return n
}

// fetchUserinfo reads the userinfo endpoint with the access token (JSON only).
func (f *flow) fetchUserinfo(ctx context.Context, accessToken string) (map[string]any, error) {
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, f.userinfo, nil)
	if err != nil {
		return nil, &Error{Code: "userinfo_request_failed"}
	}
	req.Header.Set("Authorization", "Bearer "+accessToken)
	req.Header.Set("Accept", "application/json")
	req.Header.Set("User-Agent", UserAgent)
	res, err := f.client.Do(req)
	if err != nil {
		if errors.Is(err, ErrInsecureRedirect) {
			return nil, &Error{Code: "insecure_redirect"}
		}
		if errors.Is(err, context.DeadlineExceeded) || errors.Is(err, context.Canceled) {
			return nil, err
		}
		return nil, &Error{Code: "userinfo_request_failed"}
	}
	defer res.Body.Close()
	if res.StatusCode != http.StatusOK {
		_, _ = io.Copy(io.Discard, io.LimitReader(res.Body, maxProfileBytes))
		return nil, &Error{Code: fmt.Sprintf("userinfo_status_%d", res.StatusCode)}
	}
	dec := json.NewDecoder(io.LimitReader(res.Body, maxProfileBytes))
	dec.UseNumber()
	var out map[string]any
	if err := dec.Decode(&out); err != nil || out == nil {
		return nil, &Error{Code: "userinfo_bad_response"}
	}
	return out, nil
}

// Discovery backoff (§5.4): the first retry after DiscoveryBackoff, doubling up to
// DiscoveryMaxBackoff, until discovery succeeds.
const (
	DiscoveryBackoff    = time.Second
	DiscoveryMaxBackoff = 5 * time.Minute
)

// OIDC is sign-in with one generic OpenID Connect provider. It is unavailable (not listed, routes
// 404) until Run has fetched {issuer}/.well-known/openid-configuration and found it acceptable:
// the document's issuer equals REQHARBOR_OIDC_ISSUER, and the authorization, token, userinfo and
// JWKS endpoints are https (http only for a loopback host). A document that breaks these rules is
// a configuration error: logged, never retried, and the provider stays hidden. Network errors and
// bad responses are retried with backoff.
type OIDC struct {
	issuer, clientID, secret, label, redirect, emailClaim string
	scopes                                                []string
	requireVerified                                       bool
	client                                                *http.Client
	clock                                                 clock.Clock
	logger                                                *slog.Logger
	backoff                                               time.Duration

	ready atomic.Pointer[flow]
}

// NewOIDC returns the (unavailable) OIDC provider for cfg. Its callback is
// {PUBLIC_URL}/auth/callback/oidc.
func NewOIDC(cfg *config.Config, d Deps) *OIDC {
	b := cfg.Test.OIDCDiscoveryBackoff
	if b <= 0 {
		b = DiscoveryBackoff
	}
	return &OIDC{
		issuer: cfg.OIDCIssuer, clientID: cfg.OIDCClientID, secret: cfg.OIDCClientSecret, label: cfg.OIDCName,
		redirect: cfg.PublicURL + "/auth/callback/oidc", emailClaim: cfg.OIDCEmailClaim, scopes: cfg.OIDCScopes,
		requireVerified: cfg.OIDCRequireVerifiedEmail, client: SecureClient(d.Client), clock: d.Clock, logger: d.Logger, backoff: b,
	}
}

func (o *OIDC) ID() string    { return "oidc" }
func (o *OIDC) Label() string { return o.label }

// CallbackURL is the redirect URI to register at the identity provider.
func (o *OIDC) CallbackURL() string { return o.redirect }

// Available reports whether discovery has succeeded.
func (o *OIDC) Available() bool { return o.ready.Load() != nil }

// AuthURL is the provider's authorization URL; "" while unavailable (the routes answer 404 then).
func (o *OIDC) AuthURL(state, verifier, nonce string) string {
	f := o.ready.Load()
	if f == nil {
		return ""
	}
	return f.AuthURL(state, verifier, nonce)
}

// Exchange is the flow's exchange once discovery has succeeded.
func (o *OIDC) Exchange(ctx context.Context, code, verifier, nonce string) (*Profile, error) {
	f := o.ready.Load()
	if f == nil {
		return nil, &Error{Code: "not_discovered"}
	}
	return f.Exchange(ctx, code, verifier, nonce)
}

// ConfigError is a discovery document that can never work; discovery is not retried.
type ConfigError struct{ Msg string }

func (e *ConfigError) Error() string { return e.Msg }

// Run performs discovery until it succeeds (then calls changed), hits a configuration error, or
// ctx ends.
func (o *OIDC) Run(ctx context.Context, changed func()) {
	delay := o.backoff
	for attempt := 1; ; attempt++ {
		f, err := o.Discover(ctx)
		if err == nil {
			o.ready.Store(f)
			o.logger.Info("OIDC sign-in enabled", "issuer", o.issuer, "attempt", attempt)
			if changed != nil {
				changed()
			}
			return
		}
		if ctx.Err() != nil {
			return
		}
		var ce *ConfigError
		if errors.As(err, &ce) {
			o.logger.Error("OIDC sign-in disabled: "+ce.Msg, "issuer", o.issuer)
			return
		}
		o.logger.Warn("OIDC discovery failed; retrying", "issuer", o.issuer, "attempt", attempt, "reason", ErrorCode(err), "retry_in", delay.String())
		t := time.NewTimer(delay)
		select {
		case <-ctx.Done():
			t.Stop()
			return
		case <-t.C:
		}
		delay = min(2*delay, DiscoveryMaxBackoff)
	}
}

// discoveryDoc is the part of the OpenID Provider Metadata the server uses.
type discoveryDoc struct {
	Issuer                string   `json:"issuer"`
	AuthorizationEndpoint string   `json:"authorization_endpoint"`
	TokenEndpoint         string   `json:"token_endpoint"`
	UserinfoEndpoint      string   `json:"userinfo_endpoint"`
	JWKSURI               string   `json:"jwks_uri"`
	IDTokenAlgs           []string `json:"id_token_signing_alg_values_supported"`
	TokenAuthMethods      []string `json:"token_endpoint_auth_methods_supported"`
}

// maxLoggedValue bounds a value from the discovery document quoted in a log message.
const maxLoggedValue = 200

// clip cuts an IdP-supplied string to maxLoggedValue bytes (on a rune boundary) for the log.
func clip(s string) string {
	if len(s) <= maxLoggedValue {
		return s
	}
	cut := maxLoggedValue
	for cut > 0 && !utf8.RuneStart(s[cut]) {
		cut--
	}
	return s[:cut] + "…"
}

// DiscoveryURL is the discovery document's URL for an issuer (OIDC Discovery §4).
func DiscoveryURL(issuer string) string {
	return strings.TrimSuffix(issuer, "/") + "/.well-known/openid-configuration"
}

// Discover fetches and checks the discovery document once and returns the flow it describes. A
// *ConfigError is permanent; any other error may be retried.
func (o *OIDC) Discover(ctx context.Context) (*flow, error) {
	ctx, cancel := context.WithTimeout(ctx, HTTPTimeout)
	defer cancel()
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, DiscoveryURL(o.issuer), nil)
	if err != nil {
		return nil, &ConfigError{Msg: "REQHARBOR_OIDC_ISSUER is not a usable URL"}
	}
	req.Header.Set("Accept", "application/json")
	req.Header.Set("User-Agent", UserAgent)
	res, err := o.client.Do(req)
	if err != nil {
		if errors.Is(err, ErrInsecureRedirect) {
			return nil, &ConfigError{Msg: "the discovery document redirects to a non-https URL"}
		}
		if errors.Is(err, context.DeadlineExceeded) || errors.Is(err, context.Canceled) {
			return nil, err
		}
		return nil, &Error{Code: "discovery_request_failed"}
	}
	defer res.Body.Close()
	body, err := io.ReadAll(io.LimitReader(res.Body, maxProfileBytes+1))
	if err != nil {
		return nil, &Error{Code: "discovery_request_failed"}
	}
	if res.StatusCode != http.StatusOK {
		return nil, &Error{Code: fmt.Sprintf("discovery_status_%d", res.StatusCode)}
	}
	var doc discoveryDoc
	if len(body) > maxProfileBytes || !bytes.HasPrefix(bytes.TrimSpace(body), []byte("{")) || json.Unmarshal(body, &doc) != nil {
		return nil, &Error{Code: "discovery_bad_response"}
	}
	return o.flowFrom(&doc)
}

// flowFrom checks a discovery document (§5.4) and builds the flow.
func (o *OIDC) flowFrom(doc *discoveryDoc) (*flow, error) {
	if doc.Issuer != o.issuer {
		return nil, &ConfigError{Msg: fmt.Sprintf("the discovery document's issuer %q differs from REQHARBOR_OIDC_ISSUER %q; set REQHARBOR_OIDC_ISSUER to exactly the issuer the provider publishes", clip(doc.Issuer), o.issuer)}
	}
	endpoints := []struct {
		name, value string
		required    bool
	}{
		{"authorization_endpoint", doc.AuthorizationEndpoint, true},
		{"token_endpoint", doc.TokenEndpoint, true},
		{"jwks_uri", doc.JWKSURI, true},
		{"userinfo_endpoint", doc.UserinfoEndpoint, false},
	}
	for _, e := range endpoints {
		if e.value == "" {
			if e.required {
				return nil, &ConfigError{Msg: "the discovery document has no " + e.name}
			}
			continue
		}
		u, err := url.Parse(e.value)
		if err != nil || !u.IsAbs() || !config.IsHTTPSOrLoopback(u) {
			return nil, &ConfigError{Msg: fmt.Sprintf("the discovery document's %s %q is not https (http is allowed only for a loopback host)", e.name, clip(e.value))}
		}
	}
	if len(doc.IDTokenAlgs) > 0 && !contains(doc.IDTokenAlgs, AlgRS256) && !contains(doc.IDTokenAlgs, AlgES256) {
		return nil, &ConfigError{Msg: "the provider signs ID tokens with none of RS256 and ES256 (id_token_signing_alg_values_supported: " + clip(strings.Join(doc.IDTokenAlgs, " ")) + ")"}
	}
	style := oauth2.AuthStyleInHeader // client_secret_basic, the default of OIDC Discovery
	if len(doc.TokenAuthMethods) > 0 && !contains(doc.TokenAuthMethods, "client_secret_basic") {
		if !contains(doc.TokenAuthMethods, "client_secret_post") {
			return nil, &ConfigError{Msg: "the token endpoint supports neither client_secret_basic nor client_secret_post (token_endpoint_auth_methods_supported: " + clip(strings.Join(doc.TokenAuthMethods, " ")) + ")"}
		}
		style = oauth2.AuthStyleInParams
	}
	return &flow{
		id:    "oidc",
		label: o.label,
		oauth: oauth2.Config{
			ClientID:     o.clientID,
			ClientSecret: o.secret,
			Endpoint:     oauth2.Endpoint{AuthURL: doc.AuthorizationEndpoint, TokenURL: doc.TokenEndpoint, AuthStyle: style},
			RedirectURL:  o.redirect,
			Scopes:       o.scopes,
		},
		verifier: &idTokenVerifier{
			keys:     NewJWKS(doc.JWKSURI, o.client, o.clock),
			issuers:  []string{o.issuer},
			clientID: o.clientID,
			now:      o.clock.Now,
		},
		profileIssuer:   o.issuer,
		userinfo:        doc.UserinfoEndpoint,
		emailClaim:      o.emailClaim,
		requireVerified: o.requireVerified,
		client:          o.client,
	}, nil
}

var (
	_ Redirector = (*OIDC)(nil)
	_ Dynamic    = (*OIDC)(nil)
)
