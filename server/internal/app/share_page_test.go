package app_test

// Port of the share-page cases of worker/test/shares.test.ts and the escaping case of
// worker/test/qa_parity_security.test.ts (06-go-server.md §1.6, S2). html/template writes " as
// &#34; and + as &#43; where the Worker wrote &quot; and +; nothing depends on that (§1.5).

import (
	"crypto/sha256"
	"encoding/base64"
	"os"
	"regexp"
	"strings"
	"testing"
	"time"

	tu "github.com/mihajlopopara/reqharbor/server/internal/testutil"
)

func sharePage(t *testing.T, h *tu.Harness, id string) (*tu.Resp, string) {
	t.Helper()
	res := h.Call(t, "/s/"+id, tu.Req{})
	return res, string(res.Body)
}

func mustContain(t *testing.T, text string, subs ...string) {
	t.Helper()
	for _, s := range subs {
		if !strings.Contains(text, s) {
			t.Errorf("page lacks %q", s)
		}
	}
}

func TestSharePage_AC_SHARE_3_TreeBadgesURLsHeadersBodiesAndCopyAsCurl(t *testing.T) {
	h := tu.New(t)
	ws := h.NewWorkspace(t, "")
	id := createShare(t, h, ws.Token, nil, "")["shareId"].(string)
	res, text := sharePage(t, h, id)
	if res.StatusCode != 200 || res.Header.Get("Content-Type") != "text/html; charset=utf-8" {
		t.Fatalf("%d %q", res.StatusCode, res.Header.Get("Content-Type"))
	}
	// tree: collection > Admin > Danger zone
	mustContain(t, text, `<nav class="tree"`,
		`Users API <span aria-hidden="true">›</span> Admin <span aria-hidden="true">›</span> Danger zone`,
		`<span class="m m-get">GET</span>`, `<span class="m m-delete">DELETE</span>`,
		`<span class="var">{{baseUrl}}</span>/users?page=1`,
		"<td>Accept</td><td>application/json</td>",
		`<tr class="off"><td>X-Debug</td>`,
		"&#34;q&#34;: &#34;ada&#34;", // pretty JSON body, escaped
		// Same format as the app's exporter: URL rebuilt from params, every part on its own line.
		"curl \\\n  -X QUERY \\\n  &#39;https://staging.example.com/users/search?page=1&#39; \\\n  -H &#39;Accept: application/json&#39;",
		// environment attached: the raw variant for the "Keep {{variables}}" toggle
		"&#39;{{baseUrl}}/users/search?page=1&#39; \\\n  --globoff",
		"Authorization: Bearer {{token}}", // inherited auth; the secret token is never resolved
	)
	if !regexp.MustCompile(`(?s)Admin.*Danger zone`).MatchString(text) {
		t.Error("folder order")
	}
	if n := len(regexp.MustCompile(`data-copy="r\d+"`).FindAllString(text, -1)); n != 3 {
		t.Errorf("copy buttons: %d", n)
	}
	// Read-only: no forms or editable inputs other than the variables toggle.
	if regexp.MustCompile(`(?i)<form|<textarea|contenteditable`).MatchString(text) || strings.Count(text, "<input") != 1 {
		t.Error("page is not read-only")
	}
}

func TestSharePage_AC_QRY_1_QUERYGetsTheTealBadge(t *testing.T) {
	h := tu.New(t)
	ws := h.NewWorkspace(t, "")
	_, text := sharePage(t, h, createShare(t, h, ws.Token, nil, "")["shareId"].(string))
	mustContain(t, text, `<span class="m m-query">QUERY</span>`, "--method-query:#2fc9c0", ".m-query{color:var(--method-query)}")
}

func TestSharePage_AC_SHARE_3_UnexportableRequestsShowTheErrorInsteadOfCopy(t *testing.T) {
	h := tu.New(t)
	ws := h.NewWorkspace(t, "")
	id := createShare(t, h, ws.Token, map[string]any{"schemaVersion": 1, "request": tu.SampleRequest(map[string]any{"name": "Bad", "method": "get"})}, "request")["shareId"].(string)
	res, text := sharePage(t, h, id)
	if res.StatusCode != 200 {
		t.Fatal(res.StatusCode)
	}
	mustContain(t, text, "Can't export as cURL: Invalid method &#34;get&#34;")
	if strings.Contains(text, "data-copy=") {
		t.Error("copy button on an unexportable request")
	}
}

