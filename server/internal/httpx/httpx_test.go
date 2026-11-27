package httpx

import (
	"bytes"
	"context"
	"encoding/json"
	"io"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
)

func serve(t *testing.T, h http.Handler, req *http.Request) *httptest.ResponseRecorder {
	t.Helper()
	rec := httptest.NewRecorder()
	h.ServeHTTP(rec, req)
	return rec
}

func wrapped(mux *http.ServeMux, logs io.Writer, hsts string) http.Handler {
	return Wrap(CapturePattern(mux), slog.New(slog.NewJSONHandler(logs, nil)), SecurityOptions{HSTS: hsts})
}

func errorOf(t *testing.T, rec *httptest.ResponseRecorder) (string, string) {
	t.Helper()
	var b struct {
		Error struct{ Code, Message string } `json:"error"`
	}
	if err := json.Unmarshal(rec.Body.Bytes(), &b); err != nil {
		t.Fatalf("not an error envelope: %s", rec.Body)
	}
	return b.Error.Code, b.Error.Message
}

func TestMethods_405WithAllowInRegistrationOrder(t *testing.T) {
	ok := http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) { w.WriteHeader(204) })
	m := Methods{{"POST", ok}, {"GET", ok}, {"DELETE", ok}}
	if rec := serve(t, m, httptest.NewRequest("GET", "/x", nil)); rec.Code != 204 {
		t.Fatalf("GET %d", rec.Code)
	}
	for _, method := range []string{"HEAD", "PUT", "OPTIONS", "get"} {
		rec := serve(t, m, httptest.NewRequest(method, "/x", nil))
		code, msg := errorOf(t, rec)
		if rec.Code != 405 || rec.Header().Get("Allow") != "POST, GET, DELETE" || code != "method_not_allowed" || msg != "Method "+method+" not allowed; use POST, GET, DELETE" {
			t.Fatalf("%s: %d %q %s", method, rec.Code, rec.Header().Get("Allow"), rec.Body)
		}
		if rec.Header().Get("Content-Type") != ContentTypeJSON {
			t.Fatal("content type")
		}
	}
}

func TestNotFoundHandler(t *testing.T) {
	rec := serve(t, NotFoundHandler, httptest.NewRequest("DELETE", "/a%20b/c", nil))
	code, msg := errorOf(t, rec)
	if rec.Code != 404 || code != "not_found" || msg != "No route for DELETE /a%20b/c" {
		t.Fatalf("%d %s", rec.Code, rec.Body)
	}
}

func TestReadJSON_OrderMatrix(t *testing.T) {
	const max = 64
	cases := []struct {
		name, ct, body string
		contentLength  int64 // 0: from body; -1: unknown
		status         int
		code           string
	}{
		{"wrong type first", "text/plain", strings.Repeat("x", 100), 0, 415, "unsupported_media_type"},
		{"declared too large", "application/json", strings.Repeat("x", 100), 0, 413, "payload_too_large"},
		{"missing body", "application/json", "", 0, 400, "invalid_json"},
		{"streamed too large", "application/json", strings.Repeat(" ", 100), -1, 413, "payload_too_large"},
		{"invalid utf-8", "application/json", "{\"a\":\"\xc3\x28\"}", 0, 400, "invalid_json"},
		{"blank", "application/json", " \t\r\n", 0, 400, "invalid_json"},
		{"syntax", "application/json", "{", 0, 400, "invalid_json"},
		{"bom stripped", "application/json", "\xef\xbb\xbf{}", 0, 200, ""},
		{"two boms", "application/json", "\xef\xbb\xbf\xef\xbb\xbf{}", 0, 400, "invalid_json"},
		{"ct params", "application/json;charset=utf-8", "{}", 0, 200, ""},
		{"ct case", "APPLICATION/JSON", "{}", 0, 200, ""},
		{"ct prefix only", "application/json-patch+json", "{}", 0, 415, "unsupported_media_type"},
		{"not an object", "application/json", "[1]", 0, 400, "validation_failed"},
		{"null", "application/json", "null", 0, 400, "validation_failed"},
		{"string", "application/json", `"{}"`, 0, 400, "validation_failed"},
		{"too deep", "application/json", strings.Repeat("[", 60), 0, 400, "invalid_json"},
	}
	for _, c := range cases {
		var body io.Reader = strings.NewReader(c.body)
		if c.body == "" {
			body = nil
		}
		req := httptest.NewRequest("POST", "/x", body)
		if c.ct != "" {
			req.Header.Set("Content-Type", c.ct)
		}
		if c.contentLength == -1 {
			req.ContentLength = -1
		}
		rec := httptest.NewRecorder()
		_, err := ReadJSONObject(rec, req, max)
		if c.status == 200 {
			if err != nil {
				t.Fatalf("%s: %v", c.name, err)
			}
			continue
		}
		e, ok := AsAPIError(err)
		if !ok || e.Status != c.status || e.Code != c.code {
			t.Fatalf("%s: got %v, want %d %s", c.name, err, c.status, c.code)
		}
	}
}

