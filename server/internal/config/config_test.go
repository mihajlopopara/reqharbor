package config

import (
	"errors"
	"os"
	"strings"
	"testing"
	"time"
)

func parse(t *testing.T, env ...string) (*Result, error) {
	t.Helper()
	if DevAuthCompiled && !hasVar(env, "REQHARBOR_LISTEN") && hasVar(env, "REQHARBOR_PUBLIC_URL") {
		// A devauth build refuses non-loopback listeners; keep the other tests meaningful.
		env = append([]string{"REQHARBOR_LISTEN=127.0.0.1:8080"}, env...)
	}
	return Parse(Options{Environ: env, ReadFile: func(p string) ([]byte, error) {
		if p == "/run/secrets/gh" {
			return []byte("s3cret\n"), nil
		}
		return nil, &os.PathError{Op: "open", Path: p, Err: os.ErrNotExist}
	}})
}

func mustParse(t *testing.T, env ...string) *Result {
	t.Helper()
	res, err := parse(t, env...)
	if err != nil {
		t.Fatalf("Parse(%v): %v", env, err)
	}
	return res
}

func wantErr(t *testing.T, contains string, env ...string) {
	t.Helper()
	_, err := parse(t, env...)
	var ce *Error
	if !errors.As(err, &ce) {
		t.Fatalf("Parse(%v): want a config error containing %q, got %v", env, contains, err)
	}
	if !strings.Contains(err.Error(), contains) {
		t.Fatalf("Parse(%v): error %q does not contain %q", env, err, contains)
	}
}

const pub = "REQHARBOR_PUBLIC_URL=https://rh.example.com"

func hasVar(env []string, name string) bool {
	for _, kv := range env {
		if strings.HasPrefix(kv, name+"=") {
			return true
		}
	}
	return false
}

func TestDefaults(t *testing.T) {
	c := mustParse(t, pub).Config
	checks := []struct {
		name      string
		got, want any
	}{
		{"PublicURL", c.PublicURL, "https://rh.example.com"},
		{"Listen", c.Listen, map[bool]string{false: ":8080", true: "127.0.0.1:8080"}[DevAuthCompiled]},
		{"HSTS", c.HSTS, "max-age=31536000"},
		{"DataDir", c.DataDir, "./data"},
		{"ServerName", c.ServerName, "rh.example.com"},
		{"LogLevel", c.LogLevel, "info"},
		{"LogFormat", c.LogFormat, "json"},
		{"ClientIPHeader", c.ClientIPHeader, "X-Forwarded-For"},
		{"Signup", c.Signup, "invite"},
		{"MaxUsers", c.MaxUsers, int64(500)},
		{"PasswordAuth", c.PasswordAuth, true},
		{"LegacyWorkspaces", c.LegacyWorkspaces, "off"},
		{"LegacyTotalBytes", c.LegacyTotalBytes, int64(1 << 30)},
		{"OpenSignupTotalBytes", c.OpenSignupTotalBytes, int64(5 << 30)},
		{"QuotaLegacyBytes", c.QuotaLegacyBytes, int64(52428800)},
		{"QuotaPersonalBytes", c.QuotaPersonalBytes, int64(104857600)},
		{"MinFreeDiskBytes", c.MinFreeDiskBytes, int64(536870912)},
		{"MaxDBBytes", c.MaxDBBytes, int64(0)},
		{"SessionMax", c.SessionMax, int64(20)},
		{"RateLegacyCreatePerHour", c.RateLegacyCreatePerHour, int64(10)},
		{"RateShareCreatePerHour", c.RateShareCreatePerHour, int64(30)},
		{"ShutdownTimeout", c.ShutdownTimeout, 20 * time.Second},
		{"DevAuth", c.DevAuth, false},
		{"TrustedProxies", len(c.TrustedProxies), 0},
	}
	for _, ch := range checks {
		if ch.got != ch.want {
			t.Errorf("%s = %v, want %v", ch.name, ch.got, ch.want)
		}
	}
}