func TestSharePage_AC_SHARE_3_ASingleRequestRendersWithoutTheTree(t *testing.T) {
	h := tu.New(t)
	ws := h.NewWorkspace(t, "")
	id := createShare(t, h, ws.Token, map[string]any{"schemaVersion": 1, "request": tu.SampleRequest(map[string]any{"name": "Solo"})}, "request")["shareId"].(string)
	res, text := sharePage(t, h, id)
	if res.StatusCode != 200 || strings.Contains(text, `<nav class="tree"`) || strings.Count(text, "data-copy=") != 1 {
		t.Fatalf("%d\n%s", res.StatusCode, text)
	}
	mustContain(t, text, "<h1>Solo</h1>", `<div class="layout single">`)
}

func b64sha(b []byte) string {
	s := sha256.Sum256(b)
	return base64.StdEncoding.EncodeToString(s[:])
}

func TestSharePage_AC_SHARE_3_StrictCSPWhoseHashesMatchTheInlineAssets(t *testing.T) {
	h := tu.New(t)
	ws := h.NewWorkspace(t, "")
	id := createShare(t, h, ws.Token, nil, "")["shareId"].(string)
	res, page := sharePage(t, h, id)
	csp := res.Header.Get("Content-Security-Policy")
	for _, re := range []string{`default-src 'none'`, `frame-ancestors 'none'`, `script-src 'sha256-[A-Za-z0-9+/]+=*'`, `style-src 'sha256-[A-Za-z0-9+/]+=*'`} {
		if !regexp.MustCompile(re).MatchString(csp) {
			t.Errorf("csp %q lacks %s", csp, re)
		}
	}
	if strings.Contains(csp, "unsafe-inline") || strings.Contains(csp, "unsafe-eval") {
		t.Fatalf("csp %q", csp)
	}
	// The hashes are those of web/static/share.{css,js}, each embedded exactly once.
	css, err := os.ReadFile("../web/static/share.css")
	if err != nil {
		t.Fatal(err)
	}
	js, err := os.ReadFile("../web/static/share.js")
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(csp, "'sha256-"+b64sha(js)+"'") || !strings.Contains(csp, "'sha256-"+b64sha(css)+"'") {
		t.Fatalf("hash mismatch: %q", csp)
	}
	if strings.Count(page, "<style>"+string(css)+"</style>") != 1 || strings.Count(page, "<script>"+string(js)+"</script>") != 1 || strings.Count(page, "<script>") != 1 {
		t.Fatal("inline assets must appear exactly once")
	}
	// No external resources at all.
	if regexp.MustCompile(`<(link|img|iframe)\b|src=`).MatchString(page) {
		t.Fatal("external resource")
	}
	for _, m := range regexp.MustCompile(`https?://[^\s'"<&]*`).FindAllString(page, -1) {
		if !strings.HasPrefix(m, "https://staging.example.com") && !strings.HasPrefix(m, "https://api.example.com") {
			t.Fatalf("unexpected URL %q", m)
		}
	}
	// The error pages carry the same policy and style.
	gone := h.Call(t, "/s/AAAAAAAAAAAAAAAAAAAAAA", tu.Req{})
	if gone.StatusCode != 404 || gone.Header.Get("Content-Security-Policy") != csp || strings.Count(string(gone.Body), "<style>"+string(css)+"</style>") != 1 {
		t.Fatalf("error page %d %q", gone.StatusCode, gone.Header.Get("Content-Security-Policy"))
	}
	mustContain(t, string(gone.Body), "<h1>Share not found</h1>")
}

func TestSharePage_CSPMatchesTheWorkersPolicy(t *testing.T) {
	// The assets are byte-for-byte the Worker's PAGE_CSS and PAGE_JS, so the policy is identical.
	src, err := os.ReadFile("../../../worker/src/share-page.ts")
	if err != nil {
		t.Skip("worker/ not present:", err)
	}
	for _, c := range []struct{ name, file string }{{"PAGE_CSS", "../web/static/share.css"}, {"PAGE_JS", "../web/static/share.js"}, {"LOGO_SVG", "../web/static/logo.svg"}} {
		m := regexp.MustCompile(`(?s)const ` + c.name + " = `(.*?)`;").FindSubmatch(src)
		got, err := os.ReadFile(c.file)
		if m == nil || err != nil || string(m[1]) != string(got) {
			t.Errorf("%s differs from %s", c.file, c.name)
		}
	}
}

