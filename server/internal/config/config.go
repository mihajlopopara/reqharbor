// Package config parses and validates the REQHARBOR_* environment (06-go-server.md §6).
//
// Parse never reads the process environment itself: callers pass os.Environ() (or a test list),
// and file contents are read through an injectable function.
package config

import (
	"crypto/tls"
	"errors"
	"fmt"
	"net"
	netmail "net/mail"
	"net/netip"
	"net/url"
	"os"
	"regexp"
	"sort"
	"strconv"
	"strings"
	"time"

	"github.com/mihajlopopara/reqharbor/server/internal/ids"
)

// Legacy-workspace modes (REQHARBOR_LEGACY_WORKSPACES).
const (
	LegacyOff      = "off"
	LegacyExisting = "existing"
	LegacyOpen     = "open"
)

// MinSetupCodeChars is the shortest REQHARBOR_SETUP_CODE accepted, counted after normalisation
// (generated codes have 16).
const MinSetupCodeChars = 12

// Signup modes (REQHARBOR_SIGNUP).
const (
	SignupOpen   = "open"
	SignupInvite = "invite"
	SignupClosed = "closed"
)

// Config is the validated server configuration.
type Config struct {
	PublicURL string // normalised origin, no trailing slash
	Listen    string
	HSTS      string // "" when off
	DataDir   string

	ServerName     string
	LogLevel       string
	LogFormat      string
	LogClientIP    bool
	TrustedProxies []netip.Prefix
	ClientIPHeader string

	Signup          string
	MaxUsers        int64
	AdminEmail      string
	SetupCode       string
	PasswordAuth    bool
	Argon2MemoryKiB int64

	GitHubClientID, GitHubClientSecret         string
	GoogleClientID, GoogleClientSecret         string
	OIDCIssuer, OIDCClientID, OIDCClientSecret string
	OIDCName, OIDCScopes                       string
	OIDCRequireVerifiedEmail                   bool

	DevAuth bool

	SMTPHost, SMTPUsername, SMTPPassword, SMTPFrom, SMTPTLS string
	SMTPPort                                                int64

	LegacyWorkspaces     string
	LegacyTotalBytes     int64
	OpenSignupTotalBytes int64
	Teams                bool
	MinClientVersion     string

	QuotaLegacyBytes   int64
	QuotaPersonalBytes int64
	QuotaTeamBytes     int64
	QuotaOwnerBytes    int64
	MaxOwnedTeams      int64
	MaxTeamMembers     int64
	MaxTeamInvites     int64
	MaxDBBytes         int64
	MinFreeDiskBytes   int64
	SessionMax         int64

	RateLegacyCreatePerHour   int64
	RateAuthPerMin            int64
	RateLoginPerMin           int64
	RateSignupPerHour         int64
	RateTokenPerMin           int64
	RateSyncPerMin            int64
	RateShareCreatePerHour    int64
	RateEmailPerRecipientHour int64
	RateEmailPerHour          int64

	BackupInterval  time.Duration
	BackupKeep      int64
	ShutdownTimeout time.Duration

	// Test holds overrides that no environment variable reaches: Parse never sets them, only
	// tests do (06-go-server.md §10 S5: fake provider servers).
	Test TestOverrides
}

// TestOverrides point the external providers at fake servers in tests. Empty fields keep the
// real endpoints.
type TestOverrides struct {
	GitHubAuthURL  string // default https://github.com/login/oauth/authorize
	GitHubTokenURL string // default https://github.com/login/oauth/access_token
	GitHubAPIURL   string // default https://api.github.com
	// SMTPTLSConfig is the base client TLS configuration of the SMTP relay (a pool that trusts
	// the fake SMTP server's certificate); nil means the system roots.
	SMTPTLSConfig *tls.Config
}

// SMTPEnabled reports whether REQHARBOR_SMTP_HOST is set: email codes at signup and reset links
// by email (§5.5, §5.7).
func (c *Config) SMTPEnabled() bool { return c.SMTPHost != "" }

// MinBackupInterval is the shortest REQHARBOR_BACKUP_INTERVAL accepted.
const MinBackupInterval = time.Minute

// Error is a startup configuration error; serve exits with code 2 and its one-line message.
type Error struct{ Msg string }

