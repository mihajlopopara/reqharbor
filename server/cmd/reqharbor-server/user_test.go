package main

import (
	"context"
	"database/sql"
	"encoding/base64"
	"path/filepath"
	"regexp"
	"strings"
	"testing"
	"time"

	"github.com/mihajlopopara/reqharbor/server/internal/auth/throttle"
	"github.com/mihajlopopara/reqharbor/server/internal/db"
)

func cliEnvFor(t *testing.T, extra ...string) []string {
	t.Helper()
	env := append([]string{
		"REQHARBOR_DATA_DIR=" + filepath.Join(t.TempDir(), "data"),
		"REQHARBOR_PUBLIC_URL=https://rh.example.com",
		"REQHARBOR_ARGON2_MEMORY_KIB=8192",
		"REQHARBOR_LISTEN=127.0.0.1:8080", // devauth builds need a loopback listener
	}, extra...)
	if code, _, errs := runCLI(env, "migrate"); code != 0 {
		t.Fatalf("migrate: %s", errs)
	}
	return env
}

func withStdin(s string) func() {
	old := stdin
	stdin = strings.NewReader(s)
	return func() { stdin = old }
}

func openDB(t *testing.T, env []string) *sql.DB {
	t.Helper()
	d, err := db.Open(dataDir(env), db.Options{})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { d.Close() })
	return d.Writer
}

func expect(t *testing.T, env []string, wantCode int, want string, args ...string) string {
	t.Helper()
	code, out, errs := runCLI(env, args...)
	if code != wantCode || !strings.Contains(out+errs, want) {
		t.Fatalf("%v: exit %d (want %d)\nstdout: %s\nstderr: %s\nwant: %s", args, code, wantCode, out, errs, want)
	}
	return out
}