func TestWrap_SecurityHeaderDefaultsAndHandlerOverrides(t *testing.T) {
	mux := http.NewServeMux()
	mux.HandleFunc("/plain", func(w http.ResponseWriter, r *http.Request) { _, _ = w.Write([]byte("hi")) })
	mux.HandleFunc("/custom", func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Security-Policy", "default-src 'self'")
		w.Header().Set("Cache-Control", "public, max-age=60")
		w.Header().Set("Cross-Origin-Resource-Policy", "cross-origin")
		w.Header().Set("X-Frame-Options", "SAMEORIGIN") // never weaker than DENY
		w.WriteHeader(201)
	})
	mux.HandleFunc("/v1/cookie", func(w http.ResponseWriter, r *http.Request) {
		http.SetCookie(w, &http.Cookie{Name: "a", Value: "b"})
	})
	mux.HandleFunc("/auth/cookie", func(w http.ResponseWriter, r *http.Request) {
		http.SetCookie(w, &http.Cookie{Name: "a", Value: "b"})
	})
	h := wrapped(mux, io.Discard, "max-age=31536000")

	rec := serve(t, h, httptest.NewRequest("GET", "/plain", nil))
	for k, want := range map[string]string{
		"X-Content-Type-Options":       "nosniff",
		"Referrer-Policy":              "no-referrer",
		"X-Frame-Options":              "DENY",
		"Cross-Origin-Opener-Policy":   "same-origin",
		"Cross-Origin-Resource-Policy": "same-origin",
		"Permissions-Policy":           "camera=(), microphone=(), geolocation=(), interest-cohort=()",
		"Strict-Transport-Security":    "max-age=31536000",
		"Content-Security-Policy":      APICSP,
		"Cache-Control":                "no-store",
	} {
		if got := rec.Header().Get(k); got != want {
			t.Errorf("%s = %q, want %q", k, got, want)
		}
	}
	if len(rec.Header().Get("X-Request-Id")) != 24 {
		t.Errorf("request id %q", rec.Header().Get("X-Request-Id"))
	}

	rec = serve(t, h, httptest.NewRequest("GET", "/custom", nil))
	if rec.Code != 201 || rec.Header().Get("Content-Security-Policy") != "default-src 'self'" || rec.Header().Get("Cache-Control") != "public, max-age=60" ||
		rec.Header().Get("Cross-Origin-Resource-Policy") != "cross-origin" || rec.Header().Get("X-Frame-Options") != "DENY" {
		t.Errorf("overrides: %d %v", rec.Code, rec.Header())
	}

	if rec := serve(t, h, httptest.NewRequest("GET", "/v1/cookie", nil)); rec.Header().Get("Set-Cookie") != "" {
		t.Error("Set-Cookie kept outside /auth/")
	}
	if rec := serve(t, h, httptest.NewRequest("GET", "/auth/cookie", nil)); rec.Header().Get("Set-Cookie") == "" {
		t.Error("Set-Cookie dropped under /auth/")
	}
	if rec := serve(t, wrapped(mux, io.Discard, ""), httptest.NewRequest("GET", "/plain", nil)); rec.Header().Get("Strict-Transport-Security") != "" {
		t.Error("HSTS sent while off")
	}
}