func (e *Error) Error() string { return e.Msg }

func errorf(format string, a ...any) error { return &Error{Msg: fmt.Sprintf(format, a...)} }

// Options are the inputs of Parse.
type Options struct {
	// Environ is a list of KEY=VALUE pairs, as from os.Environ.
	Environ []string
	// ReadFile reads *_FILE secrets; nil means os.ReadFile.
	ReadFile func(string) ([]byte, error)
}

// Result is the parsed configuration plus the startup warnings to log.
type Result struct {
	Config   *Config
	Warnings []string
}

var hstsRe = regexp.MustCompile(`^max-age=\d+(; ?includeSubDomains)?(; ?preload)?$`)

// secretNames may be supplied as <NAME>_FILE.
var secretNames = []string{
	"REQHARBOR_GITHUB_CLIENT_SECRET",
	"REQHARBOR_GOOGLE_CLIENT_SECRET",
	"REQHARBOR_OIDC_CLIENT_SECRET",
	"REQHARBOR_SMTP_PASSWORD",
}

// limitFamilies are prefixes in which an unknown name is a startup error (Q9).
var limitFamilies = []string{"REQHARBOR_RATE_", "REQHARBOR_QUOTA_", "REQHARBOR_MAX_"}

type env struct {
	vals     map[string]string
	readFile func(string) ([]byte, error)
}

func (e *env) get(name string) (string, bool) {
	v, ok := e.vals[name]
	return v, ok
}

func (e *env) str(name, def string) string {
	if v, ok := e.vals[name]; ok && strings.TrimSpace(v) != "" {
		return strings.TrimSpace(v)
	}
	return def
}

func (e *env) boolean(name string, def bool) (bool, error) {
	v, ok := e.vals[name]
	if !ok || strings.TrimSpace(v) == "" {
		return def, nil
	}
	switch strings.ToLower(strings.TrimSpace(v)) {
	case "true", "1", "yes", "on":
		return true, nil
	case "false", "0", "no", "off":
		return false, nil
	}
	return false, errorf("%s must be true or false, got %q", name, v)
}

func (e *env) integer(name string, def, min int64) (int64, error) {
	v, ok := e.vals[name]
	if !ok || strings.TrimSpace(v) == "" {
		return def, nil
	}
	n, err := strconv.ParseInt(strings.TrimSpace(v), 10, 64)
	if err != nil || n < min {
		return 0, errorf("%s must be an integer >= %d, got %q", name, min, v)
	}
	return n, nil
}

func (e *env) duration(name string, def time.Duration) (time.Duration, error) {
	v, ok := e.vals[name]
	if !ok || strings.TrimSpace(v) == "" {
		return def, nil
	}
	s := strings.TrimSpace(v)
	if s == "0" {
		return 0, nil
	}
	d, err := time.ParseDuration(s)
	if err != nil || d < 0 {
		return 0, errorf("%s must be a duration such as 20s or 24h, got %q", name, v)
	}
	return d, nil
}

func (e *env) oneOf(name, def string, allowed ...string) (string, error) {
	v := strings.ToLower(e.str(name, def))
	for _, a := range allowed {
		if v == a {
			return v, nil
		}
	}
	return "", errorf("%s must be one of %s, got %q", name, strings.Join(allowed, ", "), e.vals[name])
}

// secret returns NAME, or the trimmed contents of the file named by NAME_FILE.
func (e *env) secret(name string) (string, error) {
	v, hasV := e.vals[name]
	f, hasF := e.vals[name+"_FILE"]
	hasV = hasV && v != ""
	hasF = hasF && f != ""
	if hasV && hasF {
		return "", errorf("set either %s or %s_FILE, not both", name, name)
	}
	if hasF {
		b, err := e.readFile(f)
		if err != nil {
			return "", errorf("%s_FILE: cannot read %s: %v", name, f, unwrapPathErr(err))
		}
		return strings.TrimRight(string(b), "\r\n"), nil
	}
	return v, nil
}

func unwrapPathErr(err error) error {
	var pe *os.PathError
	if errors.As(err, &pe) {
		return pe.Err
	}
	return err
}

