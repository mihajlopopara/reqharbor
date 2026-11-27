package app_test

import (
	"context"
	"encoding/json"
	"errors"
	"io"
	"log/slog"
	"net"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/mihajlopopara/reqharbor/server/internal/app"
	"github.com/mihajlopopara/reqharbor/server/internal/db"
	"github.com/mihajlopopara/reqharbor/server/internal/jobs"
	"github.com/mihajlopopara/reqharbor/server/internal/mail"
	tu "github.com/mihajlopopara/reqharbor/server/internal/testutil"
)

type running struct {
	base string
	done chan error
	stop context.CancelFunc
}

func startRun(t *testing.T, dataDir string, hook func(context.Context)) *running {
	t.Helper()
	return startRunWith(t, dataDir, nil, app.RunOptions{BeforeMigrate: hook})
}

func startRunWith(t *testing.T, dataDir string, env []string, opts app.RunOptions) *running {
	t.Helper()
	cfg := tu.ParseEnv(t, append(tu.DefaultEnv(dataDir), env...))
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithCancel(context.Background())
	r := &running{base: "http://" + ln.Addr().String(), done: make(chan error, 1), stop: cancel}
	logger := slog.New(slog.NewJSONHandler(io.Discard, nil))
	go func() {
		opts.Listener = ln
		r.done <- app.Run(ctx, cfg, logger, app.DefaultVersion, opts)
	}()
	t.Cleanup(func() {
		cancel()
		select {
		case <-r.done:
		case <-time.After(10 * time.Second):
			t.Error("Run did not stop")
		}
	})
	return r
}

func get(t *testing.T, url string) (int, map[string]any, http.Header) {
	t.Helper()
	res, err := http.Get(url)
	if err != nil {
		return 0, nil, nil
	}
	defer res.Body.Close()
	var v map[string]any
	_ = json.NewDecoder(res.Body).Decode(&v)
	return res.StatusCode, v, res.Header
}

func waitFor(t *testing.T, what string, cond func() bool) {
	t.Helper()
	deadline := time.Now().Add(10 * time.Second)
	for !cond() {
		if time.Now().After(deadline) {
			t.Fatalf("timed out waiting for %s", what)
		}
		time.Sleep(10 * time.Millisecond)
	}
}

func TestRun_StartupOrder_HealthzAnswersWhileMigrationsBlock(t *testing.T) {
	release := make(chan struct{})
	entered := make(chan struct{})
	r := startRun(t, t.TempDir(), func(context.Context) {
		close(entered)
		<-release
	})
	waitFor(t, "healthz", func() bool { c, _, _ := get(t, r.base+"/healthz"); return c == 200 })
	<-entered

	code, body, _ := get(t, r.base+"/readyz")
	if code != 503 || body["status"] != "unavailable" || body["checks"].(map[string]any)["migrations"] != "pending" {
		t.Fatalf("readyz %d %v", code, body)
	}
	for _, p := range []string{"/v1/sync", "/v1/server", "/v1/workspaces", "/nope"} {
		code, body, hd := get(t, r.base+p)
		e, _ := body["error"].(map[string]any)
		if code != 503 || e["code"] != "starting" || e["message"] != "Server is starting; try again shortly" || hd.Get("Retry-After") != "5" {
			t.Fatalf("%s: %d %v", p, code, body)
		}
	}
	close(release)
	waitFor(t, "readyz", func() bool { c, _, _ := get(t, r.base+"/readyz"); return c == 200 })
	_, body, _ = get(t, r.base+"/readyz")
	tu.JSONEq(t, body, `{"status":"ok","checks":{"db":"ok","migrations":"ok","disk":"ok"}}`)
	if code, _, _ := get(t, r.base+"/v1/server"); code != 200 {
		t.Fatalf("/v1/server %d", code)
	}
	r.stop()
	if err := <-r.done; err != nil {
		t.Fatalf("Run: %v", err)
	}
	r.done <- nil // for the cleanup
}

