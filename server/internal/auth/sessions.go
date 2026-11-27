package auth

import (
	"context"
	"crypto/aes"
	"crypto/cipher"
	"crypto/hkdf"
	"crypto/rand"
	"crypto/sha256"
	"encoding/base64"
	"encoding/json"
	"errors"
	"net/http"
	"regexp"
	"time"

	"github.com/mihajlopopara/reqharbor/server/internal/clock"
	"github.com/mihajlopopara/reqharbor/server/internal/httpx"
	"github.com/mihajlopopara/reqharbor/server/internal/ids"
	"github.com/mihajlopopara/reqharbor/server/internal/store"
)

var (
	bearerRe  = regexp.MustCompile(`(?i)^Bearer\s+(\S+)\s*$`)
	accessRe  = regexp.MustCompile(`^rha_[A-Za-z0-9_-]{43}$`)
	refreshRe = regexp.MustCompile(`^rhr_[A-Za-z0-9_-]{43}$`)
)

const dayMillis = 24 * 60 * 60 * 1000

// UserJSON is the user object of the token response and of /v1/me.
type UserJSON struct {
	ID        string  `json:"id"`
	Email     string  `json:"email"`
	Name      string  `json:"name"`
	AvatarURL *string `json:"avatarUrl"`
	IsAdmin   bool    `json:"isAdmin"`
}

// UserView converts a stored user.
func UserView(u *store.User) UserJSON {
	return UserJSON{ID: u.ID, Email: u.Email, Name: u.Name, AvatarURL: u.AvatarURL, IsAdmin: u.IsAdmin}
}

// TokenResponse is RFC 6749 §5.1 plus session_id and user (§5.8).
type TokenResponse struct {
	AccessToken  string   `json:"access_token"`
	RefreshToken string   `json:"refresh_token"`
	TokenType    string   `json:"token_type"`
	ExpiresIn    int64    `json:"expires_in"`
	SessionID    string   `json:"session_id"`
	User         UserJSON `json:"user"`
}

// pendingPair is what the grace window returns again, sealed with the old refresh token.
type pendingPair struct {
	AccessToken     string `json:"accessToken"`
	RefreshToken    string `json:"refreshToken"`
	AccessExpiresAt int64  `json:"accessExpiresAt"`
}

func graceKey(oldRefresh, sessionID string) ([]byte, error) {
	return hkdf.Key(sha256.New, []byte(oldRefresh), []byte(sessionID), "rh-refresh-grace", 32)
}

func sealPair(oldRefresh, sessionID string, p pendingPair) (string, error) {
	key, err := graceKey(oldRefresh, sessionID)
	if err != nil {
		return "", err
	}
	block, err := aes.NewCipher(key)
	if err != nil {
		return "", err
	}
	gcm, err := cipher.NewGCM(block)
	if err != nil {
		return "", err
	}
	nonce := make([]byte, gcm.NonceSize())
	if _, err := rand.Read(nonce); err != nil {
		return "", err
	}
	plain, _ := json.Marshal(p)
	return base64.RawStdEncoding.EncodeToString(gcm.Seal(nonce, nonce, plain, []byte(sessionID))), nil
}

func openPair(oldRefresh, sessionID, ct string) (*pendingPair, error) {
	raw, err := base64.RawStdEncoding.DecodeString(ct)
	if err != nil {
		return nil, err
	}
	key, err := graceKey(oldRefresh, sessionID)
	if err != nil {
		return nil, err
	}
	block, err := aes.NewCipher(key)
	if err != nil {
		return nil, err
	}
	gcm, err := cipher.NewGCM(block)
	if err != nil {
		return nil, err
	}
	if len(raw) < gcm.NonceSize() {
		return nil, errors.New("auth: short pending pair")
	}
	plain, err := gcm.Open(nil, raw[:gcm.NonceSize()], raw[gcm.NonceSize():], []byte(sessionID))
	if err != nil {
		return nil, err
	}
	var p pendingPair
	return &p, json.Unmarshal(plain, &p)
}

