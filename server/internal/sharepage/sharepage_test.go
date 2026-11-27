package sharepage

import (
	"encoding/json"
	"reflect"
	"regexp"
	"strings"
	"testing"
	"time"

	"github.com/mihajlopopara/reqharbor/server/internal/jsonx"
	"github.com/mihajlopopara/reqharbor/server/internal/web"
)

func parse(t testing.TB, v any) *jsonx.Value {
	t.Helper()
	b, err := json.Marshal(v)
	if err != nil {
		t.Fatal(err)
	}
	p, err := jsonx.Parse(b)
	if err != nil {
		t.Fatal(err)
	}
	return p
}

func render(t testing.TB, kind string, snap any) string {
	t.Helper()
	out, err := Render(NormalizeSnapshot(kind, parse(t, snap)), Meta{ShareID: "s", CreatedAt: 0, UpdatedAt: 1764237600000})
	if err != nil {
		t.Fatalf("Render: %v", err)
	}
	return string(out)
}

func TestWithVars_SplitsVariableReferences(t *testing.T) {
	got := WithVars("{{a}}/x?{{ b.c }}&{{bad name}}{{\u00a0d\u3000}}")
	want := []Segment{{"{{a}}", true}, {"/x?", false}, {"{{ b.c }}", true}, {"&{{bad name}}", false}, {"{{\u00a0d\u3000}}", true}}
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("%#v", got)
	}
	if WithVars("") != nil {
		t.Fatal("empty text has no segments")
	}
}

func TestPrettyJSON_LikeJSONStringifyIndent2(t *testing.T) {
	for in, want := range map[string]string{
		`{"q":"ada"}`: "{\n  \"q\": \"ada\"\n}",
		`{"b":1,"2":[],"1":{},"a":[1.50,true,null,"<"]}`: "{\n  \"1\": {},\n  \"2\": [],\n  \"b\": 1,\n  \"a\": [\n    1.5,\n    true,\n    null,\n    \"<\"\n  ]\n}",
		`1e400`:    "null",
		`not json`: "not json",
		` "s\n" `:  `"s\n"`,
	} {
		if got := PrettyJSON(in); got != want {
			t.Errorf("PrettyJSON(%q) = %q, want %q", in, got, want)
		}
	}
}

func TestNormalize_MalformedShapesFallBack(t *testing.T) {
	s := NormalizeSnapshot("collection", parse(t, map[string]any{
		"schemaVersion": 1,
		"collection": map[string]any{
			"name":     42,
			"folders":  []any{map[string]any{"id": "a", "parentFolderId": "b"}, "junk"},
			"requests": []any{nil, map[string]any{"headers": "x", "params": []any{1, nil}, "body": map[string]any{"type": "nope"}, "auth": 5, "sortOrder": json.RawMessage("1e400")}},
		},
	}))
	if s.Name != "42" || len(s.Folders) != 2 || s.Folders[1].ID != "folder-1" || s.Folders[1].Name != "Untitled folder" {
		t.Fatalf("%+v", s)
	}
	r := s.Requests[1]
	if r.Name != "Untitled request" || r.Method != "GET" || r.Body.Type != "none" || r.Auth.Type != "none" || len(r.Params) != 2 ||
		r.Params[0].Key != "" || !r.Params[0].Enabled || r.SortOrder != 1 || !r.Settings.VerifyTLS || r.Settings.TimeoutMs != 30000 {
		t.Fatalf("%+v", r)
	}
}

func TestRender_AC_FWD_6_UnknownRequestKindsGetTheImportCard(t *testing.T) {
	const card = "This request type can't be previewed on the web. Import it into ReqHarbor."
	for _, kind := range []any{"grpc", "websocket", "socketio", "graphql", 7, map[string]any{"x": 1}} {
		req := map[string]any{"name": "Stream", "kind": kind, "url": "wss://rt.example.com/{{room}}", "method": "GET",
			"headers": []any{map[string]any{"key": "X-Hidden", "value": "1", "enabled": true}}}
		// Request share.
		page := render(t, "request", map[string]any{"schemaVersion": 1, "request": req})
		if !strings.Contains(page, card) || strings.Contains(page, "data-copy=") || strings.Contains(page, `class="curl"`) ||
			strings.Contains(page, `class="m m-get"`) || strings.Contains(page, "X-Hidden") || strings.Contains(page, "This request has no body.") {
			t.Fatalf("kind %v:\n%s", kind, page)
		}
		if !strings.Contains(page, `<span class="var">{{room}}</span>`) {
			t.Fatalf("kind %v: the URL is still shown:\n%s", kind, page)
		}
		// Collection share: the card sits next to normal HTTP requests, which keep their Copy button.
		page = render(t, "collection", map[string]any{"schemaVersion": 1, "collection": map[string]any{
			"name": "Mixed", "requests": []any{req, map[string]any{"name": "Plain", "kind": "http", "url": "https://a.test"}, map[string]any{"name": "Old", "url": "https://b.test"}},
		}})
		if strings.Count(page, card) != 1 || strings.Count(page, "data-copy=") != 2 || strings.Count(page, `<span class="m m-get">GET</span>`) != 4 {
			t.Fatalf("kind %v in a collection:\n%s", kind, page)
		}
	}
	// The nav and the request bar show the kind as a custom badge, never as a GET.
	page := render(t, "collection", map[string]any{"schemaVersion": 1, "collection": map[string]any{"requests": []any{map[string]any{"kind": "grpc"}}}})
	if strings.Count(page, `<span class="m m-custom" title="grpc">grpc</span>`) != 2 || strings.Contains(page, `class="m m-get"`) {
		t.Fatal(page)
	}
	// kind null counts as absent: HTTP.
	page = render(t, "request", map[string]any{"schemaVersion": 1, "request": map[string]any{"kind": nil, "url": "https://a.test"}})
	if strings.Contains(page, card) || !strings.Contains(page, "data-copy=") {
		t.Fatal(page)
	}
}