func TestRun_PreMigrationBackupAndSingleInstanceLock(t *testing.T) {
	dir := t.TempDir()
	d, err := db.Open(dir, db.Options{})
	if err != nil {
		t.Fatal(err)
	}
	if _, err := d.Migrate(context.Background(), 2, time.Now); err != nil {
		t.Fatal(err)
	}
	if _, err := d.Writer.Exec("INSERT INTO workspaces (id, name, token_hash, rev, created_at) VALUES ('W', 'w', 'h', 0, 1)"); err != nil {
		t.Fatal(err)
	}
	d.Close()

	r := startRun(t, dir, nil)
	waitFor(t, "readyz", func() bool { c, _, _ := get(t, r.base+"/readyz"); return c == 200 })
	matches, _ := filepath.Glob(filepath.Join(dir, "backups", "pre-migrate-v2-v5-*.db"))
	if len(matches) != 1 {
		t.Fatalf("backups: %v", matches)
	}
	// The backup is a v2 database with the row.
	bdir := t.TempDir()
	b, _ := os.ReadFile(matches[0])
	if err := os.WriteFile(filepath.Join(bdir, db.FileName), b, 0o600); err != nil {
		t.Fatal(err)
	}
	bd, err := db.Open(bdir, db.Options{})
	if err != nil {
		t.Fatal(err)
	}
	defer bd.Close()
	if v, _ := db.CurrentVersion(context.Background(), bd.Reader); v != 2 {
		t.Fatalf("backup at v%d", v)
	}

	// A second serve on the same data directory is refused.
	cfg := tu.ParseEnv(t, tu.DefaultEnv(dir))
	err = app.Run(context.Background(), cfg, slog.New(slog.NewTextHandler(io.Discard, nil)), "x", app.RunOptions{})
	if !errors.Is(err, db.ErrLocked) {
		t.Fatalf("second Run: %v", err)
	}
	// A fresh database takes no backup.
	fresh := t.TempDir()
	r2 := startRun(t, fresh, nil)
	waitFor(t, "readyz", func() bool { c, _, _ := get(t, r2.base+"/readyz"); return c == 200 })
	if m, _ := filepath.Glob(filepath.Join(fresh, "backups", "*")); len(m) != 0 {
		t.Fatalf("unexpected backups %v", m)
	}
}

func TestRun_FailedMigrationKeepsHealthzUpAndReadyzDown(t *testing.T) {
	h := tu.New(t)
	h.App.SetMigrations(app.MigrationsFailed)
	res := h.Call(t, "/readyz", tu.Req{})
	if res.StatusCode != 503 || !strings.Contains(string(res.Body), `"migrations":"failed"`) {
		t.Fatalf("%d %s", res.StatusCode, res.Body)
	}
	if res := h.Call(t, "/healthz", tu.Req{}); res.StatusCode != 200 {
		t.Fatalf("healthz %d", res.StatusCode)
	}
}

func TestDiscovery_V1Server(t *testing.T) {
	h := tu.New(t, tu.WithEnv("REQHARBOR_SERVER_NAME=Acme ReqHarbor"))
	res := h.Call(t, "/v1/server", tu.Req{})
	if res.StatusCode != 200 || res.Header.Get("Cache-Control") != "no-store" || res.Header.Get("Access-Control-Allow-Origin") != "" {
		t.Fatalf("%d %v", res.StatusCode, res.Header)
	}
	tu.JSONEq(t, res.Body, map[string]any{
		"software": "reqharbor-server", "version": app.DefaultVersion, "apiVersion": 1,
		"instanceId": h.App.InstanceID(), "syncEpoch": h.App.SyncEpoch(),
		"name": "Acme ReqHarbor", "publicUrl": "https://reqharbor.test", "minClientVersion": nil,
		"auth": map[string]any{"methods": []any{map[string]any{"id": "password", "label": "Email and password"}},
			"signup": "invite", "passwordReset": "admin", "emailVerification": false, "setupRequired": true},
		"features": map[string]any{"accounts": true, "teams": false, "legacyWorkspaces": "open"},
		"sync":     map[string]any{"types": []any{"project", "collection", "folder", "request", "environment"}, "requestKinds": []any{"http"}},
		"limits":   map[string]any{"entityBytes": 524288, "pushChanges": 500, "requestBytes": 5242880, "snapshotBytes": 1048576, "jsonDepth": 100},
	})
	if r := h.Call(t, "/v1/server", tu.Req{Method: "POST"}); r.StatusCode != 405 || r.Header.Get("Allow") != "GET" {
		t.Fatalf("POST %d", r.StatusCode)
	}
	// The default name is the public URL's host; minClientVersion is reported when set.
	h2 := tu.New(t, tu.WithEnv("REQHARBOR_MIN_CLIENT_VERSION=0.3.0", "REQHARBOR_LEGACY_WORKSPACES=off"))
	v := h2.Call(t, "/v1/server", tu.Req{}).JSON(t)
	if v["name"] != "reqharbor.test" || v["minClientVersion"] != "0.3.0" || v["features"].(map[string]any)["legacyWorkspaces"] != "off" {
		t.Fatalf("%v", v)
	}
	if v["instanceId"] == h.App.InstanceID() {
		t.Fatal("two servers share an instance id")
	}
}