// Parse validates the environment. It returns a *Error for any invalid setting.
func Parse(opts Options) (*Result, error) {
	e := &env{vals: map[string]string{}, readFile: opts.ReadFile}
	if e.readFile == nil {
		e.readFile = os.ReadFile
	}
	for _, kv := range opts.Environ {
		k, v, ok := strings.Cut(kv, "=")
		if ok && strings.HasPrefix(k, "REQHARBOR_") {
			e.vals[k] = v
		}
	}
	res := &Result{Config: &Config{}}
	c := res.Config

	warnings, err := checkUnknown(e.vals)
	if err != nil {
		return nil, err
	}
	res.Warnings = append(res.Warnings, warnings...)

	if v, ok := e.get("REQHARBOR_DATABASE_URL"); ok && v != "" {
		return nil, errorf("Postgres is not supported in this version; unset REQHARBOR_DATABASE_URL")
	}

	c.Listen = e.str("REQHARBOR_LISTEN", ":8080")
	if _, _, err := net.SplitHostPort(c.Listen); err != nil {
		return nil, errorf("REQHARBOR_LISTEN must be host:port or :port, got %q", c.Listen)
	}
	// One definition of a loopback listener for both rules that depend on it: the PUBLIC_URL
	// default here and the devauth guard below (an IP literal; "localhost:P" is not one).
	loopback := IsLoopbackListen(c.Listen)

	if raw := e.str("REQHARBOR_PUBLIC_URL", ""); raw != "" {
		u, err := normaliseOrigin(raw)
		if err != nil {
			return nil, errorf("REQHARBOR_PUBLIC_URL %v", err)
		}
		c.PublicURL = u
	} else if loopback {
		c.PublicURL = "http://" + c.Listen
	} else {
		return nil, errorf("REQHARBOR_PUBLIC_URL is required: the address users enter in ReqHarbor, for example https://rh.example.com")
	}
	pub, _ := url.Parse(c.PublicURL)

	hsts := e.str("REQHARBOR_HSTS", "max-age=31536000")
	switch {
	case strings.EqualFold(hsts, "off"):
		c.HSTS = ""
	case !hstsRe.MatchString(hsts):
		return nil, errorf("REQHARBOR_HSTS must be off or match max-age=N[; includeSubDomains][; preload], got %q", hsts)
	case pub.Scheme == "https":
		c.HSTS = hsts
	}

	c.DataDir = e.str("REQHARBOR_DATA_DIR", "./data")
	c.ServerName = e.str("REQHARBOR_SERVER_NAME", pub.Host)
	if c.LogLevel, err = e.oneOf("REQHARBOR_LOG_LEVEL", "info", "debug", "info", "warn", "error"); err != nil {
		return nil, err
	}
	if c.LogFormat, err = e.oneOf("REQHARBOR_LOG_FORMAT", "json", "json", "text"); err != nil {
		return nil, err
	}
	if c.LogClientIP, err = e.boolean("REQHARBOR_LOG_CLIENT_IP", false); err != nil {
		return nil, err
	}
	prefixes, pw, err := ParseTrustedProxies(e.str("REQHARBOR_TRUSTED_PROXIES", ""))
	if err != nil {
		return nil, err
	}
	c.TrustedProxies = prefixes
	res.Warnings = append(res.Warnings, pw...)
	c.ClientIPHeader = e.str("REQHARBOR_CLIENT_IP_HEADER", "X-Forwarded-For")

	if c.Signup, err = e.oneOf("REQHARBOR_SIGNUP", SignupInvite, SignupOpen, SignupInvite, SignupClosed); err != nil {
		return nil, err
	}
	if c.MaxUsers, err = e.integer("REQHARBOR_MAX_USERS", 500, 0); err != nil {
		return nil, err
	}
	c.AdminEmail = strings.ToLower(e.str("REQHARBOR_ADMIN_EMAIL", ""))
	c.SetupCode = e.str("REQHARBOR_SETUP_CODE", "")
	if c.SetupCode != "" {
		// The code must survive the pages' normalisation (a value such as "----" never could)
		// and be long enough for an internet-facing first run.
		if n := ids.NormalizeCode(c.SetupCode); len(n) < MinSetupCodeChars || !ids.IsCrockford(n) {
			return nil, errorf("REQHARBOR_SETUP_CODE must be at least %d Crockford base32 characters (0-9 and A-Z without U; dashes and spaces are ignored), such as one printed by `reqharbor-server setup-code`", MinSetupCodeChars)
		}
	}
	if c.PasswordAuth, err = e.boolean("REQHARBOR_PASSWORD_AUTH", true); err != nil {
		return nil, err
	}
	if c.Argon2MemoryKiB, err = e.integer("REQHARBOR_ARGON2_MEMORY_KIB", 65536, 8192); err != nil {
		return nil, err
	}

	// External providers: an id without a secret (or the reverse) is an error.
	type provider struct {
		label         string
		id, secret    *string
		idVar, secVar string
	}
	providers := []provider{
		{"GitHub", &c.GitHubClientID, &c.GitHubClientSecret, "REQHARBOR_GITHUB_CLIENT_ID", "REQHARBOR_GITHUB_CLIENT_SECRET"},
		{"Google", &c.GoogleClientID, &c.GoogleClientSecret, "REQHARBOR_GOOGLE_CLIENT_ID", "REQHARBOR_GOOGLE_CLIENT_SECRET"},
		{"OIDC", &c.OIDCClientID, &c.OIDCClientSecret, "REQHARBOR_OIDC_CLIENT_ID", "REQHARBOR_OIDC_CLIENT_SECRET"},
	}
	for _, p := range providers {
		*p.id = e.str(p.idVar, "")
		s, err := e.secret(p.secVar)
		if err != nil {
			return nil, err
		}
		*p.secret = s
		if (*p.id == "") != (*p.secret == "") {
			return nil, errorf("%s sign-in needs both %s and %s", p.label, p.idVar, p.secVar)
		}
	}
	c.OIDCIssuer = e.str("REQHARBOR_OIDC_ISSUER", "")
	if (c.OIDCIssuer == "") != (c.OIDCClientID == "") {
		return nil, errorf("OIDC sign-in needs REQHARBOR_OIDC_ISSUER together with REQHARBOR_OIDC_CLIENT_ID and REQHARBOR_OIDC_CLIENT_SECRET")
	}
	c.OIDCName = e.str("REQHARBOR_OIDC_NAME", "Single sign-on")
	c.OIDCScopes = e.str("REQHARBOR_OIDC_SCOPES", "openid email profile")
	if c.OIDCRequireVerifiedEmail, err = e.boolean("REQHARBOR_OIDC_REQUIRE_VERIFIED_EMAIL", true); err != nil {
		return nil, err
	}

	if c.DevAuth, err = e.boolean("REQHARBOR_DEV_AUTH", false); err != nil {
		return nil, err
	}
	if c.DevAuth && !DevAuthCompiled {
		return nil, errorf("dev sign-in is not compiled into this build")
	}
	if DevAuthCompiled && !IsLoopbackListen(c.Listen) {
		return nil, errorf("this is a devauth build: REQHARBOR_LISTEN must be a loopback address such as 127.0.0.1:8080, got %q", c.Listen)
	}

	c.SMTPHost = e.str("REQHARBOR_SMTP_HOST", "")
	if c.SMTPTLS, err = e.oneOf("REQHARBOR_SMTP_TLS", "starttls", "starttls", "tls", "none"); err != nil {
		return nil, err
	}
	defPort := int64(587)
	if c.SMTPTLS == "tls" {
		defPort = 465 // implicit TLS ("SMTPS")
	}
	if c.SMTPPort, err = e.integer("REQHARBOR_SMTP_PORT", defPort, 1); err != nil {
		return nil, err
	}
	if c.SMTPPort > 65535 {
		return nil, errorf("REQHARBOR_SMTP_PORT must be a port from 1 to 65535, got %d", c.SMTPPort)
	}
	c.SMTPUsername = e.str("REQHARBOR_SMTP_USERNAME", "")
	if c.SMTPPassword, err = e.secret("REQHARBOR_SMTP_PASSWORD"); err != nil {
		return nil, err
	}
	c.SMTPFrom = e.str("REQHARBOR_SMTP_FROM", "")
	if c.SMTPHost != "" {
		if strings.ContainsAny(c.SMTPHost, "/@ \t") || (strings.Contains(c.SMTPHost, ":") && net.ParseIP(strings.Trim(c.SMTPHost, "[]")) == nil) {
			return nil, errorf("REQHARBOR_SMTP_HOST must be a host name or IP address without a port, got %q", c.SMTPHost)
		}
		// An IPv6 literal may be written [::1]; the dialer adds the brackets itself, and the
		// loopback checks (here and in net/smtp's PLAIN auth) compare the bare address.
		if ip := net.ParseIP(strings.TrimSuffix(strings.TrimPrefix(c.SMTPHost, "["), "]")); ip != nil {
			c.SMTPHost = ip.String()
		}
		if c.SMTPFrom == "" {
			return nil, errorf("REQHARBOR_SMTP_FROM is required with REQHARBOR_SMTP_HOST, for example \"ReqHarbor <rh@example.com>\"")
		}
		if a, err := netmail.ParseAddress(c.SMTPFrom); err != nil || strings.ContainsAny(c.SMTPFrom, "\r\n") || !strings.Contains(a.Address, "@") {
			return nil, errorf("REQHARBOR_SMTP_FROM must be an address such as rh@example.com or \"ReqHarbor <rh@example.com>\", got %q", c.SMTPFrom)
		}
		if (c.SMTPUsername == "") != (c.SMTPPassword == "") {
			return nil, errorf("SMTP authentication needs both REQHARBOR_SMTP_USERNAME and REQHARBOR_SMTP_PASSWORD")
		}
		if c.SMTPTLS == "none" && !IsLoopbackHost(c.SMTPHost) {
			return nil, errorf("REQHARBOR_SMTP_TLS=none is allowed only for a loopback SMTP host")
		}
	} else if c.SMTPFrom != "" || c.SMTPUsername != "" || c.SMTPPassword != "" {
		res.Warnings = append(res.Warnings, "REQHARBOR_SMTP_* is set without REQHARBOR_SMTP_HOST: email is off")
	}

	if c.LegacyWorkspaces, err = e.oneOf("REQHARBOR_LEGACY_WORKSPACES", LegacyOff, LegacyOff, LegacyExisting, LegacyOpen); err != nil {
		return nil, err
	}
	ints := []struct {
		dst      *int64
		name     string
		def, min int64
	}{
		{&c.LegacyTotalBytes, "REQHARBOR_LEGACY_TOTAL_BYTES", 1 << 30, 0},
		{&c.OpenSignupTotalBytes, "REQHARBOR_OPEN_SIGNUP_TOTAL_BYTES", 5 << 30, 0},
		{&c.QuotaLegacyBytes, "REQHARBOR_QUOTA_LEGACY_BYTES", 52428800, 1},
		{&c.QuotaPersonalBytes, "REQHARBOR_QUOTA_PERSONAL_BYTES", 104857600, 0},
		{&c.QuotaTeamBytes, "REQHARBOR_QUOTA_TEAM_BYTES", 262144000, 0},
		{&c.QuotaOwnerBytes, "REQHARBOR_QUOTA_OWNER_BYTES", 0, 0},
		{&c.MaxOwnedTeams, "REQHARBOR_MAX_OWNED_TEAMS", 10, 0},
		{&c.MaxTeamMembers, "REQHARBOR_MAX_TEAM_MEMBERS", 100, 1},
		{&c.MaxTeamInvites, "REQHARBOR_MAX_TEAM_INVITES", 20, 0},
		{&c.MaxDBBytes, "REQHARBOR_MAX_DB_BYTES", 0, 0},
		{&c.MinFreeDiskBytes, "REQHARBOR_MIN_FREE_DISK_BYTES", 536870912, 0},
		{&c.SessionMax, "REQHARBOR_SESSION_MAX", 20, 1},
		{&c.RateLegacyCreatePerHour, "REQHARBOR_RATE_LEGACY_CREATE_PER_HOUR", 10, 1},
		{&c.RateAuthPerMin, "REQHARBOR_RATE_AUTH_PER_MIN", 120, 1},
		{&c.RateLoginPerMin, "REQHARBOR_RATE_LOGIN_PER_MIN", 30, 1},
		{&c.RateSignupPerHour, "REQHARBOR_RATE_SIGNUP_PER_HOUR", 30, 1},
		{&c.RateTokenPerMin, "REQHARBOR_RATE_TOKEN_PER_MIN", 60, 1},
		{&c.RateSyncPerMin, "REQHARBOR_RATE_SYNC_PER_MIN", 240, 1},
		{&c.RateShareCreatePerHour, "REQHARBOR_RATE_SHARE_CREATE_PER_HOUR", 30, 1},
		{&c.RateEmailPerRecipientHour, "REQHARBOR_RATE_EMAIL_PER_RECIPIENT_PER_HOUR", 5, 1},
		{&c.RateEmailPerHour, "REQHARBOR_RATE_EMAIL_PER_HOUR", 200, 1},
		{&c.BackupKeep, "REQHARBOR_BACKUP_KEEP", 7, 1},
	}
	for _, it := range ints {
		if *it.dst, err = e.integer(it.name, it.def, it.min); err != nil {
			return nil, err
		}
	}
	if c.Teams, err = e.boolean("REQHARBOR_TEAMS", false); err != nil {
		return nil, err
	}
	c.MinClientVersion = e.str("REQHARBOR_MIN_CLIENT_VERSION", "")
	if c.MinClientVersion != "" && !semverRe.MatchString(c.MinClientVersion) {
		return nil, errorf("REQHARBOR_MIN_CLIENT_VERSION must be a version such as 0.3.0, got %q", c.MinClientVersion)
	}
	if c.BackupInterval, err = e.duration("REQHARBOR_BACKUP_INTERVAL", 0); err != nil {
		return nil, err
	}
	if c.BackupInterval > 0 && c.BackupInterval < MinBackupInterval {
		return nil, errorf("REQHARBOR_BACKUP_INTERVAL must be 0 (off) or at least 1m, got %q", e.vals["REQHARBOR_BACKUP_INTERVAL"])
	}
	if c.ShutdownTimeout, err = e.duration("REQHARBOR_SHUTDOWN_TIMEOUT", 20*time.Second); err != nil {
		return nil, err
	}

	// Startup warnings (§6 Health and logging).
	if pub.Scheme == "http" && !IsLoopbackHost(pub.Hostname()) {
		res.Warnings = append(res.Warnings, "REQHARBOR_PUBLIC_URL is http: passwords, tokens and synced data travel unencrypted; put a TLS proxy in front and use an https URL")
	}
	if c.LegacyWorkspaces == LegacyOpen {
		res.Warnings = append(res.Warnings, "REQHARBOR_LEGACY_WORKSPACES=open: anyone who can reach this server can store up to 50 MB per anonymous workspace and publish public share pages under your domain (/s/…), which can be abused for spam or phishing. Use it only on a private network, or set the caps below.")
	}
	if c.MaxUsers == 0 {
		res.Warnings = append(res.Warnings, "REQHARBOR_MAX_USERS=0: there is no cap on the number of accounts")
	}
	if c.Signup == SignupOpen && c.SMTPHost == "" {
		res.Warnings = append(res.Warnings, "open signup without SMTP: emails are not verified, and anyone can test which emails have accounts")
	}
	return res, nil
}

