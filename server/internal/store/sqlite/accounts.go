package sqlite

import (
	"database/sql"
	"fmt"
	"strings"

	"github.com/mihajlopopara/reqharbor/server/internal/store"
)

func boolInt(b bool) int {
	if b {
		return 1
	}
	return 0
}

func nullStr(p *sql.NullString) *string {
	if !p.Valid {
		return nil
	}
	v := p.String
	return &v
}

func nullInt(p *sql.NullInt64) *int64 {
	if !p.Valid {
		return nil
	}
	v := p.Int64
	return &v
}

const userCols = "id, email, email_trusted, signup_via, name, avatar_url, is_admin, disabled_at, created_at, updated_at"

type scanner interface{ Scan(...any) error }

func scanUser(r scanner) (*store.User, error) {
	var u store.User
	var trusted, admin int64
	var avatar sql.NullString
	var disabled sql.NullInt64
	if err := r.Scan(&u.ID, &u.Email, &trusted, &u.SignupVia, &u.Name, &avatar, &admin, &disabled, &u.CreatedAt, &u.UpdatedAt); err != nil {
		return nil, notFound(err)
	}
	u.EmailTrusted, u.IsAdmin = trusted == 1, admin == 1
	u.AvatarURL, u.DisabledAt = nullStr(&avatar), nullInt(&disabled)
	return &u, nil
}

func (t *rtx) UserCount() (int64, error) {
	var n int64
	err := t.row("SELECT COUNT(*) FROM users").Scan(&n)
	return n, err
}

func (t *rtx) AdminExists() (bool, error) {
	var n int64
	err := t.row("SELECT EXISTS (SELECT 1 FROM users WHERE is_admin = 1)").Scan(&n)
	return n == 1, err
}

func (t *rtx) UserByEmail(email string) (*store.User, error) {
	return scanUser(t.row("SELECT "+userCols+" FROM users WHERE email = ?", email))
}

func (t *rtx) UserByID(id string) (*store.User, error) {
	return scanUser(t.row("SELECT "+userCols+" FROM users WHERE id = ?", id))
}

func (t *rtx) Users() ([]store.User, error) {
	rows, err := t.tx.QueryContext(t.ctx, "SELECT "+userCols+" FROM users ORDER BY created_at, id")
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var out []store.User
	for rows.Next() {
		u, err := scanUser(rows)
		if err != nil {
			return nil, err
		}
		out = append(out, *u)
	}
	return out, rows.Err()
}

func (t *rtx) PasswordPHC(userID string) (string, error) {
	var phc string
	if err := t.row("SELECT phc FROM password_credentials WHERE user_id = ?", userID).Scan(&phc); err != nil {
		return "", notFound(err)
	}
	return phc, nil
}

func (t *rtx) IdentityOf(userID string) (*store.Identity, error) {
	var i store.Identity
	err := t.row("SELECT provider, issuer, subject, user_id, email, created_at, last_login_at FROM identities WHERE user_id = ?", userID).
		Scan(&i.Provider, &i.Issuer, &i.Subject, &i.UserID, &i.Email, &i.CreatedAt, &i.LastLoginAt)
	if err != nil {
		return nil, notFound(err)
	}
	return &i, nil
}

func (t *rtx) IdentityBySubject(provider, issuer, subject string) (*store.Identity, error) {
	var i store.Identity
	err := t.row(`SELECT provider, issuer, subject, user_id, email, created_at, last_login_at FROM identities
	              WHERE provider = ? AND issuer = ? AND subject = ?`, provider, issuer, subject).
		Scan(&i.Provider, &i.Issuer, &i.Subject, &i.UserID, &i.Email, &i.CreatedAt, &i.LastLoginAt)
	if err != nil {
		return nil, notFound(err)
	}
	return &i, nil
}

func (t *rtx) PersonalSpace(userID string) (*store.Personal, error) {
	var p store.Personal
	err := t.row(`SELECT t.id, w.id, w.name, w.created_at FROM teams t JOIN workspaces w ON w.team_id = t.id
	              WHERE t.owner_user_id = ? AND t.personal = 1`, userID).Scan(&p.TeamID, &p.WorkspaceID, &p.Name, &p.CreatedAt)
	if err != nil {
		return nil, notFound(err)
	}
	return &p, nil
}