func TestRun_StartsTheMailSenderAndScheduledBackups(t *testing.T) {
	relay := tu.NewSMTPServer(t, tu.SMTPOptions{Mode: "none"}) // loopback, so TLS=none is allowed
	dir := t.TempDir()
	started := make(chan *app.App, 1)
	r := startRunWith(t, dir, append(relay.Env(), "REQHARBOR_BACKUP_INTERVAL=1h", "REQHARBOR_BACKUP_KEEP=2"),
		app.RunOptions{Started: func(a *app.App) { started <- a }})
	a := <-started
	waitFor(t, "ready", func() bool { code, _, _ := get(t, r.base+"/readyz"); return code == 200 })
	// No scheduled backup exists yet, so the first one runs as soon as the server is ready.
	waitFor(t, "the first scheduled backup", func() bool {
		l, _ := jobs.ScheduledBackups(filepath.Join(dir, "backups"))
		return len(l) == 1
	})
	if a.Mail == nil {
		t.Fatal("no outbox with SMTP configured")
	}
	a.Mail.Send("test", mail.Message{To: "x@example.test", Subject: "hello", Text: "from serve"})
	if m := relay.Wait(t, 1)[0]; m.Body != "from serve\n" || m.TLS {
		t.Fatalf("%+v", m)
	}
}

// serve starts OIDC discovery at once, before the migrations finish, and lists the provider once
// discovery succeeded (a loopback IdP may use http, §5.4).
func TestRun_OIDCDiscoveryStartsWithServe(t *testing.T) {
	hit := make(chan struct{}, 1)
	var idp *httptest.Server
	idp = httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/dex/.well-known/openid-configuration" {
			http.NotFound(w, r)
			return
		}
		select {
		case hit <- struct{}{}:
		default:
		}
		_ = json.NewEncoder(w).Encode(map[string]any{"issuer": idp.URL + "/dex", "authorization_endpoint": idp.URL + "/dex/auth",
			"token_endpoint": idp.URL + "/dex/token", "jwks_uri": idp.URL + "/dex/keys"})
	}))
	defer idp.Close()
	release := make(chan struct{})
	r := startRunWith(t, t.TempDir(), []string{"REQHARBOR_OIDC_ISSUER=" + idp.URL + "/dex", "REQHARBOR_OIDC_CLIENT_ID=rh",
		"REQHARBOR_OIDC_CLIENT_SECRET=s", "REQHARBOR_OIDC_NAME=Dex"}, app.RunOptions{BeforeMigrate: func(ctx context.Context) {
		select {
		case <-release:
		case <-ctx.Done():
		}
	}})
	select {
	case <-hit: // discovery ran while the migrations were still blocked
	case <-time.After(10 * time.Second):
		t.Fatal("no discovery request")
	}
	close(release)
	waitFor(t, "oidc listed", func() bool {
		status, body, _ := get(t, r.base+"/v1/server")
		if status != 200 {
			return false
		}
		methods, _ := body["auth"].(map[string]any)["methods"].([]any)
		for _, m := range methods {
			if mm := m.(map[string]any); mm["id"] == "oidc" && mm["label"] == "Dex" {
				return true
			}
		}
		return false
	})
}