func TestPublicURLRequiredUnlessListenerIsLoopback(t *testing.T) {
	wantErr(t, "REQHARBOR_PUBLIC_URL is required")
	wantErr(t, "REQHARBOR_PUBLIC_URL is required", "REQHARBOR_LISTEN=0.0.0.0:8080")
	wantErr(t, "REQHARBOR_PUBLIC_URL is required", "REQHARBOR_LISTEN=:9000")
	for listen, want := range map[string]string{
		"127.0.0.1:8080": "http://127.0.0.1:8080",
		"[::1]:9000":     "http://[::1]:9000",
	} {
		c := mustParse(t, "REQHARBOR_LISTEN="+listen).Config
		if c.PublicURL != want || c.HSTS != "" {
			t.Errorf("%s: PublicURL %q HSTS %q", listen, c.PublicURL, c.HSTS)
		}
	}
}

func TestPublicURLNormalisationAndValidation(t *testing.T) {
	for in, want := range map[string]string{
		"https://RH.Example.com/":      "https://rh.example.com",
		"HTTPS://rh.example.com:443":   "https://rh.example.com",
		"http://192.168.1.5:8080":      "http://192.168.1.5:8080",
		"http://[2001:DB8::1]:80":      "http://[2001:db8::1]",
		"https://rh.example.com:8443/": "https://rh.example.com:8443",
	} {
		if got := mustParse(t, "REQHARBOR_PUBLIC_URL="+in).Config.PublicURL; got != want {
			t.Errorf("%s → %s, want %s", in, got, want)
		}
	}
	for _, bad := range []string{"rh.example.com", "ftp://x", "https://rh.example.com/sync", "https://rh.example.com?x=1", "https://rh.example.com#f", "https://u:p@rh.example.com", "https://"} {
		wantErr(t, "REQHARBOR_PUBLIC_URL", "REQHARBOR_PUBLIC_URL="+bad)
	}
}

func TestHSTSValidation(t *testing.T) {
	if c := mustParse(t, pub, "REQHARBOR_HSTS=off").Config; c.HSTS != "" {
		t.Errorf("off: %q", c.HSTS)
	}
	if c := mustParse(t, pub, "REQHARBOR_HSTS=max-age=600; includeSubDomains; preload").Config; c.HSTS != "max-age=600; includeSubDomains; preload" {
		t.Errorf("full: %q", c.HSTS)
	}
	if c := mustParse(t, "REQHARBOR_PUBLIC_URL=http://lan.example:8080").Config; c.HSTS != "" {
		t.Errorf("http public URL sends HSTS %q", c.HSTS)
	}
	for _, bad := range []string{"max-age=abc", "includeSubDomains", "max-age=1; preload; includeSubDomains", "max-age=1;foo"} {
		wantErr(t, "REQHARBOR_HSTS", pub, "REQHARBOR_HSTS="+bad)
	}
}

func TestValidationErrors(t *testing.T) {
	wantErr(t, "REQHARBOR_SIGNUP", pub, "REQHARBOR_SIGNUP=sometimes")
	wantErr(t, "REQHARBOR_LEGACY_WORKSPACES", pub, "REQHARBOR_LEGACY_WORKSPACES=yes")
	wantErr(t, "REQHARBOR_LOG_LEVEL", pub, "REQHARBOR_LOG_LEVEL=loud")
	wantErr(t, "REQHARBOR_MAX_USERS", pub, "REQHARBOR_MAX_USERS=-1")
	wantErr(t, "REQHARBOR_RATE_LEGACY_CREATE_PER_HOUR", pub, "REQHARBOR_RATE_LEGACY_CREATE_PER_HOUR=0")
	wantErr(t, "REQHARBOR_QUOTA_LEGACY_BYTES", pub, "REQHARBOR_QUOTA_LEGACY_BYTES=lots")
	wantErr(t, "REQHARBOR_SHUTDOWN_TIMEOUT", pub, "REQHARBOR_SHUTDOWN_TIMEOUT=soon")
	wantErr(t, "REQHARBOR_LOG_CLIENT_IP", pub, "REQHARBOR_LOG_CLIENT_IP=maybe")
	wantErr(t, "REQHARBOR_LISTEN", pub, "REQHARBOR_LISTEN=8080")
	wantErr(t, "Postgres is not supported in this version; unset REQHARBOR_DATABASE_URL", pub, "REQHARBOR_DATABASE_URL=postgres://x")
	wantErr(t, "GitHub sign-in needs both", pub, "REQHARBOR_GITHUB_CLIENT_ID=abc")
	wantErr(t, "GitHub sign-in needs both", pub, "REQHARBOR_GITHUB_CLIENT_SECRET=abc")
	wantErr(t, "REQHARBOR_SMTP_FROM is required", pub, "REQHARBOR_SMTP_HOST=smtp.example.com")
	wantErr(t, "REQHARBOR_MIN_CLIENT_VERSION", pub, "REQHARBOR_MIN_CLIENT_VERSION=latest")
}