var semverRe = regexp.MustCompile(`^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$`)

// normaliseOrigin validates an absolute http(s) origin without path, query or fragment and
// returns it with a lowercase scheme and host and without a trailing slash or default port.
func normaliseOrigin(raw string) (string, error) {
	u, err := url.Parse(raw)
	if err != nil || (u.Scheme != "http" && u.Scheme != "https") || u.Host == "" {
		return "", fmt.Errorf("must be an absolute http(s) origin such as https://rh.example.com, got %q", raw)
	}
	if u.User != nil || (u.Path != "" && u.Path != "/") || u.RawQuery != "" || u.Fragment != "" || u.Opaque != "" || strings.HasSuffix(raw, "?") || strings.HasSuffix(raw, "#") {
		return "", fmt.Errorf("must be an origin without a path, query or fragment, got %q", raw)
	}
	scheme := strings.ToLower(u.Scheme)
	host := strings.ToLower(u.Hostname())
	port := u.Port()
	if (scheme == "http" && port == "80") || (scheme == "https" && port == "443") {
		port = ""
	}
	if strings.Contains(host, ":") {
		host = "[" + host + "]"
	}
	if port != "" {
		host += ":" + port
	}
	return scheme + "://" + host, nil
}

// IsLoopbackHost reports whether a host name (no port, brackets allowed) is loopback: a loopback
// IP literal or "localhost". It judges hosts the server connects to or that users type (the SMTP
// host, the PUBLIC_URL host). Listen addresses use IsLoopbackListen, which is stricter.
func IsLoopbackHost(host string) bool {
	host = strings.TrimSuffix(strings.TrimPrefix(host, "["), "]")
	if strings.EqualFold(host, "localhost") {
		return true
	}
	a, err := netip.ParseAddr(host)
	return err == nil && a.Unmap().IsLoopback()
}

