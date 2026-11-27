package app_test

// Port of worker/test/http.test.ts (06-go-server.md §1.6).

import (
	"context"
	"errors"
	"io"
	"strings"
	"testing"

	"github.com/mihajlopopara/reqharbor/server/internal/store"
	tu "github.com/mihajlopopara/reqharbor/server/internal/testutil"
)

func TestHTTP_BodyOver5MBIs413DeclaredAndStreamed(t *testing.T) {
	h := tu.New(t)
	ws := h.NewWorkspace(t, "")
	big := `{"changes":[],"pad":"` + strings.Repeat("x", 5*1024*1024) + `"}`
	res := h.Call(t, "/v1/sync", tu.Req{Method: "POST", Token: ws.Token, Raw: &big, Headers: map[string]string{"Content-Type": "application/json"}})
	if res.StatusCode != 413 || res.ErrorCode(t) != "payload_too_large" {
		t.Fatalf("declared: got %d %s", res.StatusCode, res.Body)
	}

	// A chunked body without Content-Length is cut off while streaming.
	pr, pw := io.Pipe()
	go func() {
		chunk := []byte(strings.Repeat("x", 1024*1024))
		for i := 0; i < 6; i++ {
			if _, err := pw.Write(chunk); err != nil {
				return
			}
		}
		pw.Close()
	}()
	streamed := h.Call(t, "/v1/sync", tu.Req{Method: "POST", Token: ws.Token, Body: pr, Headers: map[string]string{"Content-Type": "application/json"}})
	pr.Close()
	if streamed.StatusCode != 413 || streamed.ErrorCode(t) != "payload_too_large" {
		t.Fatalf("streamed: got %d %s", streamed.StatusCode, streamed.Body)
	}
}

func TestHTTP_InvalidJSONWrongContentTypeAndNonObjectBodies(t *testing.T) {
	h := tu.New(t)
	ws := h.NewWorkspace(t, "")
	cases := []struct {
		ct, body string
		status   int
		code     string
	}{
		{"application/json", "{not json", 400, "invalid_json"},
		{"application/json", "", 400, "invalid_json"},
		{"text/plain", "{}", 415, "unsupported_media_type"},
		{"application/json; charset=utf-8", "[]", 400, "validation_failed"},
		{"application/json", "null", 400, "validation_failed"},
		// Beyond the Worker test: the rest of the readJSON order matrix (§1.2).
		{"", "{}", 415, "unsupported_media_type"},
		{"Application/JSON ; charset=utf-8", `{"changes":[]}`, 200, ""},
		{"application/jsonx", "{}", 415, "unsupported_media_type"},
		{"application/json", "   \n\t ", 400, "invalid_json"},
		{"application/json", "\ufeff" + `{"changes":[]}`, 200, ""},
		{"application/json", "\xff\xfe{}", 400, "invalid_json"},
		{"application/json", "42", 400, "validation_failed"},
		{"application/json", `{"changes":[]} x`, 400, "invalid_json"},
	}
	for _, c := range cases {
		body := c.body
		hd := map[string]string{}
		if c.ct != "" {
			hd["Content-Type"] = c.ct
		}
		res := h.Call(t, "/v1/sync", tu.Req{Method: "POST", Token: ws.Token, Raw: &body, Headers: hd})
		if res.StatusCode != c.status {
			t.Fatalf("%q %q: status %d, want %d: %s", c.ct, c.body, res.StatusCode, c.status, res.Body)
		}
		if got := res.Header.Get("Content-Type"); got != "application/json; charset=utf-8" {
			t.Fatalf("%q: content type %q", c.body, got)
		}
		if c.code != "" {
			v := res.JSON(t)
			e, _ := v["error"].(map[string]any)
			if len(v) != 1 || e["code"] != c.code || e["message"] == "" || len(e) != 2 {
				t.Fatalf("%q: body %s", c.body, res.Body)
			}
		}
	}
}

func TestHTTP_ReadJSONOrder_DeclaredLengthBeforeMissingBodyAndAuthBeforeBody(t *testing.T) {
	h := tu.New(t)
	// 401 before the body: a 5 MB body without a token gets 401.
	big := strings.Repeat("x", 6*1024*1024)
	res := h.Call(t, "/v1/sync", tu.Req{Method: "POST", Raw: &big, Headers: map[string]string{"Content-Type": "text/plain"}})
	if res.StatusCode != 401 {
		t.Fatalf("got %d", res.StatusCode)
	}
	ws := h.NewWorkspace(t, "")
	// 415 is checked before 413.
	res = h.Call(t, "/v1/sync", tu.Req{Method: "POST", Token: ws.Token, Raw: &big, Headers: map[string]string{"Content-Type": "text/plain"}})
	if res.StatusCode != 415 {
		t.Fatalf("got %d", res.StatusCode)
	}
	// A missing body (no Content-Length, no bytes) is 400 invalid_json.
	res = h.Call(t, "/v1/sync", tu.Req{Method: "POST", Token: ws.Token, Headers: map[string]string{"Content-Type": "application/json"}})
	if res.StatusCode != 400 || res.ErrorCode(t) != "invalid_json" {
		t.Fatalf("got %d %s", res.StatusCode, res.Body)
	}
}

