package auth

import (
	"crypto/sha256"
	"encoding/base64"
	"errors"
	"io"
	"net/http"
	"net/url"
	"regexp"

	"github.com/mihajlopopara/reqharbor/server/internal/clock"
	"github.com/mihajlopopara/reqharbor/server/internal/httpx"
	"github.com/mihajlopopara/reqharbor/server/internal/ids"
	"github.com/mihajlopopara/reqharbor/server/internal/jsonx"
	"github.com/mihajlopopara/reqharbor/server/internal/store"
)

var (
	formContentType = regexp.MustCompile(`(?i)^application/x-www-form-urlencoded\s*(;|$)`)
	jsonContentType = regexp.MustCompile(`(?i)^application/json\s*(;|$)`)
)

// tokenFields reads the token request: application/x-www-form-urlencoded (RFC 6749 §4.1.3, what
// the desktop client sends) or application/json with the same names; anything else is 415.
func tokenFields(w http.ResponseWriter, r *http.Request) (map[string]string, error) {
	ct := r.Header.Get("Content-Type")
	switch {
	case formContentType.MatchString(ct):
		if r.ContentLength > httpx.MaxBodyBytes {
			return nil, httpx.TooLarge("payload_too_large", "Request body is too large")
		}
		body, err := io.ReadAll(http.MaxBytesReader(w, r.Body, 64<<10))
		if err != nil {
			var mbe *http.MaxBytesError
			if errors.As(err, &mbe) {
				return nil, httpx.TooLarge("payload_too_large", "Request body is too large")
			}
			return nil, invalidGrant("Request body could not be read")
		}
		vals, err := url.ParseQuery(string(body))
		if err != nil {
			return nil, invalidGrant("Request body is not a valid form")
		}
		out := map[string]string{}
		for k, v := range vals {
			if len(v) != 1 {
				return nil, invalidGrant("Parameter " + k + " is repeated")
			}
			out[k] = v[0]
		}
		return out, nil
	case jsonContentType.MatchString(ct):
		v, err := httpx.ReadJSONObject(w, r, 64<<10)
		if err != nil {
			return nil, err
		}
		out := map[string]string{}
		for i, k := range v.Keys {
			val := v.Vals[i]
			if val.Kind == jsonx.String {
				out[k] = val.S
			} else if !val.IsNull() {
				return nil, invalidGrant("Parameter " + k + " must be a string")
			}
		}
		return out, nil
	}
	return nil, httpx.NewError(415, "unsupported_media_type", "Content-Type must be application/x-www-form-urlencoded or application/json")
}

// pkceChallenge is BASE64URL(SHA256(verifier)) (RFC 7636 S256).
func pkceChallenge(verifier string) string {
	sum := sha256.Sum256([]byte(verifier))
	return base64.RawURLEncoding.EncodeToString(sum[:])
}

var verifierRe = regexp.MustCompile(`^[A-Za-z0-9._~-]{43,128}$`)

// token is POST /v1/auth/token (§5.8). Every grant failure is 400 invalid_grant.
func (s *Service) token(w http.ResponseWriter, r *http.Request) error {
	if err := httpx.CheckClientVersion(r, s.Config.MinClientVersion); err != nil {
		return err
	}
	bucket := s.bucket(r)
	if ok, retry := s.TokenLimiter.Allow("token:" + bucket); !ok {
		return httpx.RateLimited("Too many sign-in requests; try again later", retrySeconds(retry))
	}
	if ok, retry := s.FailedExchange.Check("token_fail:" + bucket); !ok {
		return httpx.RateLimited("Too many failed sign-in attempts from this address; try again later", retrySeconds(retry))
	}
	f, err := tokenFields(w, r)
	if err != nil {
		return err
	}
	resp, err := s.grant(r, f)
	if err != nil {
		if e, ok := httpx.AsAPIError(err); ok && (e.Code == "invalid_grant" || e.Code == "session_revoked") {
			s.FailedExchange.Allow("token_fail:" + bucket)
		}
		return err
	}
	return httpx.WriteJSON(w, http.StatusOK, resp, map[string]string{"Cache-Control": "no-store", "Pragma": "no-cache"})
}

func (s *Service) grant(r *http.Request, f map[string]string) (*TokenResponse, error) {
	if f["client_id"] != ClientID {
		return nil, invalidGrant("client_id must be " + ClientID)
	}
	switch f["grant_type"] {
	case "authorization_code":
		return s.exchange(r, f)
	case "refresh_token":
		return s.refresh(r.Context(), f["refresh_token"])
	case "":
		return nil, invalidGrant("grant_type is required")
	default:
		return nil, invalidGrant("grant_type must be authorization_code or refresh_token")
	}
}

// exchange consumes the code in one statement and only then checks the verifier, the redirect
// URI and the client, so a failed check still burns the code (§5.2).
func (s *Service) exchange(r *http.Request, f map[string]string) (*TokenResponse, error) {
	code, verifier, redirect := f["code"], f["code_verifier"], f["redirect_uri"]
	if code == "" || verifier == "" || redirect == "" {
		return nil, invalidGrant("code, code_verifier and redirect_uri are required")
	}
	if len(code) > 128 {
		return nil, invalidGrant("Authorization code is invalid or expired")
	}
	now := s.Clock.Now()
	var resp *TokenResponse
	var burned bool
	err := s.Store.Write(r.Context(), func(tx store.WriteTx) error {
		ar, err := tx.ConsumeAuthCode(ids.SHA256Hex(code), clock.Millis(now))
		if errors.Is(err, store.ErrNotFound) {
			return invalidGrant("Authorization code is invalid or expired")
		}
		if err != nil {
			return err
		}
		if !verifierRe.MatchString(verifier) || !ids.Equal(pkceChallenge(verifier), ar.CodeChallenge) ||
			redirect != ar.RedirectURI || ar.ClientID != ClientID || ar.UserID == nil {
			burned = true
			return nil // commit: the code stays consumed
		}
		u, err := tx.UserByID(*ar.UserID)
		if err != nil {
			return err
		}
		if u.DisabledAt != nil {
			burned = true
			return nil
		}
		resp, err = s.newSession(tx, u, ar.DeviceName, now)
		return err
	})
	if err != nil {
		return nil, err
	}
	if burned {
		return nil, invalidGrant("Authorization code is invalid or expired")
	}
	return resp, nil
}