func TestSecretFromFile(t *testing.T) {
	c := mustParse(t, pub, "REQHARBOR_GITHUB_CLIENT_ID=id", "REQHARBOR_GITHUB_CLIENT_SECRET_FILE=/run/secrets/gh").Config
	if c.GitHubClientSecret != "s3cret" {
		t.Errorf("secret %q", c.GitHubClientSecret)
	}
	wantErr(t, "cannot read /run/secrets/missing", pub, "REQHARBOR_GITHUB_CLIENT_ID=id", "REQHARBOR_GITHUB_CLIENT_SECRET_FILE=/run/secrets/missing")
	wantErr(t, "not both", pub, "REQHARBOR_GITHUB_CLIENT_ID=id", "REQHARBOR_GITHUB_CLIENT_SECRET=x", "REQHARBOR_GITHUB_CLIENT_SECRET_FILE=/run/secrets/gh")
}

func TestUnknownVariables(t *testing.T) {
	res := mustParse(t, pub, "REQHARBOR_FANCY=1", "OTHER_THING=1", "REQHARBOR_SIGNUPP=open")
	joined := strings.Join(res.Warnings, "\n")
	if !strings.Contains(joined, "unknown variable REQHARBOR_FANCY") {
		t.Errorf("warnings: %v", res.Warnings)
	}
	if !strings.Contains(joined, "unknown variable REQHARBOR_SIGNUPP (did you mean REQHARBOR_SIGNUP?)") {
		t.Errorf("warnings: %v", res.Warnings)
	}
	if strings.Contains(joined, "OTHER_THING") {
		t.Errorf("non-REQHARBOR variable warned about")
	}
	// Limit families are errors: a typo there would silently keep a default.
	wantErr(t, "unknown variable REQHARBOR_RATE_LEGACY_CREATES_PER_HOUR (did you mean REQHARBOR_RATE_LEGACY_CREATE_PER_HOUR?)", pub, "REQHARBOR_RATE_LEGACY_CREATES_PER_HOUR=100")
	wantErr(t, "unknown variable REQHARBOR_QUOTA_LEGACY", pub, "REQHARBOR_QUOTA_LEGACY=1")
	wantErr(t, "unknown variable REQHARBOR_MAX_USER", pub, "REQHARBOR_MAX_USER=1")
	// The design's example: a missing family prefix is caught through its suggestion.
	wantErr(t, "unknown variable REQHARBOR_LEGACY_CREATE_PER_HOUR (did you mean REQHARBOR_RATE_LEGACY_CREATE_PER_HOUR?)", pub, "REQHARBOR_LEGACY_CREATE_PER_HOUR=100")
}

func TestDevAuthRequiresADevauthBuild(t *testing.T) {
	if DevAuthCompiled {
		t.Skip("devauth build")
	}
	wantErr(t, "dev sign-in is not compiled into this build", pub, "REQHARBOR_DEV_AUTH=true")
	if c := mustParse(t, pub, "REQHARBOR_DEV_AUTH=false").Config; c.DevAuth {
		t.Error("dev auth on")
	}
}