func TestSharePage_XSS_EveryPieceOfUserContentIsEscaped(t *testing.T) {
	h := tu.New(t)
	ws := h.NewWorkspace(t, "")
	x := `<script>alert(1)</script>"><img src=x onerror=alert(2)>'`
	snapshot := map[string]any{
		"schemaVersion": 1,
		"collection": map[string]any{
			"name": x, "description": x,
			"auth":    map[string]any{"type": "basic", "username": x, "password": x},
			"folders": []any{map[string]any{"id": "f", "name": x, "parentFolderId": nil}},
			"requests": []any{
				map[string]any{"id": "r", "folderId": "f", "name": x, "method": x, "url": "javascript:alert(3)//" + x,
					"params": []any{map[string]any{"key": x, "value": x, "enabled": true}}, "headers": []any{map[string]any{"key": x, "value": x, "enabled": true}},
					"auth": map[string]any{"type": "inherit"}, "body": map[string]any{"type": "json", "text": x}},
				map[string]any{"id": "r2", "name": "form", "method": "POST", "url": "https://a.test", "headers": []any{},
					"auth": map[string]any{"type": "apikey", "apiKey": map[string]any{"key": x, "value": x, "in": "header"}},
					"body": map[string]any{"type": "form", "form": []any{map[string]any{"key": x, "value": x, "kind": "file", "enabled": true}}}},
				map[string]any{"id": "r3", "name": "gql", "method": "QUERY", "url": "https://a.test",
					"body": map[string]any{"type": "graphql", "graphql": map[string]any{"query": x, "variables": x}}},
				map[string]any{"id": "r4", "name": x, "kind": x, "url": x},
			},
		},
		"environment": map[string]any{"name": x, "variables": []any{map[string]any{"key": x, "value": x, "enabled": true, "secret": false}}},
	}
	res, text := sharePage(t, h, createShare(t, h, ws.Token, snapshot, "")["shareId"].(string))
	if res.StatusCode != 200 {
		t.Fatal(res.StatusCode)
	}
	if strings.Contains(text, "<script>alert") || strings.Contains(text, "<img") {
		t.Fatal("raw markup rendered")
	}
	// No injected attribute: drop quoted attribute values (user text inside them is escaped), then scan tags.
	if regexp.MustCompile(`(?i)<[^>]*\sonerror=`).MatchString(regexp.MustCompile(`"[^"]*"`).ReplaceAllString(text, `""`)) {
		t.Fatal("injected attribute")
	}
	if regexp.MustCompile(`(?i)href="javascript:`).MatchString(text) {
		t.Fatal("javascript: href")
	}
	mustContain(t, text, "&lt;script&gt;alert(1)&lt;/script&gt;&#34;&gt;&lt;img src=x onerror=alert(2)&gt;&#39;", "<title>&lt;script&gt;",
		`title="&lt;script&gt;`) // the custom-method tooltip attribute is escaped too
	// The only <script> element is the static, hash-allowed one; all hrefs are internal anchors.
	if strings.Count(text, "<script") != 1 {
		t.Fatalf("script elements: %d", strings.Count(text, "<script"))
	}
	for _, m := range regexp.MustCompile(`href="([^"]*)"`).FindAllStringSubmatch(text, -1) {
		if !regexp.MustCompile(`^#r\d+$`).MatchString(m[1]) {
			t.Fatalf("href %q", m[1])
		}
	}
}

func TestSharePage_XSS_MalformedSnapshotShapesNeverBreakRendering(t *testing.T) {
	h := tu.New(t)
	ws := h.NewWorkspace(t, "")
	snapshot := map[string]any{
		"schemaVersion": 1,
		"collection": map[string]any{
			"name":     42,
			"folders":  []any{map[string]any{"id": "a", "parentFolderId": "b"}, map[string]any{"id": "b", "parentFolderId": "a"}, "junk"},
			"requests": []any{nil, map[string]any{"headers": "x", "params": []any{1, nil}, "body": map[string]any{"type": "nope"}, "auth": 5}},
		},
	}
	res, text := sharePage(t, h, createShare(t, h, ws.Token, snapshot, "")["shareId"].(string))
	if res.StatusCode != 200 {
		t.Fatal(res.StatusCode)
	}
	mustContain(t, text, "Untitled request", "<h1>42</h1>")
}

func TestSharePage_AC_SHARE_4_UpdateShowsTheNewSnapshot(t *testing.T) {
	h := tu.New(t)
	ws := h.NewWorkspace(t, "")
	id := createShare(t, h, ws.Token, nil, "")["shareId"].(string)
	next := tu.SampleCollectionSnapshot()
	next["collection"].(map[string]any)["name"] = "Users API v2"
	if r := h.Call(t, "/v1/shares/"+id, tu.Req{Method: "PUT", Token: ws.Token, JSON: map[string]any{"snapshot": next}}); r.StatusCode != 200 {
		t.Fatal(r.StatusCode)
	}
	_, text := sharePage(t, h, id)
	mustContain(t, text, "<h1>Users API v2</h1>")
}