func TestRender_InlineAssetsAppearExactlyOnce(t *testing.T) {
	page := render(t, "request", map[string]any{"schemaVersion": 1, "request": map[string]any{"name": "x"}})
	if strings.Count(page, "<style>"+web.ShareCSS+"</style>") != 1 || strings.Count(page, "<script>"+web.ShareJS+"</script>") != 1 ||
		strings.Count(page, "<script") != 1 || strings.Count(page, "<style") != 1 {
		t.Fatal("inline assets must appear exactly once")
	}
	for _, status := range []int{404, 410} {
		b, err := RenderError(status)
		if err != nil || strings.Count(string(b), "<style>"+web.ShareCSS+"</style>") != 1 || strings.Contains(string(b), "<script") {
			t.Fatalf("%d: %v", status, err)
		}
	}
}

func TestRender_NotesHintsAndAuthBlocks(t *testing.T) {
	page := render(t, "collection", map[string]any{"schemaVersion": 1, "collection": map[string]any{
		"name": "C", "auth": map[string]any{"type": "basic", "username": "u"},
		"requests": []any{
			map[string]any{"name": "q", "method": "QUERY", "url": "https://a.test", "auth": map[string]any{"type": "inherit"},
				"settings": map[string]any{"followRedirects": false, "timeoutMs": 1500.5, "verifyTls": false}},
			map[string]any{"name": "k", "url": "https://a.test", "auth": map[string]any{"type": "apikey", "apiKey": map[string]any{"key": "K", "value": "{{v}}", "in": "query"}},
				"body": map[string]any{"type": "form", "form": []any{map[string]any{"key": "f", "value": "/p", "kind": "file", "enabled": false}}}},
			map[string]any{"name": "e", "url": "https://a.test", "body": map[string]any{"type": "formEncoded", "formEncoded": []any{}}},
			map[string]any{"name": "g", "method": "POST", "url": "https://a.test", "body": map[string]any{"type": "graphql", "graphql": map[string]any{"query": "{ me }", "variables": map[string]any{"a": 1}}}},
		},
	}})
	for _, want := range []string{
		`<span class="meta">Redirects not followed · Timeout 1500.5 ms · TLS verification off</span>`,
		`<p class="hint">QUERY requests normally carry a body</p>`,
		`<p class="meta">Inherits basic from the collection</p>`,
		`<p class="meta">API key in query</p><div class="field"><span class="label">Key</span><code>K</code></div><div class="field"><span class="label">Value</span><code><span class="var">{{v}}</span></code></div>`,
		`<tr class="off"><td>f</td><td>/p</td><td class="ui">File</td></tr>`,
		`<caption>Form <span class="cnt">0</span></caption>`,
		`<div class="sublabel">GraphQL Variables</div><pre class="code">{
  &#34;a&#34;: 1
}</pre>`,
		`Read-only shared collection · 4 requests · Updated 2025-11-27 10:00 UTC`,
	} {
		if !strings.Contains(page, want) {
			t.Errorf("page lacks %q", want)
		}
	}
	// The empty form-encoded body shows the no-body text.
	if strings.Count(page, "This request has no body.") != 2 {
		t.Errorf("no-body count: %d", strings.Count(page, "This request has no body."))
	}
}

func TestRender_TreeOrderCyclesAndDepth(t *testing.T) {
	folders := []any{
		map[string]any{"id": "a", "name": "A", "parentFolderId": "b", "sortOrder": 0},
		map[string]any{"id": "b", "name": "B", "parentFolderId": "a", "sortOrder": 0},
		map[string]any{"id": "self", "name": "Self", "parentFolderId": "self", "sortOrder": 5},
		map[string]any{"id": "z", "name": "zeta", "sortOrder": 1},
		map[string]any{"id": "y", "name": "Alpha", "sortOrder": 1},
	}
	page := render(t, "collection", map[string]any{"schemaVersion": 1, "collection": map[string]any{"name": "T", "folders": folders,
		"requests": []any{map[string]any{"name": "in-cycle", "folderId": "a"}, map[string]any{"name": "in-self", "folderId": "self"}}}})
	order := regexp.MustCompile(`<span class="trow folder depth-0"><span class="name">([^<]*)</span>`).FindAllStringSubmatch(page, -1)
	var names []string
	for _, m := range order {
		names = append(names, m[1])
	}
	// a and b only reach each other, so they never show; a self-parented folder sits at the root.
	if !reflect.DeepEqual(names, []string{"Alpha", "zeta", "Self"}) || strings.Contains(page, "in-cycle") || !strings.Contains(page, "T <span aria-hidden=\"true\">›</span> Self") {
		t.Fatalf("%v\n%s", names, page)
	}
}