func TestUserCommands(t *testing.T) {
	env := cliEnvFor(t, "REQHARBOR_MAX_USERS=2")
	expect(t, env, 0, "EMAIL", "user", "list")

	restore := withStdin("a long password\n")
	expect(t, env, 0, "created admin a@example.test", "user", "create", "--email", "A@Example.test", "--name", "Ada", "--password-stdin")
	restore()
	out := expect(t, env, 0, "created user b@example.test", "user", "create", "--email", "b@example.test", "--reset-link")
	if !regexp.MustCompile(`https://rh\.example\.com/auth/reset#t=[A-Za-z0-9_-]{43}`).MatchString(out) {
		t.Fatalf("no reset link: %s", out)
	}
	expect(t, env, 2, "exactly one of --password-stdin and --reset-link", "user", "create", "--email", "c@example.test")
	expect(t, env, 1, "REQHARBOR_MAX_USERS is reached", "user", "create", "--email", "c@example.test", "--reset-link")
	expect(t, env, 0, "created user c@example.test", "user", "create", "--email", "c@example.test", "--reset-link", "--force")
	expect(t, env, 1, "already exists", "user", "create", "--email", "c@example.test", "--reset-link", "--force")
	restore = withStdin("short\n")
	expect(t, env, 1, "at least 10 characters", "user", "create", "--email", "d@example.test", "--password-stdin", "--force")
	restore()

	out = expect(t, env, 0, "a@example.test", "user", "list")
	if !regexp.MustCompile(`a@example\.test\s+\S+\s+yes\s+active\s+Email and password`).MatchString(out) ||
		!regexp.MustCompile(`b@example\.test\s+\S+\s+no\s+active`).MatchString(out) {
		t.Fatalf("list:\n%s", out)
	}
	w := openDB(t, env)
	isAdmin := func(email string) (admin bool, trusted bool, via string) {
		t.Helper()
		if err := w.QueryRow("SELECT is_admin, email_trusted, signup_via FROM users WHERE email = ?", email).Scan(&admin, &trusted, &via); err != nil {
			t.Fatal(err)
		}
		return
	}
	if a, tr, via := isAdmin("a@example.test"); !a || !tr || via != "cli" {
		t.Fatalf("a: %v %v %s", a, tr, via)
	}
	expect(t, env, 0, "b@example.test admin: true", "user", "set-admin", "--email", "b@example.test", "--on")
	expect(t, env, 0, "b@example.test admin: false", "user", "set-admin", "--email", "b@example.test", "--off")
	if a, _, _ := isAdmin("b@example.test"); a {
		t.Fatal("set-admin --off")
	}
	expect(t, env, 2, "exactly one of --on and --off", "user", "set-admin", "--email", "b@example.test")
	expect(t, env, 1, "no account with this email", "user", "set-admin", "--email", "zz@example.test", "--on")

	// Sessions are revoked by disable, set-password and sessions revoke.
	addSession := func(id string) {
		t.Helper()
		if _, err := w.Exec(`INSERT INTO sessions (id, user_id, client_id, access_hash, access_expires_at, refresh_hash, created_at,
		      last_used_day, idle_expires_at, expires_at) SELECT ?, id, 'reqharbor-desktop', ?, 9e15, ?, 1, 1, 9e15, 9e15 FROM users WHERE email = 'a@example.test'`,
			id, "a"+id, "r"+id); err != nil {
			t.Fatal(err)
		}
	}
	addSession("S1")
	expect(t, env, 0, "a@example.test disabled; 1 sessions revoked", "user", "disable", "--email", "a@example.test")
	var disabled sql.NullInt64
	_ = w.QueryRow("SELECT disabled_at FROM users WHERE email = 'a@example.test'").Scan(&disabled)
	if !disabled.Valid {
		t.Fatal("not disabled")
	}
	expect(t, env, 0, "a@example.test enabled", "user", "enable", "--email", "a@example.test")
	_ = w.QueryRow("SELECT disabled_at FROM users WHERE email = 'a@example.test'").Scan(&disabled)
	if disabled.Valid {
		t.Fatal("still disabled")
	}
	addSession("S2")
	expect(t, env, 0, "a@example.test: 1 sessions revoked", "user", "sessions", "revoke", "--email", "a@example.test")
	addSession("S3")

	// unlock and set-password clear every throttle row of the email, whatever the IP.
	key := make([]byte, 32)
	if _, err := w.Exec("INSERT INTO server_state (key, value) VALUES ('throttle_key', ?)", base64.StdEncoding.EncodeToString(key)); err != nil {
		t.Fatal(err)
	}
	addThrottle := func(bucket string) {
		t.Helper()
		if _, err := w.Exec("INSERT INTO login_throttle (key_hash, email_key, failed_count, locked_until, last_failed_at) VALUES (?, ?, 5, 9e15, 1)",
			throttle.PairKey(key, "a@example.test", bucket), throttle.EmailKey(key, "a@example.test")); err != nil {
			t.Fatal(err)
		}
	}
	addThrottle("1.1.1.1")
	addThrottle("2.2.2.2")
	expect(t, env, 0, "a@example.test unlocked (2 throttle entries cleared)", "user", "unlock", "--email", "A@example.test")
	addThrottle("3.3.3.3")
	restore = withStdin("another long password\n")
	expect(t, env, 0, "password changed for a@example.test; 1 sessions revoked", "user", "set-password", "--email", "a@example.test", "--password-stdin")
	restore()
	var rows int
	_ = w.QueryRow("SELECT COUNT(*) FROM login_throttle").Scan(&rows)
	if rows != 0 {
		t.Fatal("set-password left throttle rows")
	}
	expect(t, env, 2, "--password-stdin is required", "user", "set-password", "--email", "a@example.test")
	expect(t, env, 0, "https://rh.example.com/auth/reset#t=", "user", "reset-link", "--email", "a@example.test", "--ttl", "1h")
	expect(t, env, 2, "--email is required", "user", "reset-link")
	for _, bad := range []string{"0", "-1h", "169h", "8760h"} {
		expect(t, env, 2, "--ttl must be positive and at most 168h0m0s", "user", "reset-link", "--email", "a@example.test", "--ttl", bad)
	}
	expect(t, env, 0, "https://rh.example.com/auth/reset#t=", "user", "reset-link", "--email", "a@example.test", "--ttl", "168h")
	expect(t, env, 2, `unknown user command "frob"`, "user", "frob")

	// A provider-only account has no password to reset.
	if _, err := w.Exec(`INSERT INTO users (id, email, email_trusted, signup_via, created_at, updated_at) VALUES ('GH', 'gh@example.test', 1, 'cli', 1, 1);
	                     INSERT INTO identities (provider, subject, user_id, created_at, last_login_at) VALUES ('github', '7', 'GH', 1, 1)`); err != nil {
		t.Fatal(err)
	}
	expect(t, env, 1, "this account signs in with GitHub", "user", "reset-link", "--email", "gh@example.test")
	restore = withStdin("a long password\n")
	expect(t, env, 1, "signs in with GitHub", "user", "set-password", "--email", "gh@example.test", "--password-stdin")
	restore()
	expect(t, env, 0, "GitHub", "user", "list")

	// Setup codes only while no account exists.
	expect(t, env, 1, "already has accounts", "setup-code")
	fresh := cliEnvFor(t)
	out = expect(t, fresh, 0, "enter this setup code", "setup-code")
	if !regexp.MustCompile(`^[0-9A-Z]{4}-[0-9A-Z]{4}-[0-9A-Z]{4}-[0-9A-Z]{4}\n`).MatchString(out) {
		t.Fatalf("setup-code output: %q", out)
	}
}