// newSession inserts a session for user inside tx and applies the per-user cap.
func (s *Service) newSession(tx store.WriteTx, u *store.User, deviceName string, now time.Time) (*TokenResponse, error) {
	access, refresh := ids.Token("rha_"), ids.Token("rhr_")
	ms := clock.Millis(now)
	sess := store.Session{
		ID: ids.ULID(now), UserID: u.ID, ClientID: ClientID, DeviceName: deviceName,
		AccessHash: ids.SHA256Hex(access), AccessExpiresAt: clock.Millis(now.Add(AccessTTL)),
		RefreshHash: ids.SHA256Hex(refresh), CreatedAt: ms, LastUsedDay: ms / dayMillis,
		IdleExpiresAt: clock.Millis(now.Add(RefreshIdleTTL)), ExpiresAt: clock.Millis(now.Add(RefreshMaxTTL)),
	}
	if err := tx.InsertSession(sess); err != nil {
		return nil, err
	}
	if err := tx.RevokeSessionsBeyond(u.ID, s.Config.SessionMax, ms); err != nil {
		return nil, err
	}
	return &TokenResponse{AccessToken: access, RefreshToken: refresh, TokenType: "Bearer",
		ExpiresIn: int64(AccessTTL / time.Second), SessionID: sess.ID, User: UserView(u)}, nil
}

func invalidGrant(message string) *httpx.APIError {
	return httpx.NewError(400, "invalid_grant", message)
}

func unauthorized(code, message string) *httpx.APIError {
	return httpx.NewError(401, code, message).WithHeader("WWW-Authenticate", `Bearer realm="reqharbor", error="invalid_token"`)
}

// refresh rotates a refresh token (§5.8): one compare-and-swap; within 60 s the superseded token
// returns the identical pair; after that it is reuse and revokes the session.
func (s *Service) refresh(ctx context.Context, token string) (*TokenResponse, error) {
	if !refreshRe.MatchString(token) {
		return nil, invalidGrant("Refresh token is invalid or expired")
	}
	oldHash := ids.SHA256Hex(token)
	now := s.Clock.Now()
	ms := clock.Millis(now)
	var resp *TokenResponse
	reused := false
	err := s.Store.Write(ctx, func(tx store.WriteTx) error {
		sess, err := tx.SessionByRefreshHash(oldHash)
		if errors.Is(err, store.ErrNotFound) {
			prev, err := tx.SessionByPrevRefreshHash(oldHash)
			if errors.Is(err, store.ErrNotFound) {
				return invalidGrant("Refresh token is invalid or expired")
			}
			if err != nil {
				return err
			}
			if prev.RevokedAt != nil {
				return unauthorized("session_revoked", "This session was signed out; sign in again")
			}
			if prev.PendingUntil != nil && ms < *prev.PendingUntil && prev.PendingPairCT != nil {
				pair, err := openPair(token, prev.ID, *prev.PendingPairCT)
				if err != nil {
					return invalidGrant("Refresh token is invalid or expired")
				}
				u, err := tx.UserByID(prev.UserID)
				if err != nil {
					return err
				}
				if u.DisabledAt != nil {
					return invalidGrant("Refresh token is invalid or expired")
				}
				resp = &TokenResponse{AccessToken: pair.AccessToken, RefreshToken: pair.RefreshToken, TokenType: "Bearer",
					ExpiresIn: max(0, (pair.AccessExpiresAt-ms)/1000), SessionID: prev.ID, User: UserView(u)}
				return nil
			}
			// Reuse after the grace window: someone else holds a copy. The session is revoked in
			// this transaction, which therefore commits (returns nil); the 401 is built below. A
			// second transaction could miss the session if a rotation committed in between.
			if err := tx.RevokeSession(prev.ID, ms); err != nil {
				return err
			}
			reused = true
			return nil
		}
		if err != nil {
			return err
		}
		if sess.RevokedAt != nil || sess.IdleExpiresAt <= ms || sess.ExpiresAt <= ms {
			return invalidGrant("Refresh token is invalid or expired")
		}
		u, err := tx.UserByID(sess.UserID)
		if err != nil {
			return err
		}
		if u.DisabledAt != nil {
			return invalidGrant("Refresh token is invalid or expired")
		}
		access, newRefresh := ids.Token("rha_"), ids.Token("rhr_")
		accessExp := clock.Millis(now.Add(AccessTTL))
		ct, err := sealPair(token, sess.ID, pendingPair{AccessToken: access, RefreshToken: newRefresh, AccessExpiresAt: accessExp})
		if err != nil {
			return err
		}
		idle := min(clock.Millis(now.Add(RefreshIdleTTL)), sess.ExpiresAt)
		if _, err := tx.RotateSession(store.Rotation{
			OldRefreshHash: oldHash, NewRefreshHash: ids.SHA256Hex(newRefresh), NewAccessHash: ids.SHA256Hex(access),
			AccessExpiresAt: accessExp, IdleExpiresAt: idle, PendingPairCT: ct,
			PendingUntil: clock.Millis(now.Add(GraceWindow)), Now: ms,
		}); err != nil {
			if errors.Is(err, store.ErrNotFound) {
				return invalidGrant("Refresh token is invalid or expired")
			}
			return err
		}
		resp = &TokenResponse{AccessToken: access, RefreshToken: newRefresh, TokenType: "Bearer",
			ExpiresIn: int64(AccessTTL / time.Second), SessionID: sess.ID, User: UserView(u)}
		return nil
	})
	if err == nil && reused {
		s.Logger.Warn("refresh token reused after the grace window; session revoked")
		return nil, unauthorized("session_revoked", "This session was signed out because its sign-in was used on another device; sign in again")
	}
	if err != nil {
		return nil, err
	}
	return resp, nil
}

