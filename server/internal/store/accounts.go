package store

// Account rows (0004_accounts.sql, 06-go-server.md §8.3). All timestamps are Unix milliseconds.

// User is one account.
type User struct {
	ID           string
	Email        string
	EmailTrusted bool
	SignupVia    string // setup, invite, open, cli, admin_email
	Name         string
	AvatarURL    *string
	IsAdmin      bool
	DisabledAt   *int64
	CreatedAt    int64
	UpdatedAt    int64
}

// Identity is an external sign-in identity (S5).
type Identity struct {
	Provider    string
	Issuer      string
	Subject     string
	UserID      string
	Email       string
	CreatedAt   int64
	LastLoginAt int64
}

// Personal is the personal team, its owner membership and its space, created with the user.
type Personal struct {
	TeamID      string
	WorkspaceID string
	Name        string
	CreatedAt   int64
}

// Session is one signed-in device.
type Session struct {
	ID              string
	UserID          string
	ClientID        string
	DeviceName      string
	AccessHash      string
	AccessExpiresAt int64
	RefreshHash     string
	PrevRefreshHash *string
	PendingPairCT   *string
	PendingUntil    *int64
	CreatedAt       int64
	LastUsedDay     int64
	IdleExpiresAt   int64
	ExpiresAt       int64
	RevokedAt       *int64
}

// Rotation is the compare-and-swap refresh of a session (§5.8).
type Rotation struct {
	OldRefreshHash  string
	NewRefreshHash  string
	NewAccessHash   string
	AccessExpiresAt int64
	IdleExpiresAt   int64
	PendingPairCT   string
	PendingUntil    int64
	Now             int64
}

// AuthRequest is one browser sign-in started by the app (`?ar=`).
type AuthRequest struct {
	ID               string
	BrowserHash      string
	ClientID         string
	RedirectURI      string
	CodeChallenge    string
	ClientState      string
	DeviceName       string
	MethodHint       *string
	GrantTokenHash   *string
	Provider         *string
	PasswordAttempts int64
	CodeAttempts     int64
	UserID           *string
	CodeHash         *string
	CodeExpiresAt    *int64
	CreatedAt        int64
	ExpiresAt        int64
	ConsumedAt       *int64
}

// One-time token purposes.
const (
	PurposeSetup         = "setup"
	PurposeSignupInvite  = "signup_invite"
	PurposeResetPassword = "reset_password"
)

// OneTimeToken is a setup code, an invite code or a password-reset token.
type OneTimeToken struct {
	TokenHash string
	ID        string
	Purpose   string
	UserID    *string
	Email     *string
	MaxUses   int64
	Uses      int64
	CreatedBy *string
	CreatedAt int64
	ExpiresAt int64
	RevokedAt *int64
}

// Usable reports whether the token can still be used at now.
func (t *OneTimeToken) Usable(now int64) bool {
	return t.RevokedAt == nil && t.Uses < t.MaxUses && t.ExpiresAt > now
}

// Throttle is one (email, IP bucket) password-throttle row (§5.9).
type Throttle struct {
	KeyHash      string
	EmailKey     string
	FailedCount  int64
	LockedUntil  int64
	LastFailedAt int64
}

// AccountsReadTx are the account queries.
type AccountsReadTx interface {
	UserCount() (int64, error)
	AdminExists() (bool, error)
	UserByEmail(email string) (*User, error)
	UserByID(id string) (*User, error)
	Users() ([]User, error)
	// PasswordPHC returns the user's password hash, or ErrNotFound for a provider-only account.
	PasswordPHC(userID string) (string, error)
	// IdentityOf returns the user's external identity, or ErrNotFound.
	IdentityOf(userID string) (*Identity, error)
	PersonalSpace(userID string) (*Personal, error)

	AuthRequest(id string) (*AuthRequest, error)
	OneTimeToken(hash string) (*OneTimeToken, error)
	OneTimeTokens(purpose string) ([]OneTimeToken, error)

	// SessionByAccessHash returns the session and its user.
	SessionByAccessHash(hash string) (*Session, *User, error)
	SessionByRefreshHash(hash string) (*Session, error)
	SessionByPrevRefreshHash(hash string) (*Session, error)
	Session(id string) (*Session, error)
	UserSessions(userID string) ([]Session, error)

	Throttle(keyHash string) (*Throttle, error)
	ThrottleExists(emailKey string) (bool, error)
}

// AccountsWriteTx are the account writes.
type AccountsWriteTx interface {
	InsertUser(u User) error
	SetUserAdmin(id string, admin bool, now int64) error
	SetUserDisabled(id string, disabledAt *int64, now int64) error
	// SetPassword inserts or replaces the user's password hash.
	SetPassword(userID, phc string, now int64) error
	InsertPassword(userID, phc string, now int64) error
	InsertIdentity(i Identity) error
	InsertPersonal(userID string, p Personal) error

	InsertAuthRequest(ar AuthRequest) error
	SetAuthRequestGrant(id, grantHash string) error
	// IncrAuthRequestAttempts adds one to password_attempts or code_attempts and returns it.
	IncrAuthRequestAttempts(id, field string) (int64, error)
	SetAuthRequestCode(id, userID, codeHash string, codeExpiresAt int64) error
	// ConsumeAuthCode marks the request holding codeHash consumed, if its code is unexpired and
	// unused, and returns it; ErrNotFound otherwise.
	ConsumeAuthCode(codeHash string, now int64) (*AuthRequest, error)

	InsertOneTimeToken(t OneTimeToken) error
	// UseOneTimeToken adds one use if the token is still usable at now; false otherwise.
	UseOneTimeToken(hash string, now int64) (bool, error)
	RevokeOneTimeToken(id string, now int64) (bool, error)
	DeleteOneTimeTokens(purpose string) error

	InsertSession(s Session) error
	// RevokeSessionsBeyond revokes the user's oldest live sessions so at most keep stay live.
	RevokeSessionsBeyond(userID string, keep, now int64) error
	// RotateSession is the refresh compare-and-swap; ErrNotFound when nothing matched.
	RotateSession(r Rotation) (*Session, error)
	RevokeSession(id string, now int64) error
	// RevokeUserSession revokes one live session of userID; false when there is none.
	RevokeUserSession(id, userID string, now int64) (bool, error)
	RevokeUserSessions(userID string, now int64) (int64, error)
	SetSessionLastUsedDay(id string, day int64) error

	PutThrottle(t Throttle) error
	DeleteThrottle(keyHash string) error
	DeleteThrottleByEmail(emailKey string) (int64, error)

	// GC deletes expired auth requests, dead one-time tokens and sessions and stale throttle rows.
	GC(now, deadBefore, throttleBefore int64) (GCStats, error)
}

// GCStats counts the rows a GC pass deleted.
type GCStats struct {
	AuthRequests, Tokens, Sessions, Throttle int64
}
