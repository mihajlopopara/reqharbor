// Package testutil is the Go port of worker/test/helpers.ts: an in-process server on a temporary
// data directory plus request helpers (06-go-server.md §1.6).
package testutil

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"log/slog"
	"math/rand/v2"
	"net/http"
	"net/http/httptest"
	"reflect"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/mihajlopopara/reqharbor/server/internal/app"
	"github.com/mihajlopopara/reqharbor/server/internal/clock"
	"github.com/mihajlopopara/reqharbor/server/internal/config"
	"github.com/mihajlopopara/reqharbor/server/internal/db"
	"github.com/mihajlopopara/reqharbor/server/internal/store"
	"github.com/mihajlopopara/reqharbor/server/internal/store/sqlite"
)

// CanonicalHost is the host of the harness PUBLIC_URL.
const CanonicalHost = "reqharbor.test"

// Harness is a running in-process server.
type Harness struct {
	URL    string
	Server *httptest.Server
	App    *app.App
	Store  store.Store
	DB     *db.DB // nil with WithStore
	Clock  *clock.Fake
	Config *config.Config
	Logs   *Buffer
	Client *http.Client
	// ClientVersion, when set, is sent as X-ReqHarbor-Version by the token helpers.
	ClientVersion string

	logger     *slog.Logger
	httpClient *http.Client
}

type options struct {
	dataDir string
	env     []string
	store   store.Store
	config  []func(*config.Config)
	client  *http.Client
	wrap    func(store.Store) store.Store
}

// Option customises New.
type Option func(*options)

// WithEnv adds or overrides REQHARBOR_* settings ("KEY=VALUE").
func WithEnv(kv ...string) Option { return func(o *options) { o.env = append(o.env, kv...) } }

// WithConfig changes the parsed configuration before the server starts (for the test-only
// fields no environment variable reaches, such as config.TestOverrides).
func WithConfig(f func(*config.Config)) Option {
	return func(o *options) { o.config = append(o.config, f) }
}

// WithHTTPClient sets the client of the outbound provider calls.
func WithHTTPClient(c *http.Client) Option { return func(o *options) { o.client = c } }

// WithDataDir serves an existing data directory (for example a database seeded at an older
// schema) instead of a fresh one; New migrates it like serve does.
func WithDataDir(dir string) Option { return func(o *options) { o.dataDir = dir } }

// WithStoreWrap wraps the real SQLite store (tests: a store that runs a hook before a write).
func WithStoreWrap(f func(store.Store) store.Store) Option { return func(o *options) { o.wrap = f } }

// WithStore serves from a stub store instead of a real database.
func WithStore(s store.Store) Option { return func(o *options) { o.store = s } }

// Buffer is a goroutine-safe log sink.
type Buffer struct {
	mu sync.Mutex
	b  bytes.Buffer
}

func (b *Buffer) Write(p []byte) (int, error) {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.b.Write(p)
}

func (b *Buffer) String() string {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.b.String()
}

// Count returns how many log lines contain s.
func (b *Buffer) Count(s string) int { return strings.Count(b.String(), s) }

// DefaultEnv is the harness configuration; options append to it (later entries win).
func DefaultEnv(dataDir string) []string {
	return []string{
		"REQHARBOR_PUBLIC_URL=https://" + CanonicalHost,
		"REQHARBOR_LISTEN=127.0.0.1:0",
		"REQHARBOR_DATA_DIR=" + dataDir,
		"REQHARBOR_LEGACY_WORKSPACES=open",
		"REQHARBOR_TRUSTED_PROXIES=127.0.0.1/32",
		"REQHARBOR_CLIENT_IP_HEADER=CF-Connecting-IP",
		"REQHARBOR_MIN_FREE_DISK_BYTES=0",
		"REQHARBOR_ARGON2_MEMORY_KIB=8192", // the minimum: keeps password tests fast
	}
}

// ParseEnv parses an env list the way the harness does, failing the test on error.
func ParseEnv(t testing.TB, env []string) *config.Config {
	t.Helper()
	res, err := config.Parse(config.Options{Environ: env})
	if err != nil {
		t.Fatalf("config: %v", err)
	}
	return res.Config
}

// New starts a server on a fresh temporary data directory with the migrations applied.
func New(t testing.TB, opts ...Option) *Harness {
	t.Helper()
	var o options
	for _, opt := range opts {
		opt(&o)
	}
	dir := o.dataDir
	if dir == "" {
		dir = t.TempDir()
	}
	cfg := ParseEnv(t, append(DefaultEnv(dir), o.env...))
	for _, f := range o.config {
		f(cfg)
	}
	logs := &Buffer{}
	logger := slog.New(slog.NewJSONHandler(logs, &slog.HandlerOptions{Level: slog.LevelDebug}))
	clk := clock.NewFake(time.Date(2026, 11, 27, 12, 0, 0, 0, time.UTC))
	h := &Harness{Clock: clk, Config: cfg, Logs: logs, logger: logger, httpClient: o.client}

	st := o.store
	if st == nil {
		d, err := db.Open(cfg.DataDir, db.Options{Lock: true})
		if err != nil {
			t.Fatalf("open db: %v", err)
		}
		t.Cleanup(func() { d.Close() })
		if _, err := d.Migrate(context.Background(), 0, clk.Now); err != nil {
			t.Fatalf("migrate: %v", err)
		}
		h.DB = d
		st = sqlite.New(d)
		if o.wrap != nil {
			st = o.wrap(st)
		}
	}
	h.Store = st
	deps := app.Deps{Config: cfg, Clock: clk, Store: st, Logger: logger, Version: app.DefaultVersion, DB: h.DB, HTTPClient: o.client}
	h.App = app.New(deps)
	if o.store == nil {
		if err := h.App.Init(context.Background()); err != nil {
			t.Fatalf("init: %v", err)
		}
	} else {
		h.App.ForceReady()
	}
	h.Server = httptest.NewServer(h.App.Handler())
	t.Cleanup(h.Server.Close)
	h.URL = h.Server.URL
	h.Client = &http.Client{CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }}
	return h
}