const authRequestCols = `id, browser_hash, client_id, redirect_uri, code_challenge, client_state, device_name, method_hint,
	grant_token_hash, provider, upstream_verifier, password_attempts, code_attempts, user_id, code_hash, code_expires_at, created_at, expires_at, consumed_at, ticket_hash`

func scanAuthRequest(r scanner) (*store.AuthRequest, error) {
	var a store.AuthRequest
	var hint, grant, provider, verifier, user, code, ticket sql.NullString
	var codeExp, consumed sql.NullInt64
	err := r.Scan(&a.ID, &a.BrowserHash, &a.ClientID, &a.RedirectURI, &a.CodeChallenge, &a.ClientState, &a.DeviceName, &hint,
		&grant, &provider, &verifier, &a.PasswordAttempts, &a.CodeAttempts, &user, &code, &codeExp, &a.CreatedAt, &a.ExpiresAt, &consumed, &ticket)
	if err != nil {
		return nil, notFound(err)
	}
	a.MethodHint, a.GrantTokenHash, a.Provider, a.UserID, a.CodeHash = nullStr(&hint), nullStr(&grant), nullStr(&provider), nullStr(&user), nullStr(&code)
	a.UpstreamVerifier, a.TicketHash = nullStr(&verifier), nullStr(&ticket)
	a.CodeExpiresAt, a.ConsumedAt = nullInt(&codeExp), nullInt(&consumed)
	return &a, nil
}

func (t *rtx) AuthRequest(id string) (*store.AuthRequest, error) {
	return scanAuthRequest(t.row("SELECT "+authRequestCols+" FROM auth_requests WHERE id = ?", id))
}

func (t *rtx) AuthRequestByUpstreamState(stateHash string) (*store.AuthRequest, error) {
	return scanAuthRequest(t.row("SELECT "+authRequestCols+" FROM auth_requests WHERE upstream_state_hash = ?", stateHash))
}

const tokenCols = "token_hash, id, purpose, user_id, email, max_uses, uses, created_by, created_at, expires_at, revoked_at"

func scanToken(r scanner) (*store.OneTimeToken, error) {
	var o store.OneTimeToken
	var user, email, by sql.NullString
	var revoked sql.NullInt64
	if err := r.Scan(&o.TokenHash, &o.ID, &o.Purpose, &user, &email, &o.MaxUses, &o.Uses, &by, &o.CreatedAt, &o.ExpiresAt, &revoked); err != nil {
		return nil, notFound(err)
	}
	o.UserID, o.Email, o.CreatedBy, o.RevokedAt = nullStr(&user), nullStr(&email), nullStr(&by), nullInt(&revoked)
	return &o, nil
}

func (t *rtx) OneTimeToken(hash string) (*store.OneTimeToken, error) {
	return scanToken(t.row("SELECT "+tokenCols+" FROM one_time_tokens WHERE token_hash = ?", hash))
}

func (t *rtx) OneTimeTokens(purpose string) ([]store.OneTimeToken, error) {
	rows, err := t.tx.QueryContext(t.ctx, "SELECT "+tokenCols+" FROM one_time_tokens WHERE purpose = ? ORDER BY created_at, id", purpose)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var out []store.OneTimeToken
	for rows.Next() {
		o, err := scanToken(rows)
		if err != nil {
			return nil, err
		}
		out = append(out, *o)
	}
	return out, rows.Err()
}

const sessionColsBare = `id, user_id, client_id, device_name, access_hash, access_expires_at, refresh_hash,
	prev_refresh_hash, pending_pair_ct, pending_until, created_at, last_used_day, idle_expires_at, expires_at, revoked_at`

// sessionCols is sessionColsBare qualified with the alias s.
var sessionCols = func() string {
	parts := strings.Split(sessionColsBare, ",")
	for i, p := range parts {
		parts[i] = "s." + strings.TrimSpace(p)
	}
	return strings.Join(parts, ", ")
}()

