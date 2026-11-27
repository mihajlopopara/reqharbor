package main

import (
	"bytes"
	"net"
	"net/http"
	"net/http/httptest"
	"path/filepath"
	"strings"
	"testing"

	"github.com/mihajlopopara/reqharbor/server/internal/config"
)

func runCLI(environ []string, args ...string) (int, string, string) {
	var out, errb bytes.Buffer
	code := run(args, environ, &out, &errb)
	return code, out.String(), errb.String()
}

func TestVersionAndUnknownCommand(t *testing.T) {
	code, out, _ := runCLI(nil, "version")
	if code != 0 || !strings.HasPrefix(out, "reqharbor-server 0.1.0 (commit unknown)") {
		t.Fatalf("%d %q", code, out)
	}
	if code, _, errs := runCLI(nil, "frobnicate"); code != 2 || !strings.Contains(errs, `unknown command "frobnicate"`) {
		t.Fatalf("%d %q", code, errs)
	}
}

func TestServeExits2OnConfigErrors(t *testing.T) {
	for env, want := range map[string]string{
		"REQHARBOR_DATA_DIR=" + t.TempDir():     "REQHARBOR_PUBLIC_URL is required",
		"REQHARBOR_DEV_AUTH=true":               "dev sign-in is not compiled into this build",
		"REQHARBOR_RATE_LEGACY_CREATES=5":       "unknown variable REQHARBOR_RATE_LEGACY_CREATES",
		"REQHARBOR_DATABASE_URL=postgres://x/y": "Postgres is not supported in this version",
	} {
		if config.DevAuthCompiled && strings.Contains(env, "DEV_AUTH") {
			continue
		}
		environ := []string{env}
		if !strings.HasPrefix(env, "REQHARBOR_DATA_DIR") {
			environ = append(environ, "REQHARBOR_PUBLIC_URL=https://rh.example.com")
		}
		code, _, errs := runCLI(environ, "serve")
		if code != 2 || !strings.Contains(errs, want) || strings.Count(strings.TrimSpace(errs), "\n") != 0 {
			t.Fatalf("%s: %d %q", env, code, errs)
		}
	}
}

func TestMigrateStatusAndApply(t *testing.T) {
	env := []string{"REQHARBOR_DATA_DIR=" + filepath.Join(t.TempDir(), "d")}
	code, out, errs := runCLI(env, "migrate", "--status")
	if code != 0 || !strings.Contains(out, "schema version 0 (latest 5)") || !strings.Contains(out, "pending 0001_init") {
		t.Fatalf("%d %q %q", code, out, errs)
	}
	if code, out, _ := runCLI(env, "migrate", "--to", "2"); code != 0 || !strings.Contains(out, "applied 0002_device_id_and_quota") || strings.Contains(out, "0003") {
		t.Fatalf("%d %q", code, out)
	}
	if code, out, _ := runCLI(env, "migrate"); code != 0 || !strings.Contains(out, "applied 0003_server_state") || !strings.Contains(out, "applied 0004_accounts") {
		t.Fatalf("%d %q", code, out)
	}
	if code, out, _ := runCLI(env, "migrate"); code != 0 || !strings.Contains(out, "nothing to apply") {
		t.Fatalf("%d %q", code, out)
	}
}

func TestHealthcheck(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path == "/healthz" {
			w.WriteHeader(200)
			return
		}
		w.WriteHeader(503)
	}))
	defer srv.Close()
	if code, _, _ := runCLI(nil, "healthcheck", "--url", srv.URL+"/healthz"); code != 0 {
		t.Fatalf("healthy: %d", code)
	}
	if code, _, _ := runCLI(nil, "healthcheck", "--url", srv.URL+"/other"); code != 1 {
		t.Fatalf("503: %d", code)
	}
	// Without --url the URL comes from REQHARBOR_LISTEN (":port" means loopback).
	_, port, _ := net.SplitHostPort(strings.TrimPrefix(srv.URL, "http://"))
	if code, _, errs := runCLI([]string{"REQHARBOR_LISTEN=:" + port}, "healthcheck"); code != 0 {
		t.Fatalf("from LISTEN: %d %s", code, errs)
	}
	if code, _, _ := runCLI([]string{"REQHARBOR_LISTEN=127.0.0.1:1"}, "healthcheck"); code != 1 {
		t.Fatalf("closed port: %d", code)
	}
}