// IsLoopbackListen reports whether a listen address binds only to a loopback IP literal
// (127.0.0.1:P or [::1]:P; ":8080", "0.0.0.0:8080" and "localhost:8080" are not: a name may
// resolve to anything). It decides both the PUBLIC_URL default and the devauth guard.
func IsLoopbackListen(listen string) bool {
	host, _, err := net.SplitHostPort(listen)
	if err != nil || host == "" {
		return false
	}
	a, err := netip.ParseAddr(host)
	return err == nil && a.Unmap().IsLoopback()
}

// ParseTrustedProxies parses REQHARBOR_TRUSTED_PROXIES: a comma-separated list of explicit CIDRs
// (a bare IP is a single host). There is no "private" keyword (Q14). Ranges wider than /16
// (IPv4) or /48 (IPv6) produce a warning.
func ParseTrustedProxies(s string) ([]netip.Prefix, []string, error) {
	var out []netip.Prefix
	var warnings []string
	for _, part := range strings.Split(s, ",") {
		part = strings.TrimSpace(part)
		if part == "" {
			continue
		}
		var p netip.Prefix
		var err error
		if strings.Contains(part, "/") {
			p, err = netip.ParsePrefix(part)
		} else {
			var a netip.Addr
			a, err = netip.ParseAddr(part)
			if err == nil {
				p = netip.PrefixFrom(a, a.BitLen())
			}
		}
		if err != nil {
			return nil, nil, errorf("REQHARBOR_TRUSTED_PROXIES accepts only explicit CIDRs such as 172.30.53.10/32, got %q", part)
		}
		p = p.Masked()
		if p.Addr().Is4In6() {
			p = netip.PrefixFrom(p.Addr().Unmap(), p.Bits()-96)
		}
		if (p.Addr().Is4() && p.Bits() < 16) || (p.Addr().Is6() && p.Bits() < 48) {
			warnings = append(warnings, fmt.Sprintf("REQHARBOR_TRUSTED_PROXIES entry %s is very wide: every client inside it can choose its own rate-limit bucket", p))
		}
		out = append(out, p)
	}
	return out, warnings, nil
}