func sessionDest(s *store.Session, prev, ct *sql.NullString, until, revoked *sql.NullInt64) []any {
	return []any{&s.ID, &s.UserID, &s.ClientID, &s.DeviceName, &s.AccessHash, &s.AccessExpiresAt, &s.RefreshHash,
		prev, ct, until, &s.CreatedAt, &s.LastUsedDay, &s.IdleExpiresAt, &s.ExpiresAt, revoked}
}

func scanSession(r scanner) (*store.Session, error) {
	var s store.Session
	var prev, ct sql.NullString
	var until, revoked sql.NullInt64
	if err := r.Scan(sessionDest(&s, &prev, &ct, &until, &revoked)...); err != nil {
		return nil, notFound(err)
	}
	s.PrevRefreshHash, s.PendingPairCT, s.PendingUntil, s.RevokedAt = nullStr(&prev), nullStr(&ct), nullInt(&until), nullInt(&revoked)
	return &s, nil
}

func (t *rtx) SessionByAccessHash(hash string) (*store.Session, *store.User, error) {
	var s store.Session
	var prev, ct sql.NullString
	var until, revoked sql.NullInt64
	var u store.User
	var trusted, admin int64
	var avatar sql.NullString
	var disabled sql.NullInt64
	dest := append(sessionDest(&s, &prev, &ct, &until, &revoked),
		&u.ID, &u.Email, &trusted, &u.SignupVia, &u.Name, &avatar, &admin, &disabled, &u.CreatedAt, &u.UpdatedAt)
	err := t.row(`SELECT `+sessionCols+`, u.id, u.email, u.email_trusted, u.signup_via, u.name, u.avatar_url, u.is_admin,
	                     u.disabled_at, u.created_at, u.updated_at
	              FROM sessions s JOIN users u ON u.id = s.user_id WHERE s.access_hash = ?`, hash).Scan(dest...)
	if err != nil {
		return nil, nil, notFound(err)
	}
	s.PrevRefreshHash, s.PendingPairCT, s.PendingUntil, s.RevokedAt = nullStr(&prev), nullStr(&ct), nullInt(&until), nullInt(&revoked)
	u.EmailTrusted, u.IsAdmin = trusted == 1, admin == 1
	u.AvatarURL, u.DisabledAt = nullStr(&avatar), nullInt(&disabled)
	return &s, &u, nil
}

func (t *rtx) SessionByRefreshHash(hash string) (*store.Session, error) {
	return scanSession(t.row("SELECT "+sessionCols+" FROM sessions s WHERE s.refresh_hash = ?", hash))
}

func (t *rtx) Session(id string) (*store.Session, error) {
	return scanSession(t.row("SELECT "+sessionCols+" FROM sessions s WHERE s.id = ?", id))
}

func (t *rtx) SessionByPrevRefreshHash(hash string) (*store.Session, error) {
	return scanSession(t.row("SELECT "+sessionCols+" FROM sessions s WHERE s.prev_refresh_hash = ?", hash))
}

func (t *rtx) UserSessions(userID string) ([]store.Session, error) {
	rows, err := t.tx.QueryContext(t.ctx, "SELECT "+sessionCols+" FROM sessions s WHERE s.user_id = ? ORDER BY s.created_at, s.id", userID)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var out []store.Session
	for rows.Next() {
		s, err := scanSession(rows)
		if err != nil {
			return nil, err
		}
		out = append(out, *s)
	}
	return out, rows.Err()
}

func (t *rtx) Throttle(keyHash string) (*store.Throttle, error) {
	var th store.Throttle
	err := t.row("SELECT key_hash, email_key, failed_count, locked_until, last_failed_at FROM login_throttle WHERE key_hash = ?", keyHash).
		Scan(&th.KeyHash, &th.EmailKey, &th.FailedCount, &th.LockedUntil, &th.LastFailedAt)
	if err != nil {
		return nil, notFound(err)
	}
	return &th, nil
}

func (t *rtx) ThrottleExists(emailKey string) (bool, error) {
	var n int64
	err := t.row("SELECT EXISTS (SELECT 1 FROM login_throttle WHERE email_key = ?)", emailKey).Scan(&n)
	return n == 1, err
}

// --- writes ---