// Restart stops the server and starts a new process-equivalent on the same data directory: a new
// App (fresh in-memory limiters and throttle counters) over a reopened database. The URL changes.
func (h *Harness) Restart(t testing.TB) {
	t.Helper()
	if h.DB == nil {
		t.Fatal("Restart needs a real database")
	}
	h.Server.Close()
	h.DB.Close()
	d, err := db.Open(h.Config.DataDir, db.Options{Lock: true})
	if err != nil {
		t.Fatalf("reopen db: %v", err)
	}
	t.Cleanup(func() { d.Close() })
	h.DB = d
	h.Store = sqlite.New(d)
	h.App = app.New(app.Deps{Config: h.Config, Clock: h.Clock, Store: h.Store, Logger: h.logger, Version: app.DefaultVersion, DB: d,
		HTTPClient: h.httpClient})
	if err := h.App.Init(context.Background()); err != nil {
		t.Fatalf("init: %v", err)
	}
	h.Server = httptest.NewServer(h.App.Handler())
	t.Cleanup(h.Server.Close)
	h.URL = h.Server.URL
}

// Req describes one call.
type Req struct {
	Method  string
	Token   string
	JSON    any       // marshalled; sets Content-Type: application/json unless Headers sets one
	Raw     *string   // raw body text
	Body    io.Reader // streamed raw body (no Content-Length)
	Headers map[string]string
}

// Resp is a response with its body read.
type Resp struct {
	*http.Response
	Body []byte
}

// JSON decodes the body into a generic value.
func (r *Resp) JSON(t testing.TB) map[string]any {
	t.Helper()
	var v map[string]any
	if err := json.Unmarshal(r.Body, &v); err != nil {
		t.Fatalf("response %d is not a JSON object: %v: %s", r.StatusCode, err, truncate(r.Body))
	}
	return v
}

// ErrorCode returns error.code of an error envelope.
func (r *Resp) ErrorCode(t testing.TB) string {
	t.Helper()
	e, _ := r.JSON(t)["error"].(map[string]any)
	code, _ := e["code"].(string)
	return code
}

func truncate(b []byte) string {
	if len(b) > 300 {
		return string(b[:300]) + "…"
	}
	return string(b)
}

// Call sends a request. By default it is on the canonical origin: Host reqharbor.test and
// X-Forwarded-Proto https from the trusted 127.0.0.1 peer.
func (h *Harness) Call(t testing.TB, path string, r Req) *Resp {
	t.Helper()
	var body io.Reader
	switch {
	case r.Body != nil:
		body = r.Body
	case r.Raw != nil:
		body = strings.NewReader(*r.Raw)
	case r.JSON != nil:
		b, err := json.Marshal(r.JSON)
		if err != nil {
			t.Fatal(err)
		}
		body = bytes.NewReader(b)
	}
	method := r.Method
	if method == "" {
		method = "GET"
		if body != nil {
			method = "POST"
		}
	}
	req, err := http.NewRequest(method, h.URL+path, body)
	if err != nil {
		t.Fatal(err)
	}
	if r.Body != nil {
		req.ContentLength = -1
	}
	req.Host = CanonicalHost
	req.Header.Set("X-Forwarded-Proto", "https")
	if r.JSON != nil {
		req.Header.Set("Content-Type", "application/json")
	}
	if r.Token != "" {
		req.Header.Set("Authorization", "Bearer "+r.Token)
	}
	for k, v := range r.Headers {
		if strings.EqualFold(k, "Host") {
			req.Host = v
			continue
		}
		req.Header.Set(k, v)
	}
	res, err := h.Client.Do(req)
	if err != nil {
		t.Fatalf("%s %s: %v", method, path, err)
	}
	defer res.Body.Close()
	b, err := io.ReadAll(res.Body)
	if err != nil {
		t.Fatalf("read body: %v", err)
	}
	return &Resp{Response: res, Body: b}
}

// Str returns a pointer to s (for Req.Raw).
func Str(s string) *string { return &s }

// RandomIP is a random 10.x.y.z address so the workspace-creation limit never couples tests.
func RandomIP() string {
	return fmt.Sprintf("10.%d.%d.%d", rand.IntN(256), rand.IntN(256), rand.IntN(256))
}