// knownNames is every variable §6 documents. Unknown names are warnings, or errors in a limit
// family.
var knownNames = func() map[string]bool {
	names := []string{
		"PUBLIC_URL", "LISTEN", "HSTS", "DATA_DIR", "DATABASE_URL", "SERVER_NAME", "LOG_LEVEL", "LOG_FORMAT",
		"LOG_CLIENT_IP", "TRUSTED_PROXIES", "CLIENT_IP_HEADER", "SIGNUP", "MAX_USERS", "ADMIN_EMAIL",
		"SETUP_CODE", "PASSWORD_AUTH", "ARGON2_MEMORY_KIB", "GITHUB_CLIENT_ID", "GITHUB_CLIENT_SECRET",
		"GOOGLE_CLIENT_ID", "GOOGLE_CLIENT_SECRET", "OIDC_ISSUER", "OIDC_CLIENT_ID", "OIDC_CLIENT_SECRET",
		"OIDC_NAME", "OIDC_SCOPES", "OIDC_REQUIRE_VERIFIED_EMAIL", "DEV_AUTH", "SMTP_HOST", "SMTP_PORT",
		"SMTP_USERNAME", "SMTP_PASSWORD", "SMTP_FROM", "SMTP_TLS", "LEGACY_WORKSPACES", "LEGACY_TOTAL_BYTES",
		"OPEN_SIGNUP_TOTAL_BYTES", "TEAMS", "MIN_CLIENT_VERSION", "QUOTA_LEGACY_BYTES", "QUOTA_PERSONAL_BYTES",
		"QUOTA_TEAM_BYTES", "QUOTA_OWNER_BYTES", "MAX_OWNED_TEAMS", "MAX_TEAM_MEMBERS", "MAX_TEAM_INVITES",
		"MAX_DB_BYTES", "MIN_FREE_DISK_BYTES", "SESSION_MAX", "RATE_LEGACY_CREATE_PER_HOUR", "RATE_AUTH_PER_MIN",
		"RATE_LOGIN_PER_MIN", "RATE_SIGNUP_PER_HOUR", "RATE_TOKEN_PER_MIN", "RATE_SYNC_PER_MIN",
		"RATE_SHARE_CREATE_PER_HOUR", "RATE_EMAIL_PER_RECIPIENT_PER_HOUR", "RATE_EMAIL_PER_HOUR",
		"BACKUP_INTERVAL", "BACKUP_KEEP", "SHUTDOWN_TIMEOUT",
	}
	m := make(map[string]bool, len(names)+len(secretNames))
	for _, n := range names {
		m["REQHARBOR_"+n] = true
	}
	for _, n := range secretNames {
		m[n+"_FILE"] = true
	}
	return m
}()