func TestDevauthBuildRequiresALoopbackListener(t *testing.T) {
	if !DevAuthCompiled {
		t.Skip("default build")
	}
	for _, listen := range []string{":8080", "0.0.0.0:8080", "10.0.0.1:8080"} {
		wantErr(t, "REQHARBOR_LISTEN must be a loopback address", pub, "REQHARBOR_LISTEN="+listen)
	}
	if c := mustParse(t, pub, "REQHARBOR_LISTEN=127.0.0.1:8080", "REQHARBOR_DEV_AUTH=true").Config; !c.DevAuth {
		t.Error("dev auth off")
	}
}

func TestTrustedProxies(t *testing.T) {
	c := mustParse(t, pub, "REQHARBOR_TRUSTED_PROXIES=172.30.53.10/32, 10.0.0.1 ,2001:db8::/48").Config
	got := make([]string, len(c.TrustedProxies))
	for i, p := range c.TrustedProxies {
		got[i] = p.String()
	}
	if strings.Join(got, ",") != "172.30.53.10/32,10.0.0.1/32,2001:db8::/48" {
		t.Errorf("%v", got)
	}
	wantErr(t, "only explicit CIDRs", pub, "REQHARBOR_TRUSTED_PROXIES=private")
	wantErr(t, "only explicit CIDRs", pub, "REQHARBOR_TRUSTED_PROXIES=10.0.0.0/8,loopback")
	res := mustParse(t, pub, "REQHARBOR_TRUSTED_PROXIES=10.0.0.0/8,2001:db8::/32,192.168.0.0/16")
	n := 0
	for _, w := range res.Warnings {
		if strings.Contains(w, "very wide") {
			n++
		}
	}
	if n != 2 {
		t.Errorf("wide warnings: %v", res.Warnings)
	}
}

func TestStartupWarnings(t *testing.T) {
	res := mustParse(t, "REQHARBOR_PUBLIC_URL=http://lan.example:8080", "REQHARBOR_LEGACY_WORKSPACES=open", "REQHARBOR_MAX_USERS=0", "REQHARBOR_SIGNUP=open")
	joined := strings.Join(res.Warnings, "\n")
	for _, w := range []string{"REQHARBOR_PUBLIC_URL is http", "REQHARBOR_LEGACY_WORKSPACES=open: anyone who can reach this server", "REQHARBOR_MAX_USERS=0", "open signup without SMTP"} {
		if !strings.Contains(joined, w) {
			t.Errorf("missing warning %q in %v", w, res.Warnings)
		}
	}
	if res := mustParse(t, "REQHARBOR_LISTEN=127.0.0.1:8080"); len(res.Warnings) != 0 {
		t.Errorf("loopback http warned: %v", res.Warnings)
	}
}

func TestHealthcheckURL(t *testing.T) {
	for listen, want := range map[string]string{
		"":               "http://127.0.0.1:8080/healthz",
		":8080":          "http://127.0.0.1:8080/healthz",
		":9000":          "http://127.0.0.1:9000/healthz",
		"0.0.0.0:7000":   "http://127.0.0.1:7000/healthz",
		"[::]:7000":      "http://[::1]:7000/healthz",
		"127.0.0.1:8081": "http://127.0.0.1:8081/healthz",
		"10.1.2.3:80":    "http://10.1.2.3:80/healthz",
	} {
		got, err := HealthcheckURL(listen)
		if err != nil || got != want {
			t.Errorf("%q → %q %v, want %q", listen, got, err, want)
		}
	}
	if _, err := HealthcheckURL("8080"); err == nil {
		t.Error("bad listen accepted")
	}
}

func TestLoopbackHelpers(t *testing.T) {
	for in, want := range map[string]bool{"127.0.0.1:1": true, "[::1]:1": true, "127.9.9.9:1": true, ":8080": false, "0.0.0.0:1": false, "localhost:1": false, "10.0.0.1:1": false} {
		if got := IsLoopbackListen(in); got != want {
			t.Errorf("IsLoopbackListen(%q) = %v", in, got)
		}
	}
}