// Workspace is a created legacy workspace.
type Workspace struct {
	ID    string
	Token string
}

// NewWorkspace creates a legacy workspace from a random client IP.
func (h *Harness) NewWorkspace(t testing.TB, name string) Workspace {
	t.Helper()
	if name == "" {
		name = "Test workspace"
	}
	res := h.Call(t, "/v1/workspaces", Req{JSON: map[string]any{"name": name}, Headers: map[string]string{"CF-Connecting-IP": RandomIP()}})
	if res.StatusCode != 201 {
		t.Fatalf("create workspace failed: %d %s", res.StatusCode, res.Body)
	}
	v := res.JSON(t)
	return Workspace{ID: v["workspaceId"].(string), Token: v["token"].(string)}
}

// Change builds a pushed change; data nil means {id, name: "<type> <id>"}.
func Change(typ, id string, updatedAt int64, data map[string]any) map[string]any {
	if data == nil {
		data = map[string]any{"id": id, "name": typ + " " + id}
	}
	return map[string]any{"type": typ, "id": id, "updatedAt": updatedAt, "deleted": false, "data": data}
}

// Tombstone builds a pushed delete.
func Tombstone(typ, id string, updatedAt int64) map[string]any {
	return map[string]any{"type": typ, "id": id, "updatedAt": updatedAt, "deleted": true, "data": nil}
}

// Changes wraps changes in a push body.
func Changes(cs ...map[string]any) map[string]any {
	list := make([]any, len(cs))
	for i, c := range cs {
		list[i] = c
	}
	return map[string]any{"changes": list}
}

// SampleRequest is the Worker tests' request fixture.
func SampleRequest(overrides map[string]any) map[string]any {
	r := map[string]any{
		"id":           "01REQ0000000000000000000001",
		"collectionId": "01COL0000000000000000000001",
		"folderId":     nil,
		"name":         "List users",
		"method":       "GET",
		"url":          "{{baseUrl}}/users?page=1",
		"params":       []any{map[string]any{"key": "page", "value": "1", "enabled": true}},
		"headers": []any{
			map[string]any{"key": "Accept", "value": "application/json", "enabled": true},
			map[string]any{"key": "X-Debug", "value": "1", "enabled": false},
		},
		"auth":      map[string]any{"type": "none"},
		"body":      map[string]any{"type": "none"},
		"settings":  map[string]any{"followRedirects": true, "timeoutMs": 30000, "verifyTls": true},
		"sortOrder": 0,
	}
	for k, v := range overrides {
		r[k] = v
	}
	return r
}

// SampleCollectionSnapshot is the Worker tests' collection share snapshot.
func SampleCollectionSnapshot() map[string]any {
	return map[string]any{
		"schemaVersion": 1,
		"collection": map[string]any{
			"id":          "01COL0000000000000000000001",
			"name":        "Users API",
			"description": "Endpoints for users",
			"auth":        map[string]any{"type": "bearer", "token": "{{token}}"},
			"folders": []any{
				map[string]any{"id": "F1", "collectionId": "01COL0000000000000000000001", "parentFolderId": nil, "name": "Admin", "sortOrder": 1},
				map[string]any{"id": "F2", "collectionId": "01COL0000000000000000000001", "parentFolderId": "F1", "name": "Danger zone", "sortOrder": 0},
			},
			"requests": []any{
				SampleRequest(nil),
				SampleRequest(map[string]any{"id": "R2", "folderId": "F1", "name": "Search users", "method": "QUERY",
					"url": "{{baseUrl}}/users/search", "auth": map[string]any{"type": "inherit"},
					"body": map[string]any{"type": "json", "text": `{"q":"ada"}`}, "sortOrder": 0}),
				SampleRequest(map[string]any{"id": "R3", "folderId": "F2", "name": "Delete user", "method": "DELETE",
					"url": "https://api.example.com/users/1", "sortOrder": 0}),
			},
		},
		"environment": map[string]any{
			"name": "Staging",
			"variables": []any{
				map[string]any{"key": "baseUrl", "value": "https://staging.example.com", "enabled": true, "secret": false},
				map[string]any{"key": "token", "value": "super-secret-token", "enabled": true, "secret": true},
			},
		},
	}
}

// JSONEq fails unless got and want (JSON texts or values) are structurally equal.
func JSONEq(t testing.TB, got any, want any) {
	t.Helper()
	norm := func(v any) any {
		var b []byte
		switch x := v.(type) {
		case []byte:
			b = x
		case string:
			b = []byte(x)
		default:
			var err error
			if b, err = json.Marshal(x); err != nil {
				t.Fatal(err)
			}
		}
		var out any
		if err := json.Unmarshal(b, &out); err != nil {
			t.Fatalf("not JSON: %v: %s", err, truncate(b))
		}
		return out
	}
	g, w := norm(got), norm(want)
	if !reflect.DeepEqual(g, w) {
		gb, _ := json.Marshal(g)
		wb, _ := json.Marshal(w)
		t.Fatalf("JSON mismatch\n got: %s\nwant: %s", truncate(gb), truncate(wb))
	}
}