func TestSharePage_AC_FWD_6_GRPCAndWebSocketRequestsGetTheImportCard(t *testing.T) {
	h := tu.New(t)
	ws := h.NewWorkspace(t, "")
	snap := tu.SampleCollectionSnapshot()
	c := snap["collection"].(map[string]any)
	c["requests"] = append(c["requests"].([]any),
		tu.SampleRequest(map[string]any{"id": "G1", "name": "Stream orders", "kind": "grpc", "url": "grpc://orders:50051", "sortOrder": 9}),
		tu.SampleRequest(map[string]any{"id": "W1", "name": "Live feed", "kind": "websocket", "url": "wss://feed.example/{{room}}", "sortOrder": 10}),
	)
	_, text := sharePage(t, h, createShare(t, h, ws.Token, snap, "")["shareId"].(string))
	card := "This request type can't be previewed on the web. Import it into ReqHarbor."
	if strings.Count(text, card) != 2 {
		t.Fatalf("cards: %d", strings.Count(text, card))
	}
	// The three HTTP requests keep their Copy button and cURL block; the other two have neither.
	if strings.Count(text, "data-copy=") != 3 || strings.Count(text, `<details class="curl">`) != 3 {
		t.Fatalf("copy %d curl %d", strings.Count(text, "data-copy="), strings.Count(text, `<details class="curl">`))
	}
	// Custom badges, in the nav and the request bar; long labels are cut to 7 characters.
	for badge, n := range map[string]int{
		`<span class="m m-custom" title="grpc">grpc</span>`:                    2,
		`<span class="m m-custom" title="websocket">websock` + "\u2026</span>": 2,
	} {
		if strings.Count(text, badge) != n {
			t.Errorf("badge %s: %d", badge, strings.Count(text, badge))
		}
	}
	// Five requests, of which only the sample GET is a GET.
	if strings.Count(text, `<span class="m m-get">GET</span>`) != 2 {
		t.Errorf("GET badges: %d", strings.Count(text, `<span class="m m-get">GET</span>`))
	}
	mustContain(t, text, "· 5 requests ·")
	// The JSON view keeps the kind for Import from link.
	j := h.Call(t, "/s/"+regexp.MustCompile(`[A-Za-z0-9]{22}`).FindString(h.Call(t, "/v1/shares", tu.Req{Token: ws.Token}).JSON(t)["shares"].([]any)[0].(map[string]any)["shareId"].(string))+".json", tu.Req{})
	if !strings.Contains(string(j.Body), `"kind":"grpc"`) {
		t.Fatal("kind lost in JSON")
	}
}

// W11 (06-go-server.md §1.3): a share whose environment nests references N^3 deep
// (a={{b}}x2000, b={{c}}x2000, c={{d}}x2000: about 30 KB, 8 GB per reference unbudgeted) is
// served at once, with the "Can't export as cURL" hint, and the server keeps answering.
func TestSharePage_W11_NestedVariableBombIsBudgeted(t *testing.T) {
	h := tu.New(t)
	ws := h.NewWorkspace(t, "")
	v := func(k, val string) map[string]any {
		return map[string]any{"key": k, "value": val, "enabled": true, "secret": false}
	}
	hdr := func(k string) map[string]any { return map[string]any{"key": k, "value": "{{a}}", "enabled": true} }
	snap := map[string]any{
		"schemaVersion": 1,
		"request": tu.SampleRequest(map[string]any{"name": "Bomb", "url": "https://h/{{a}}", "params": []any{},
			"headers": []any{hdr("X-1"), hdr("X-2"), hdr("X-3"), hdr("X-4")}}),
		"environment": map[string]any{"name": "Bomb", "variables": []any{
			v("a", strings.Repeat("{{b}}", 2000)), v("b", strings.Repeat("{{c}}", 2000)), v("c", strings.Repeat("{{d}}", 2000)), v("d", "x"),
		}},
	}
	id := createShare(t, h, ws.Token, snap, "request")["shareId"].(string)
	start := time.Now()
	for i := 0; i < 3; i++ { // nothing is cached: every view renders again
		res, text := sharePage(t, h, id)
		if res.StatusCode != 200 {
			t.Fatal(res.StatusCode)
		}
		mustContain(t, text, "Can't export as cURL: Resolved request is too large to export")
		if strings.Contains(text, "data-copy=") {
			t.Error("copy button on an unexportable request")
		}
	}
	if d := time.Since(start); d > 5*time.Second {
		t.Fatalf("three views took %v", d)
	}
	// The rest of the server is unaffected, and the JSON for import is the snapshot as shared.
	if res := h.Call(t, "/s/"+id+".json", tu.Req{}); res.StatusCode != 200 || len(res.Body) < 3*5*2000 {
		t.Fatalf("%d %d", res.StatusCode, len(res.Body))
	}
	if res := h.Call(t, "/v1/workspace", tu.Req{Token: ws.Token}); res.StatusCode != 200 {
		t.Fatal(res.StatusCode)
	}
}