func TestInviteCommands(t *testing.T) {
	env := cliEnvFor(t)
	out := expect(t, env, 0, "2 use(s)", "invite", "create", "--email", "c@example.test", "--uses", "2", "--ttl", "24h")
	m := regexp.MustCompile(`^([0-9A-Z]{4}-[0-9A-Z]{4}-[0-9A-Z]{4}-[0-9A-Z]{4})\nInvite (\S+):`).FindStringSubmatch(out)
	if m == nil {
		t.Fatalf("invite create output: %q", out)
	}
	id := m[2]
	out = expect(t, env, 0, id, "invite", "list")
	if !regexp.MustCompile(regexp.QuoteMeta(id) + `\s+c@example\.test\s+0/2\s+\S+ \S+\s+active`).MatchString(out) {
		t.Fatalf("list:\n%s", out)
	}
	expect(t, env, 0, "invite "+id+" revoked", "invite", "revoke", id)
	expect(t, env, 1, "no active invite", "invite", "revoke", id)
	if out := expect(t, env, 0, "revoked", "invite", "list"); !strings.Contains(out, id) {
		t.Fatal(out)
	}
	expect(t, env, 2, "give the invite ID", "invite", "revoke")
	expect(t, env, 1, "not a valid email", "invite", "create", "--email", "nope")
	expect(t, env, 1, "--uses must be at least 1", "invite", "create", "--uses", "0")
	expect(t, env, 2, `unknown invite command "x"`, "invite", "x")
	// The stored code is a hash only.
	w := openDB(t, env)
	var n int
	_ = w.QueryRow("SELECT COUNT(*) FROM one_time_tokens WHERE token_hash = ?", m[1]).Scan(&n)
	if n != 0 {
		t.Fatal("raw invite code stored")
	}
}

func TestAccountCommandsNeedAMigratedDatabaseAndConfig(t *testing.T) {
	env := []string{"REQHARBOR_DATA_DIR=" + filepath.Join(t.TempDir(), "d"), "REQHARBOR_PUBLIC_URL=https://rh.example.com", "REQHARBOR_LISTEN=127.0.0.1:8080"}
	d, err := db.Open(dataDir(env), db.Options{})
	if err != nil {
		t.Fatal(err)
	}
	if _, err := d.Migrate(context.Background(), 3, time.Now); err != nil {
		t.Fatal(err)
	}
	d.Close()
	expect(t, env, 1, "needs v5; start serve once", "user", "list")
	expect(t, []string{"REQHARBOR_LISTEN=0.0.0.0:1"}, 2, "REQHARBOR_PUBLIC_URL is required", "invite", "list")
}