// Principal is an authenticated access token.
type Principal struct {
	Session *store.Session
	User    *store.User
}

// Authenticate resolves `Authorization: Bearer rha_…` (§5.8): one indexed read joined to users,
// which also rejects disabled users; last_used_day is written only when the day changes.
func (s *Service) Authenticate(r *http.Request) (*Principal, error) {
	m := bearerRe.FindStringSubmatch(r.Header.Get("Authorization"))
	if m == nil || !accessRe.MatchString(m[1]) {
		return nil, unauthorized("auth_required", "Sign in to continue")
	}
	hash := ids.SHA256Hex(m[1])
	var p Principal
	err := s.Store.Read(r.Context(), func(tx store.ReadTx) error {
		var err error
		p.Session, p.User, err = tx.SessionByAccessHash(hash)
		return err
	})
	if errors.Is(err, store.ErrNotFound) {
		return nil, unauthorized("auth_required", "Sign in to continue")
	}
	if err != nil {
		return nil, err
	}
	if !ids.Equal(p.Session.AccessHash, hash) || p.Session.RevokedAt != nil || p.User.DisabledAt != nil {
		return nil, unauthorized("auth_required", "Sign in to continue")
	}
	ms := clock.Millis(s.Clock.Now())
	if p.Session.AccessExpiresAt <= ms {
		return nil, unauthorized("session_expired", "Your sign-in expired; refresh it")
	}
	if day := ms / dayMillis; day != p.Session.LastUsedDay {
		if err := s.Store.Write(r.Context(), func(tx store.WriteTx) error { return tx.SetSessionLastUsedDay(p.Session.ID, day) }); err != nil {
			return nil, err
		}
		p.Session.LastUsedDay = day
	}
	return &p, nil
}

// logout revokes the calling session: 204.
func (s *Service) logout(w http.ResponseWriter, r *http.Request) error {
	p, err := s.Authenticate(r)
	if err != nil {
		return err
	}
	if err := s.Store.Write(r.Context(), func(tx store.WriteTx) error {
		return tx.RevokeSession(p.Session.ID, clock.Millis(s.Clock.Now()))
	}); err != nil {
		return err
	}
	w.WriteHeader(http.StatusNoContent)
	return nil
}