func inLimitFamily(name string) bool {
	for _, p := range limitFamilies {
		if strings.HasPrefix(name, p) {
			return true
		}
	}
	return false
}

// suggest returns the known name closest to name, or "".
func suggest(name string) string {
	rest := strings.TrimPrefix(name, "REQHARBOR_")
	best, bestDist := "", 4
	for _, k := range sortedKnown() {
		// A missing family prefix (REQHARBOR_LEGACY_CREATE_PER_HOUR → …_RATE_LEGACY_CREATE_PER_HOUR).
		if strings.HasSuffix(k, "_"+rest) {
			return k
		}
		if d := levenshtein(name, k); d < bestDist {
			best, bestDist = k, d
		}
	}
	return best
}

func sortedKnown() []string {
	out := make([]string, 0, len(knownNames))
	for k := range knownNames {
		out = append(out, k)
	}
	sort.Strings(out)
	return out
}

func checkUnknown(vals map[string]string) ([]string, error) {
	var warnings []string
	names := make([]string, 0, len(vals))
	for k := range vals {
		names = append(names, k)
	}
	sort.Strings(names)
	for _, k := range names {
		if knownNames[k] {
			continue
		}
		hint := suggest(k)
		msg := "unknown variable " + k
		if hint != "" {
			msg += " (did you mean " + hint + "?)"
		}
		if inLimitFamily(k) || (hint != "" && inLimitFamily(hint)) {
			return nil, &Error{Msg: msg}
		}
		warnings = append(warnings, msg)
	}
	return warnings, nil
}

func levenshtein(a, b string) int {
	prev := make([]int, len(b)+1)
	cur := make([]int, len(b)+1)
	for j := range prev {
		prev[j] = j
	}
	for i := 1; i <= len(a); i++ {
		cur[0] = i
		for j := 1; j <= len(b); j++ {
			cost := 1
			if a[i-1] == b[j-1] {
				cost = 0
			}
			cur[j] = min(prev[j]+1, cur[j-1]+1, prev[j-1]+cost)
		}
		prev, cur = cur, prev
	}
	return prev[len(b)]
}

// HealthcheckURL builds the /healthz URL for the `healthcheck` command from REQHARBOR_LISTEN:
// an empty host, 0.0.0.0 or :: becomes the matching loopback address.
func HealthcheckURL(listen string) (string, error) {
	if listen == "" {
		listen = ":8080"
	}
	host, port, err := net.SplitHostPort(listen)
	if err != nil {
		return "", errorf("REQHARBOR_LISTEN must be host:port or :port, got %q", listen)
	}
	switch host {
	case "", "0.0.0.0":
		host = "127.0.0.1"
	case "::":
		host = "::1"
	}
	return "http://" + net.JoinHostPort(host, port) + "/healthz", nil
}