func (t *wtx) InsertUser(u store.User) error {
	_, err := t.exec("INSERT INTO users ("+userCols+") VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
		u.ID, u.Email, boolInt(u.EmailTrusted), u.SignupVia, u.Name, u.AvatarURL, boolInt(u.IsAdmin), u.DisabledAt, u.CreatedAt, u.UpdatedAt)
	return err
}

func one(res sql.Result, err error) error {
	if err != nil {
		return err
	}
	if n, _ := res.RowsAffected(); n != 1 {
		return store.ErrNotFound
	}
	return nil
}

func (t *wtx) SetUserAdmin(id string, admin bool, now int64) error {
	return one(t.exec("UPDATE users SET is_admin = ?, updated_at = ? WHERE id = ?", boolInt(admin), now, id))
}

func (t *wtx) SetUserDisabled(id string, disabledAt *int64, now int64) error {
	return one(t.exec("UPDATE users SET disabled_at = ?, updated_at = ? WHERE id = ?", disabledAt, now, id))
}

func (t *wtx) SetPassword(userID, phc string, now int64) error {
	_, err := t.exec(`INSERT INTO password_credentials (user_id, phc, updated_at) VALUES (?, ?, ?)
	     ON CONFLICT (user_id) DO UPDATE SET phc = excluded.phc, updated_at = excluded.updated_at`, userID, phc, now)
	return err
}

func (t *wtx) InsertPassword(userID, phc string, now int64) error {
	_, err := t.exec("INSERT INTO password_credentials (user_id, phc, updated_at) VALUES (?, ?, ?)", userID, phc, now)
	return err
}

func (t *wtx) InsertIdentity(i store.Identity) error {
	_, err := t.exec(`INSERT INTO identities (provider, issuer, subject, user_id, email, created_at, last_login_at)
	     VALUES (?, ?, ?, ?, ?, ?, ?)`, i.Provider, i.Issuer, i.Subject, i.UserID, i.Email, i.CreatedAt, i.LastLoginAt)
	return err
}

func (t *wtx) TouchIdentity(provider, issuer, subject, email string, now int64) error {
	return one(t.exec(`UPDATE identities SET email = CASE WHEN ?1 = '' THEN email ELSE ?1 END, last_login_at = ?2
	     WHERE provider = ?3 AND issuer = ?4 AND subject = ?5`, email, now, provider, issuer, subject))
}

func (t *wtx) SetUserProfile(id, name string, avatarURL *string, now int64) error {
	return one(t.exec("UPDATE users SET name = ?, avatar_url = ?, updated_at = ? WHERE id = ?", name, avatarURL, now, id))
}

func (t *wtx) SetUserEmail(id, email string, trusted bool, now int64) error {
	return one(t.exec("UPDATE users SET email = ?, email_trusted = ?, updated_at = ? WHERE id = ?", email, boolInt(trusted), now, id))
}

func (t *wtx) InsertPersonal(userID string, p store.Personal) error {
	if _, err := t.exec("INSERT INTO teams (id, name, personal, owner_user_id, created_at) VALUES (?, ?, 1, ?, ?)",
		p.TeamID, p.Name, userID, p.CreatedAt); err != nil {
		return err
	}
	if _, err := t.exec("INSERT INTO team_members (team_id, user_id, role, joined_at) VALUES (?, ?, 'owner', ?)",
		p.TeamID, userID, p.CreatedAt); err != nil {
		return err
	}
	// The sentinel token_hash is never a SHA-256 hex digest, so legacy auth can never match it.
	_, err := t.exec("INSERT INTO workspaces (id, name, token_hash, rev, created_at, team_id) VALUES (?, ?, 'space:' || ?, 0, ?, ?)",
		p.WorkspaceID, p.Name, p.WorkspaceID, p.CreatedAt, p.TeamID)
	return err
}

