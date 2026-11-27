package providers

import (
	"context"
	"net/http"
	"net/http/httptest"
	"net/url"
	"strings"
	"testing"

	"github.com/mihajlopopara/reqharbor/server/internal/config"
)

func TestPrimaryVerifiedEmail(t *testing.T) {
	for _, tc := range []struct {
		name   string
		emails []GitHubEmail
		want   string
	}{
		{"primary and verified", []GitHubEmail{{Email: "a@x.test"}, {Email: " B@X.Test ", Primary: true, Verified: true}}, "b@x.test"},
		{"primary not verified", []GitHubEmail{{Email: "p@x.test", Primary: true}, {Email: "v@x.test", Verified: true}}, ""},
		{"verified but not primary", []GitHubEmail{{Email: "v@x.test", Verified: true}}, ""},
		{"none", nil, ""},
		{"primary is not an address", []GitHubEmail{{Email: "nope", Primary: true, Verified: true}}, ""},
		{"GitHub noreply address", []GitHubEmail{{Email: "1+octo@users.noreply.github.com", Primary: true, Verified: true}}, "1+octo@users.noreply.github.com"},
	} {
		if got := PrimaryVerifiedEmail(tc.emails); got != tc.want {
			t.Errorf("%s: %q, want %q", tc.name, got, tc.want)
		}
	}
}

func cfg(env ...string) *config.Config {
	res, err := config.Parse(config.Options{Environ: append([]string{"REQHARBOR_LISTEN=127.0.0.1:8080", "REQHARBOR_PUBLIC_URL=https://rh.example.com"}, env...)})
	if err != nil {
		panic(err)
	}
	return res.Config
}

func TestEnabledAndListed(t *testing.T) {
	if got := Enabled(cfg(), Deps{}); len(got) != 0 {
		t.Fatalf("%v", got)
	}
	got := Enabled(cfg("REQHARBOR_GITHUB_CLIENT_ID=id", "REQHARBOR_GITHUB_CLIENT_SECRET=s"), Deps{})
	if len(got) != 1 || got[0].ID() != "github" || got[0].Label() != "GitHub" || !Listed(got[0]) {
		t.Fatalf("%v", got)
	}
	if g := got[0].(*GitHub); g.CallbackURL() != "https://rh.example.com/auth/callback/github" {
		t.Fatal(g.CallbackURL())
	}
}

func TestGitHubAuthURL(t *testing.T) {
	g := NewGitHub(cfg("REQHARBOR_GITHUB_CLIENT_ID=id", "REQHARBOR_GITHUB_CLIENT_SECRET=topsecret"), nil)
	u, err := url.Parse(g.AuthURL("st", "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk", "nonce-ignored"))
	if err != nil || u.Scheme+"://"+u.Host+u.Path != GitHubAuthURL {
		t.Fatalf("%v %v", u, err)
	}
	q := u.Query()
	for k, v := range map[string]string{"client_id": "id", "state": "st", "scope": "read:user user:email",
		"redirect_uri": "https://rh.example.com/auth/callback/github", "code_challenge_method": "S256",
		"code_challenge": "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM", "response_type": "code"} {
		if q.Get(k) != v {
			t.Errorf("%s = %q, want %q", k, q.Get(k), v)
		}
	}
	if strings.Contains(u.String(), "topsecret") {
		t.Fatal("secret in the authorize URL")
	}
	if q.Has("nonce") {
		t.Fatal("GitHub is OAuth only: no nonce")
	}
}

// Exchange failures surface as loggable codes that never carry a token.
func TestGitHubExchangeErrors(t *testing.T) {
	for _, tc := range []struct {
		name       string
		token      string // token endpoint JSON
		user       string // GET /user JSON
		userStatus int
		want       string
	}{
		{"oauth error with HTTP 200", `{"error":"bad_verification_code","error_description":"x"}`, "", 0, "token_bad_verification_code"},
		{"no access token", `{"token_type":"bearer"}`, "", 0, "token_request_failed"},
		{"user call refused", `{"access_token":"gho_secret","token_type":"bearer"}`, `{}`, 401, "api_status_401"},
		{"user without id", `{"access_token":"gho_secret","token_type":"bearer"}`, `{"login":"x"}`, 0, "user_without_id"},
		{"non-numeric id", `{"access_token":"gho_secret","token_type":"bearer"}`, `{"id":"abc"}`, 0, "api_bad_response"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			srv := httptest.NewTLSServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				w.Header().Set("Content-Type", "application/json")
				switch r.URL.Path {
				case "/token":
					_, _ = w.Write([]byte(tc.token))
				case "/user":
					if tc.userStatus != 0 {
						w.WriteHeader(tc.userStatus)
					}
					_, _ = w.Write([]byte(tc.user))
				default:
					_, _ = w.Write([]byte(`[]`))
				}
			}))
			defer srv.Close()
			c := cfg("REQHARBOR_GITHUB_CLIENT_ID=id", "REQHARBOR_GITHUB_CLIENT_SECRET=s")
			c.Test = config.TestOverrides{GitHubTokenURL: srv.URL + "/token", GitHubAPIURL: srv.URL}
			_, err := NewGitHub(c, srv.Client()).Exchange(context.Background(), "code", "verifier", "")
			if err == nil || ErrorCode(err) != tc.want || strings.Contains(err.Error(), "gho_secret") {
				t.Fatalf("%v (%s)", err, ErrorCode(err))
			}
		})
	}
}