func FuzzSharePage(f *testing.F) {
	seeds := []string{
		`{"schemaVersion":1,"collection":{"name":"<script>x</script>","folders":[{"id":"a","parentFolderId":"a"}],"requests":[{"method":"<svg/onload=1>","url":"javascript:1","body":{"type":"graphql","graphql":{"query":"q","variables":{"</script>":1}}}}]},"environment":{"variables":[{"key":"k","value":"{{k}}"}]}}`,
		`{"schemaVersion":1,"request":{"kind":"grpc","name":"</style><script>"}}`,
		`{"collection":{"requests":[null,1,"x",{"body":{"type":"json","text":"{\"a\":\"</pre><script>\"}"}}]}}`,
		`[]`, `null`, `{"request":{"params":[{"key":{"x":1}}],"headers":[[]],"auth":{"type":"basic","username":5e400}}}`,
	}
	for _, s := range seeds {
		f.Add(s, true)
		f.Add(s, false)
	}
	f.Fuzz(func(t *testing.T, raw string, collection bool) {
		v, err := jsonx.Parse([]byte(raw))
		if err != nil {
			return
		}
		kind := "request"
		if collection {
			kind = "collection"
		}
		out, err := Render(NormalizeSnapshot(kind, v), Meta{UpdatedAt: 1})
		if err != nil {
			t.Fatalf("Render: %v", err)
		}
		lower := strings.ToLower(string(out))
		if strings.Count(lower, "<script") != 1 || !strings.Contains(string(out), "<script>"+web.ShareJS+"</script>") {
			t.Fatalf("unexpected script element in\n%s", out)
		}
		if strings.Count(lower, "<style") != 1 {
			t.Fatalf("unexpected style element")
		}
	})
}

// W11: the QA reproduction (a={{b}}x2000, b={{c}}x2000, c={{d}}x2000) in the URL and several
// headers of many requests used to expand to gigabytes per view; it now renders at once, with the
// "Can't export as cURL" hint on each affected request.
func cubicEnv(n int) map[string]any {
	v := func(k, val string) map[string]any { return map[string]any{"key": k, "value": val, "enabled": true} }
	return map[string]any{"name": "Bomb", "variables": []any{
		v("a", strings.Repeat("{{b}}", n)), v("b", strings.Repeat("{{c}}", n)), v("c", strings.Repeat("{{d}}", n)), v("d", "x"),
	}}
}

const tooLarge = "Can't export as cURL: Resolved request is too large to export"

func TestRender_W11_NestedVariableBombRendersQuicklyWithTheHint(t *testing.T) {
	h := func(k string) map[string]any { return map[string]any{"key": k, "value": "{{a}}", "enabled": true} }
	var reqs []any
	for i := 0; i < 20; i++ {
		reqs = append(reqs, map[string]any{"name": "Bomb", "url": "https://h/{{a}}", "headers": []any{h("X-1"), h("X-2"), h("X-3")}})
	}
	// Rendered first: once the bombs have used up the page budget, later requests get the hint too.
	reqs = append(reqs, map[string]any{"name": "Fine", "url": "https://fine.test/{{d}}", "sortOrder": -1})
	start := time.Now()
	page := render(t, "collection", map[string]any{"schemaVersion": 1, "collection": map[string]any{"name": "C", "requests": reqs}, "environment": cubicEnv(2000)})
	if d := time.Since(start); d > 3*time.Second {
		t.Fatalf("render took %v", d)
	}
	if n := strings.Count(page, tooLarge); n != 20 {
		t.Fatalf("%d hints", n)
	}
	// The request that needs little expansion still exports, raw and resolved.
	if strings.Count(page, "data-copy=") != 1 || !strings.Contains(page, "https://fine.test/x") || len(page) > 4<<20 {
		t.Fatalf("%d bytes", len(page))
	}
}

func TestRender_W11_PageBudgetIsSharedByAllRequests(t *testing.T) {
	big := strings.Repeat("v", 300_000)
	var reqs []any
	for i := 0; i < 10; i++ {
		reqs = append(reqs, map[string]any{"name": "R", "url": "https://h/{{big}}", "sortOrder": i})
	}
	page := render(t, "collection", map[string]any{"schemaVersion": 1, "collection": map[string]any{"name": "C", "requests": reqs},
		"environment": map[string]any{"variables": []any{map[string]any{"key": "big", "value": big, "enabled": true}}}})
	// 2 MiB covers six 300 KB substitutions; the other four requests get the hint.
	if c, n := strings.Count(page, "data-copy="), strings.Count(page, tooLarge); c != 6 || n != 4 {
		t.Fatalf("%d exported, %d hints", c, n)
	}
}