func TestHTTP_UnknownRoutes404AndWrongMethods405WithAllow(t *testing.T) {
	h := tu.New(t)
	nf := h.Call(t, "/v2/nope", tu.Req{})
	if nf.StatusCode != 404 || nf.ErrorCode(t) != "not_found" {
		t.Fatalf("got %d %s", nf.StatusCode, nf.Body)
	}
	tu.JSONEq(t, nf.Body, `{"error":{"code":"not_found","message":"No route for GET /v2/nope"}}`)
	na := h.Call(t, "/v1/workspace", tu.Req{Method: "DELETE"})
	if na.StatusCode != 405 || na.Header.Get("Allow") != "GET" || na.ErrorCode(t) != "method_not_allowed" {
		t.Fatalf("got %d %q %s", na.StatusCode, na.Header.Get("Allow"), na.Body)
	}
	tu.JSONEq(t, na.Body, `{"error":{"code":"method_not_allowed","message":"Method DELETE not allowed; use GET"}}`)
	// The method list is in registration order and HEAD is never added.
	sy := h.Call(t, "/v1/sync", tu.Req{Method: "HEAD"})
	if sy.StatusCode != 405 || sy.Header.Get("Allow") != "GET, POST" {
		t.Fatalf("got %d %q", sy.StatusCode, sy.Header.Get("Allow"))
	}
	// Share ids that do not match the route pattern are a route 404, before the method.
	for _, p := range []string{"/v1/shares/a-b", "/v1/shares/" + strings.Repeat("a", 65), "/s/a-b", "/s/a.b.json", "/s/.json", "/v1/sync/", "/v1/shares/"} {
		res := h.Call(t, p, tu.Req{Method: "PATCH"})
		if res.StatusCode != 404 || res.ErrorCode(t) != "not_found" {
			t.Fatalf("%s: got %d %s", p, res.StatusCode, res.Body)
		}
	}
}

func TestHTTP_SecurityHeadersOnEveryResponse(t *testing.T) {
	h := tu.New(t)
	ws := h.NewWorkspace(t, "")
	for _, res := range []*tu.Resp{
		h.Call(t, "/v1/workspace", tu.Req{Token: ws.Token}),
		h.Call(t, "/nope", tu.Req{}),
		h.Call(t, "/v1/sync", tu.Req{}),
		h.Call(t, "/healthz", tu.Req{}),
	} {
		hd := res.Header
		for k, want := range map[string]string{
			"X-Content-Type-Options":       "nosniff",
			"Referrer-Policy":              "no-referrer",
			"X-Frame-Options":              "DENY",
			"Cross-Origin-Opener-Policy":   "same-origin",
			"Cross-Origin-Resource-Policy": "same-origin",
			"Permissions-Policy":           "camera=(), microphone=(), geolocation=(), interest-cohort=()",
			"Strict-Transport-Security":    "max-age=31536000",
			"Content-Security-Policy":      "default-src 'none'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'",
			"Cache-Control":                "no-store",
		} {
			if got := hd.Get(k); got != want {
				t.Fatalf("%s: %s = %q, want %q", res.Request.URL.Path, k, got, want)
			}
		}
		if hd.Get("Set-Cookie") != "" {
			t.Fatal("Set-Cookie on a non-/auth response")
		}
		if hd.Get("X-Request-Id") == "" {
			t.Fatal("no X-Request-Id")
		}
	}
}

func TestHTTP_HSTSOnlyForHTTPSPublicURLAndOff(t *testing.T) {
	cases := []struct {
		env  []string
		want string
	}{
		{nil, "max-age=31536000"},
		{[]string{"REQHARBOR_HSTS=max-age=63072000; includeSubDomains; preload"}, "max-age=63072000; includeSubDomains; preload"},
		{[]string{"REQHARBOR_HSTS=off"}, ""},
		{[]string{"REQHARBOR_PUBLIC_URL=http://192.168.1.5:8080"}, ""},
	}
	for _, c := range cases {
		h := tu.New(t, tu.WithEnv(c.env...))
		if got := h.Call(t, "/healthz", tu.Req{}).Header.Get("Strict-Transport-Security"); got != c.want {
			t.Fatalf("%v: HSTS %q, want %q", c.env, got, c.want)
		}
	}
}

func TestHTTP_NoCORSOnV1(t *testing.T) {
	h := tu.New(t)
	ws := h.NewWorkspace(t, "")
	res := h.Call(t, "/v1/workspace", tu.Req{Token: ws.Token, Headers: map[string]string{"Origin": "https://evil.example"}})
	if res.Header.Get("Access-Control-Allow-Origin") != "" {
		t.Fatal("CORS header on /v1")
	}
	pre := h.Call(t, "/v1/sync", tu.Req{Method: "OPTIONS", Headers: map[string]string{"Origin": "https://evil.example"}})
	if pre.StatusCode != 405 || pre.Header.Get("Access-Control-Allow-Origin") != "" {
		t.Fatalf("got %d", pre.StatusCode)
	}
}

// failingStore fails every transaction, like the Worker test's broken D1 binding.
type failingStore struct{}

var errSecret = errors.New("D1_ERROR: secret internal detail")

func (failingStore) Read(context.Context, func(store.ReadTx) error) error   { return errSecret }
func (failingStore) Write(context.Context, func(store.WriteTx) error) error { return errSecret }

func TestHTTP_AC_SYNC_6_InternalFailuresAre500WithoutLeakingDetails(t *testing.T) {
	h := tu.New(t, tu.WithStore(failingStore{}))
	res := h.Call(t, "/v1/sync", tu.Req{Token: "rh_" + strings.Repeat("A", 43)})
	if res.StatusCode != 500 || res.Header.Get("Content-Type") != "application/json; charset=utf-8" {
		t.Fatalf("got %d %q", res.StatusCode, res.Header.Get("Content-Type"))
	}
	tu.JSONEq(t, res.Body, `{"error":{"code":"internal_error","message":"Internal server error"}}`)
	if strings.Contains(string(res.Body), "secret internal detail") {
		t.Fatal("leaked detail")
	}
	if !strings.Contains(h.Logs.String(), "secret internal detail") {
		t.Fatal("the detail should be in the log")
	}
}