func TestWrap_PanicBecomes500WithoutLeakAndAccessLogUsesThePattern(t *testing.T) {
	mux := http.NewServeMux()
	mux.HandleFunc("/s/{file}", func(w http.ResponseWriter, r *http.Request) { panic("secret internal detail") })
	mux.Handle("/err", Func(func(w http.ResponseWriter, r *http.Request) error { return io.ErrUnexpectedEOF }))
	var logs bytes.Buffer
	h := wrapped(mux, &logs, "")
	rec := serve(t, h, httptest.NewRequest("GET", "/s/AbCdEf?token=x", nil))
	code, msg := errorOf(t, rec)
	if rec.Code != 500 || code != "internal_error" || msg != "Internal server error" || strings.Contains(rec.Body.String(), "secret") {
		t.Fatalf("%d %s", rec.Code, rec.Body)
	}
	if rec.Header().Get("Content-Security-Policy") == "" {
		t.Error("security headers missing on 500")
	}
	l := logs.String()
	if !strings.Contains(l, "secret internal detail") || !strings.Contains(l, `"route":"/s/{file}"`) || strings.Contains(l, "AbCdEf") || strings.Contains(l, "token=x") {
		t.Fatalf("log: %s", l)
	}
	if !strings.Contains(l, `"status":500`) || !strings.Contains(l, rec.Header().Get("X-Request-Id")) {
		t.Fatalf("log: %s", l)
	}
	rec = serve(t, h, httptest.NewRequest("GET", "/err", nil))
	if code, _ := errorOf(t, rec); rec.Code != 500 || code != "internal_error" {
		t.Fatalf("%d", rec.Code)
	}
}

func TestRateLimitedHasRetryAfter(t *testing.T) {
	e := RateLimited("slow down", 0)
	if e.Status != 429 || e.Code != "rate_limited" || e.Headers["Retry-After"] != "1" {
		t.Fatalf("%+v", e)
	}
}

func TestMarshalDoesNotEscapeHTMLOrAddANewline(t *testing.T) {
	b, err := Marshal(map[string]string{"a": "<&>"})
	if err != nil || string(b) != `{"a":"<&>"}` {
		t.Fatalf("%s %v", b, err)
	}
}

func TestQueryGet_URLSearchParamsSemantics(t *testing.T) {
	for q, want := range map[string]string{
		"since=0;DROP":        "0;DROP",
		"since=1%20OR%201=1":  "1 OR 1=1",
		"since=a+b":           "a b",
		"since=%zz":           "%zz",
		"x=1&since=5&since=6": "5",
		"since":               "",
		"since=":              "",
		"other=1":             "",
		"si%6Ece=7":           "7",
		"since=%E2%82%AC&x=%": "€",
	} {
		req := httptest.NewRequest("GET", "/x?"+q, nil)
		if got := QueryGet(req, "since"); got != want {
			t.Errorf("%q → %q, want %q", q, got, want)
		}
	}
}

func TestHandleError_AfterTheResponseStartedOnlyLogs(t *testing.T) {
	var logs bytes.Buffer
	mux := http.NewServeMux()
	mux.Handle("/partial", Func(func(w http.ResponseWriter, r *http.Request) error {
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write([]byte("half"))
		return io.ErrUnexpectedEOF
	}))
	mux.Handle("/api", Func(func(w http.ResponseWriter, r *http.Request) error {
		return NewError(409, "conflict", "Nope")
	}))
	mux.Handle("/canceled", Func(func(w http.ResponseWriter, r *http.Request) error {
		return r.Context().Err()
	}))
	h := wrapped(mux, &logs, "")
	rec := serve(t, h, httptest.NewRequest("GET", "/partial", nil))
	if rec.Code != 200 || rec.Body.String() != "half" {
		t.Fatalf("partial: %d %q", rec.Code, rec.Body)
	}
	if !strings.Contains(logs.String(), "error after the response started") {
		t.Fatalf("not logged: %s", logs.String())
	}
	rec = serve(t, h, httptest.NewRequest("GET", "/api", nil))
	if code, _ := errorOf(t, rec); rec.Code != 409 || code != "conflict" {
		t.Fatalf("api error: %d %s", rec.Code, rec.Body)
	}
	logs.Reset()
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	rec = serve(t, h, httptest.NewRequest("GET", "/canceled", nil).WithContext(ctx))
	if rec.Code != 500 || strings.Contains(logs.String(), `"level":"ERROR"`) {
		t.Fatalf("canceled: %d %s", rec.Code, logs.String())
	}
}