func (t *wtx) InsertAuthRequest(a store.AuthRequest) error {
	_, err := t.exec(`INSERT INTO auth_requests (id, browser_hash, client_id, redirect_uri, code_challenge, client_state,
	       device_name, method_hint, created_at, expires_at, ticket_hash) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
		a.ID, a.BrowserHash, a.ClientID, a.RedirectURI, a.CodeChallenge, a.ClientState, a.DeviceName, a.MethodHint, a.CreatedAt, a.ExpiresAt, a.TicketHash)
	return err
}

func (t *wtx) SetAuthRequestGrant(id, grantHash string) error {
	return one(t.exec("UPDATE auth_requests SET grant_token_hash = ? WHERE id = ?", grantHash, id))
}

func (t *wtx) SetAuthRequestUpstream(id, provider, stateHash, verifier string) error {
	return one(t.exec(`UPDATE auth_requests SET provider = ?, upstream_state_hash = ?, upstream_verifier = ?
	     WHERE id = ? AND consumed_at IS NULL AND code_hash IS NULL`, provider, stateHash, verifier, id))
}

func (t *wtx) ClearAuthRequestUpstream(id string) error {
	_, err := t.exec("UPDATE auth_requests SET upstream_state_hash = NULL, upstream_verifier = NULL WHERE id = ?", id)
	return err
}

func (t *wtx) ConsumeAuthRequest(id string, now int64) error {
	_, err := t.exec("UPDATE auth_requests SET consumed_at = ? WHERE id = ? AND consumed_at IS NULL", now, id)
	return err
}

func (t *wtx) IncrAuthRequestAttempts(id, field string) (int64, error) {
	if field != "password_attempts" && field != "code_attempts" {
		return 0, fmt.Errorf("unknown attempts field %q", field)
	}
	var n int64
	err := t.row("UPDATE auth_requests SET "+field+" = "+field+" + 1 WHERE id = ? RETURNING "+field, id).Scan(&n)
	return n, notFound(err)
}

func (t *wtx) SetAuthRequestCode(id, userID, codeHash string, codeExpiresAt int64) error {
	return one(t.exec("UPDATE auth_requests SET user_id = ?, code_hash = ?, code_expires_at = ? WHERE id = ? AND consumed_at IS NULL",
		userID, codeHash, codeExpiresAt, id))
}

func (t *wtx) ConsumeAuthCode(codeHash string, now int64) (*store.AuthRequest, error) {
	return scanAuthRequest(t.row(`UPDATE auth_requests SET consumed_at = ?1
	     WHERE code_hash = ?2 AND consumed_at IS NULL AND code_expires_at > ?1 RETURNING `+authRequestCols, now, codeHash))
}

func (t *wtx) InsertOneTimeToken(o store.OneTimeToken) error {
	_, err := t.exec("INSERT INTO one_time_tokens ("+tokenCols+") VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
		o.TokenHash, o.ID, o.Purpose, o.UserID, o.Email, o.MaxUses, o.Uses, o.CreatedBy, o.CreatedAt, o.ExpiresAt, o.RevokedAt)
	return err
}

func (t *wtx) UseOneTimeToken(hash string, now int64) (bool, error) {
	res, err := t.exec(`UPDATE one_time_tokens SET uses = uses + 1
	     WHERE token_hash = ?1 AND uses < max_uses AND expires_at > ?2 AND revoked_at IS NULL`, hash, now)
	if err != nil {
		return false, err
	}
	n, err := res.RowsAffected()
	return n == 1, err
}

func (t *wtx) RevokeOneTimeToken(id string, now int64) (bool, error) {
	res, err := t.exec("UPDATE one_time_tokens SET revoked_at = ? WHERE id = ? AND revoked_at IS NULL", now, id)
	if err != nil {
		return false, err
	}
	n, err := res.RowsAffected()
	return n == 1, err
}

func (t *wtx) DeleteOneTimeTokens(purpose string) error {
	_, err := t.exec("DELETE FROM one_time_tokens WHERE purpose = ?", purpose)
	return err
}

func (t *wtx) InsertSession(s store.Session) error {
	_, err := t.exec(`INSERT INTO sessions (id, user_id, client_id, device_name, access_hash, access_expires_at, refresh_hash,
	       created_at, last_used_day, idle_expires_at, expires_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
		s.ID, s.UserID, s.ClientID, s.DeviceName, s.AccessHash, s.AccessExpiresAt, s.RefreshHash,
		s.CreatedAt, s.LastUsedDay, s.IdleExpiresAt, s.ExpiresAt)
	return err
}

func (t *wtx) RevokeSessionsBeyond(userID string, keep, now int64) error {
	_, err := t.exec(`UPDATE sessions SET revoked_at = ?1 WHERE id IN (
	       SELECT id FROM sessions WHERE user_id = ?2 AND revoked_at IS NULL AND expires_at > ?1
	       ORDER BY created_at DESC, id DESC LIMIT -1 OFFSET ?3)`, now, userID, keep)
	return err
}

func (t *wtx) RotateSession(r store.Rotation) (*store.Session, error) {
	return scanSession(t.row(`UPDATE sessions SET prev_refresh_hash = refresh_hash, refresh_hash = ?1, access_hash = ?2,
	       access_expires_at = ?3, idle_expires_at = ?4, pending_pair_ct = ?5, pending_until = ?6
	     WHERE refresh_hash = ?7 AND revoked_at IS NULL AND idle_expires_at > ?8 AND expires_at > ?8
	     RETURNING `+sessionColsBare,
		r.NewRefreshHash, r.NewAccessHash, r.AccessExpiresAt, r.IdleExpiresAt, r.PendingPairCT, r.PendingUntil, r.OldRefreshHash, r.Now))
}

func (t *wtx) RevokeSession(id string, now int64) error {
	_, err := t.exec("UPDATE sessions SET revoked_at = ? WHERE id = ? AND revoked_at IS NULL", now, id)
	return err
}

func (t *wtx) RevokeUserSession(id, userID string, now int64) (bool, error) {
	res, err := t.exec("UPDATE sessions SET revoked_at = ? WHERE id = ? AND user_id = ? AND revoked_at IS NULL", now, id, userID)
	if err != nil {
		return false, err
	}
	n, err := res.RowsAffected()
	return n == 1, err
}

func (t *wtx) RevokeUserSessions(userID string, now int64) (int64, error) {
	res, err := t.exec("UPDATE sessions SET revoked_at = ? WHERE user_id = ? AND revoked_at IS NULL", now, userID)
	if err != nil {
		return 0, err
	}
	return res.RowsAffected()
}

func (t *wtx) SetSessionLastUsedDay(id string, day int64) error {
	_, err := t.exec("UPDATE sessions SET last_used_day = ? WHERE id = ?", day, id)
	return err
}

func (t *wtx) PutThrottle(th store.Throttle) error {
	_, err := t.exec(`INSERT INTO login_throttle (key_hash, email_key, failed_count, locked_until, last_failed_at) VALUES (?, ?, ?, ?, ?)
	     ON CONFLICT (key_hash) DO UPDATE SET failed_count = excluded.failed_count, locked_until = excluded.locked_until,
	       last_failed_at = excluded.last_failed_at`, th.KeyHash, th.EmailKey, th.FailedCount, th.LockedUntil, th.LastFailedAt)
	return err
}

func (t *wtx) DeleteThrottle(keyHash string) error {
	_, err := t.exec("DELETE FROM login_throttle WHERE key_hash = ?", keyHash)
	return err
}

func (t *wtx) DeleteThrottleByEmail(emailKey string) (int64, error) {
	res, err := t.exec("DELETE FROM login_throttle WHERE email_key = ?", emailKey)
	if err != nil {
		return 0, err
	}
	return res.RowsAffected()
}

func (t *wtx) GC(now, deadBefore, throttleBefore int64) (store.GCStats, error) {
	var st store.GCStats
	steps := []struct {
		dst  *int64
		q    string
		args []any
	}{
		{&st.AuthRequests, "DELETE FROM auth_requests WHERE expires_at <= ?", []any{now}},
		{&st.Tokens, "DELETE FROM one_time_tokens WHERE expires_at <= ?1 OR revoked_at <= ?1", []any{deadBefore}},
		{&st.Sessions, "DELETE FROM sessions WHERE expires_at <= ?1 OR revoked_at <= ?1 OR idle_expires_at <= ?1", []any{deadBefore}},
		{&st.Throttle, "DELETE FROM login_throttle WHERE last_failed_at <= ?", []any{throttleBefore}},
	}
	for _, s := range steps {
		res, err := t.exec(s.q, s.args...)
		if err != nil {
			return st, err
		}
		*s.dst, _ = res.RowsAffected()
	}
	return st, nil
}
