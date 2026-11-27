package app_test

// Port of the API cases of worker/test/shares.test.ts (06-go-server.md §1.6). The share-page
// rendering cases (AC-SHARE-3, AC-QRY-1, the XSS cases) are in share_page_test.go.

import (
	"fmt"
	"regexp"
	"strings"
	"testing"

	tu "github.com/mihajlopopara/reqharbor/server/internal/testutil"
)

func createShare(t *testing.T, h *tu.Harness, token string, snapshot any, kind string) map[string]any {
	t.Helper()
	if snapshot == nil {
		snapshot = tu.SampleCollectionSnapshot()
	}
	if kind == "" {
		kind = "collection"
	}
	res := h.Call(t, "/v1/shares", tu.Req{Token: token, JSON: map[string]any{"kind": kind, "entityId": "01COL0000000000000000000001", "snapshot": snapshot}})
	if res.StatusCode != 201 {
		t.Fatalf("create share: %d %s", res.StatusCode, res.Body)
	}
	return res.JSON(t)
}

// manyShares lifts the share-create limit for the quota cases, which create ~60 shares.
var manyShares = tu.WithEnv("REQHARBOR_RATE_SHARE_CREATE_PER_HOUR=1000")

func TestShares_AC_SHARE_1_UnguessableIDAndURL(t *testing.T) {
	h := tu.New(t)
	ws := h.NewWorkspace(t, "")
	a := createShare(t, h, ws.Token, nil, "")
	b := createShare(t, h, ws.Token, nil, "")
	if !regexp.MustCompile(`^[0-9A-Za-z]{22}$`).MatchString(a["shareId"].(string)) || a["shareId"] == b["shareId"] {
		t.Fatalf("%v %v", a, b)
	}
	if a["url"] != "https://reqharbor.test/s/"+a["shareId"].(string) {
		t.Fatalf("url %v", a["url"])
	}
	if num(a["createdAt"]) != h.Clock.Now().UnixMilli() || a["createdAt"] != a["updatedAt"] {
		t.Fatalf("%v", a)
	}
}

func TestShares_AC_SHARE_1_SharingRequiresAToken(t *testing.T) {
	h := tu.New(t)
	res := h.Call(t, "/v1/shares", tu.Req{JSON: map[string]any{"kind": "request", "entityId": "x", "snapshot": map[string]any{"schemaVersion": 1, "request": map[string]any{}}}})
	if res.StatusCode != 401 {
		t.Fatalf("got %d", res.StatusCode)
	}
}

func TestShares_AC_SHARE_1_SnapshotIsValidated(t *testing.T) {
	h := tu.New(t)
	ws := h.NewWorkspace(t, "")
	bad := []map[string]any{
		{"kind": "folder", "entityId": "x", "snapshot": map[string]any{"schemaVersion": 1, "collection": map[string]any{}}},
		{"kind": "request", "entityId": "", "snapshot": map[string]any{"schemaVersion": 1, "request": map[string]any{}}},
		{"kind": "request", "entityId": "x", "snapshot": map[string]any{"request": map[string]any{}}},
		{"kind": "request", "entityId": "x", "snapshot": map[string]any{"schemaVersion": 1, "collection": map[string]any{}}},
		{"kind": "collection", "entityId": "x", "snapshot": map[string]any{"schemaVersion": 1, "collection": map[string]any{"requests": "no"}}},
		{"kind": "collection", "entityId": "x", "snapshot": map[string]any{"schemaVersion": 1, "collection": map[string]any{"folders": 1}}},
		{"kind": "collection", "entityId": "x", "snapshot": map[string]any{"schemaVersion": 1, "collection": map[string]any{}, "environment": []any{}}},
		{"kind": "collection", "entityId": "x", "snapshot": "nope"},
		{"kind": "collection", "entityId": "x", "snapshot": map[string]any{"schemaVersion": 0, "collection": map[string]any{}}},
		{"kind": "collection", "entityId": "x", "snapshot": map[string]any{"schemaVersion": 1.5, "collection": map[string]any{}}},
	}
	for _, body := range bad {
		res := h.Call(t, "/v1/shares", tu.Req{Token: ws.Token, JSON: body})
		if res.StatusCode != 400 || res.ErrorCode(t) != "validation_failed" {
			t.Fatalf("%v: got %d %s", body, res.StatusCode, res.Body)
		}
	}
}

func TestShares_SnapshotWithUnreadableValueIs400(t *testing.T) {
	h := tu.New(t)
	ws := h.NewWorkspace(t, "")
	sh := createShare(t, h, ws.Token, nil, "")
	deep := strings.Repeat("[", 100) + strings.Repeat("]", 100) // snapshot level 1 + 100 = 101
	for _, snap := range []string{
		`{"schemaVersion":1,"request":{"n":1e400}}`,
		`{"schemaVersion":1,"request":{"s":"\uDBFF"}}`,
		`{"schemaVersion":1,"request":{"x":` + deep + `}}`,
	} {
		for _, call := range []struct{ method, path, body string }{
			{"POST", "/v1/shares", `{"kind":"request","entityId":"x","snapshot":` + snap + `}`},
			{"PUT", "/v1/shares/" + sh["shareId"].(string), `{"snapshot":` + strings.Replace(snap, `"request"`, `"collection"`, 1) + `}`},
		} {
			body := call.body
			res := h.Call(t, call.path, tu.Req{Method: call.method, Token: ws.Token, Raw: &body, Headers: map[string]string{"Content-Type": "application/json"}})
			if res.StatusCode != 400 || res.ErrorCode(t) != "validation_failed" || !strings.Contains(string(res.Body), "Snapshot contains a value ReqHarbor cannot read") {
				t.Fatalf("%s %s: %d %s", call.method, snap, res.StatusCode, res.Body)
			}
		}
	}
}

func TestShares_AC_SHARE_2_AC_ENV_7_SecretVariablesAreRemoved(t *testing.T) {
	h := tu.New(t)
	ws := h.NewWorkspace(t, "")
	sh := createShare(t, h, ws.Token, nil, "")
	id := sh["shareId"].(string)
	j := h.Call(t, "/s/"+id+".json", tu.Req{}).JSON(t)
	tu.JSONEq(t, j["snapshot"].(map[string]any)["environment"].(map[string]any)["variables"],
		`[{"key":"baseUrl","value":"https://staging.example.com","enabled":true,"secret":false}]`)
	var stored string
	if err := h.DB.Reader.QueryRow("SELECT snapshot FROM shares WHERE id = ?", id).Scan(&stored); err != nil {
		t.Fatal(err)
	}
	if strings.Contains(stored, "super-secret-token") {
		t.Fatal("secret stored")
	}
	if page := h.Call(t, "/s/"+id, tu.Req{}); strings.Contains(string(page.Body), "super-secret-token") {
		t.Fatal("secret on the page")
	}
}

func TestShares_AC_SHARE_4_UpdateReplacesTheSnapshotBehindTheSameURL(t *testing.T) {
	h := tu.New(t)
	ws := h.NewWorkspace(t, "")
	created := createShare(t, h, ws.Token, nil, "")
	id := created["shareId"].(string)
	next := tu.SampleCollectionSnapshot()
	next["collection"].(map[string]any)["name"] = "Users API v2"
	h.Clock.Advance(1500_000_000)
	upd := h.Call(t, "/v1/shares/"+id, tu.Req{Method: "PUT", Token: ws.Token, JSON: map[string]any{"snapshot": next}})
	if upd.StatusCode != 200 {
		t.Fatalf("got %d %s", upd.StatusCode, upd.Body)
	}
	u := upd.JSON(t)
	if u["url"] != created["url"] || num(u["updatedAt"]) <= num(u["createdAt"]) || u["createdAt"] != created["createdAt"] {
		t.Fatalf("%v", u)
	}
	j := h.Call(t, "/s/"+id+".json", tu.Req{}).JSON(t)
	if j["snapshot"].(map[string]any)["collection"].(map[string]any)["name"] != "Users API v2" || j["updatedAt"] != u["updatedAt"] {
		t.Fatalf("%v", j)
	}
	if page := h.Call(t, "/s/"+id, tu.Req{}); !strings.Contains(string(page.Body), "<h1>Users API v2</h1>") {
		t.Fatalf("%s", page.Body)
	}
	// updated_at never goes backwards, even if the clock does.
	h.Clock.Advance(-10_000_000_000)
	again := h.Call(t, "/v1/shares/"+id, tu.Req{Method: "PUT", Token: ws.Token, JSON: map[string]any{"snapshot": next}}).JSON(t)
	if num(again["updatedAt"]) != num(u["updatedAt"]) {
		t.Fatalf("updatedAt went back: %v", again)
	}
}

func TestShares_AC_SHARE_4_RevokeMakesTheURLGoneAndDiscardsTheSnapshot(t *testing.T) {
	h := tu.New(t)
	ws := h.NewWorkspace(t, "")
	id := createShare(t, h, ws.Token, nil, "")["shareId"].(string)
	del := h.Call(t, "/v1/shares/"+id, tu.Req{Method: "DELETE", Token: ws.Token})
	if del.StatusCode != 200 {
		t.Fatalf("got %d", del.StatusCode)
	}
	tu.JSONEq(t, del.Body, map[string]any{"shareId": id, "revokedAt": h.Clock.Now().UnixMilli()})

	page := h.Call(t, "/s/"+id, tu.Req{})
	if page.StatusCode != 410 || !strings.Contains(string(page.Body), "This link has been revoked") || strings.Contains(string(page.Body), "Users API") {
		t.Fatalf("page %d %s", page.StatusCode, page.Body)
	}
	if page.Header.Get("Cache-Control") != "no-store" {
		t.Fatalf("cache %q", page.Header.Get("Cache-Control"))
	}
	j := h.Call(t, "/s/"+id+".json", tu.Req{})
	if j.StatusCode != 410 || j.ErrorCode(t) != "gone" {
		t.Fatalf("json %d", j.StatusCode)
	}
	var snap any
	if err := h.DB.Reader.QueryRow("SELECT snapshot FROM shares WHERE id = ?", id).Scan(&snap); err != nil || snap != nil {
		t.Fatalf("snapshot %v %v", snap, err)
	}
	if upd := h.Call(t, "/v1/shares/"+id, tu.Req{Method: "PUT", Token: ws.Token, JSON: map[string]any{"snapshot": tu.SampleCollectionSnapshot()}}); upd.StatusCode != 410 {
		t.Fatalf("update revoked: %d", upd.StatusCode)
	}
	h.Clock.Advance(5_000_000_000)
	again := h.Call(t, "/v1/shares/"+id, tu.Req{Method: "DELETE", Token: ws.Token})
	if again.StatusCode != 200 {
		t.Fatalf("revoke again: %d", again.StatusCode)
	}
	if again.JSON(t)["revokedAt"] != del.JSON(t)["revokedAt"] {
		t.Fatal("revokedAt changed on the second revoke")
	}
}

func TestShares_AC_SHARE_4_OtherWorkspacesCannotUpdateOrRevoke(t *testing.T) {
	h := tu.New(t)
	owner := h.NewWorkspace(t, "")
	other := h.NewWorkspace(t, "")
	id := createShare(t, h, owner.Token, nil, "")["shareId"].(string)
	if r := h.Call(t, "/v1/shares/"+id, tu.Req{Method: "DELETE", Token: other.Token}); r.StatusCode != 404 {
		t.Fatalf("delete %d", r.StatusCode)
	}
	if r := h.Call(t, "/v1/shares/"+id, tu.Req{Method: "PUT", Token: other.Token, JSON: map[string]any{"snapshot": tu.SampleCollectionSnapshot()}}); r.StatusCode != 404 {
		t.Fatalf("put %d", r.StatusCode)
	}
	if r := h.Call(t, "/s/"+id, tu.Req{}); r.StatusCode != 200 {
		t.Fatalf("page %d", r.StatusCode)
	}
}

func TestShares_AC_SHARE_5_JSONForImportFromLinkIsCORSReadable(t *testing.T) {
	h := tu.New(t)
	ws := h.NewWorkspace(t, "")
	id := createShare(t, h, ws.Token, nil, "")["shareId"].(string)
	res := h.Call(t, "/s/"+id+".json", tu.Req{Headers: map[string]string{"Origin": "tauri://localhost"}})
	if res.StatusCode != 200 || res.Header.Get("Access-Control-Allow-Origin") != "*" || res.Header.Get("Cross-Origin-Resource-Policy") != "cross-origin" {
		t.Fatalf("got %d %v", res.StatusCode, res.Header)
	}
	v := res.JSON(t)
	if len(v) != 5 || v["shareId"] != id || v["kind"] != "collection" || v["snapshot"].(map[string]any)["schemaVersion"] != float64(1) {
		t.Fatalf("%v", v)
	}
	if n := len(v["snapshot"].(map[string]any)["collection"].(map[string]any)["requests"].([]any)); n != 3 {
		t.Fatalf("%d requests", n)
	}
	pre := h.Call(t, "/s/"+id+".json", tu.Req{Method: "OPTIONS"})
	if pre.StatusCode != 204 || pre.Header.Get("Access-Control-Allow-Origin") != "*" ||
		pre.Header.Get("Access-Control-Allow-Methods") != "GET, OPTIONS" || pre.Header.Get("Access-Control-Max-Age") != "86400" {
		t.Fatalf("preflight %d %v", pre.StatusCode, pre.Header)
	}
	// The HTML page allows only GET.
	if r := h.Call(t, "/s/"+id, tu.Req{Method: "OPTIONS"}); r.StatusCode != 405 || r.Header.Get("Allow") != "GET" {
		t.Fatalf("page OPTIONS %d", r.StatusCode)
	}
	if r := h.Call(t, "/s/"+id+".json", tu.Req{Method: "POST"}); r.StatusCode != 405 || r.Header.Get("Allow") != "GET, OPTIONS" {
		t.Fatalf("json POST %d %q", r.StatusCode, r.Header.Get("Allow"))
	}
}

func TestShares_AC_SHARE_6_ListActiveShares(t *testing.T) {
	h := tu.New(t)
	ws := h.NewWorkspace(t, "")
	a := createShare(t, h, ws.Token, nil, "")
	h.Clock.Advance(1_000_000)
	b := createShare(t, h, ws.Token, map[string]any{"schemaVersion": 1, "request": tu.SampleRequest(map[string]any{"name": "Solo"})}, "request")
	h.Clock.Advance(1_000_000)
	c := createShare(t, h, ws.Token, nil, "")
	h.Call(t, "/v1/shares/"+c["shareId"].(string), tu.Req{Method: "DELETE", Token: ws.Token})
	h.Clock.Advance(1_000_000)
	nonString := createShare(t, h, ws.Token, map[string]any{"schemaVersion": 1, "collection": map[string]any{"name": 42}}, "")
	res := h.Call(t, "/v1/shares", tu.Req{Token: ws.Token})
	if res.StatusCode != 200 {
		t.Fatalf("got %d", res.StatusCode)
	}
	list := res.JSON(t)["shares"].([]any)
	var order []string
	for _, s := range list {
		order = append(order, s.(map[string]any)["shareId"].(string))
	}
	want := []string{nonString["shareId"].(string), b["shareId"].(string), a["shareId"].(string)}
	if fmt.Sprint(order) != fmt.Sprint(want) {
		t.Fatalf("order %v, want %v", order, want)
	}
	tu.JSONEq(t, list[1], map[string]any{
		"shareId": b["shareId"], "url": b["url"], "kind": "request", "entityId": "01COL0000000000000000000001",
		"name": "Solo", "createdAt": b["createdAt"], "updatedAt": b["updatedAt"],
	})
	if list[2].(map[string]any)["name"] != "Users API" || list[0].(map[string]any)["name"] != nil {
		t.Fatalf("%v", list)
	}
	other := h.NewWorkspace(t, "")
	tu.JSONEq(t, h.Call(t, "/v1/shares", tu.Req{Token: other.Token}).Body, `{"shares":[]}`)
}

func TestShares_QA_R1_024_SnapshotsCountTowardTheQuota(t *testing.T) {
	h := tu.New(t, manyShares)
	ws := h.NewWorkspace(t, "")
	small := createShare(t, h, ws.Token, nil, "")
	big := tu.SampleCollectionSnapshot()
	big["collection"].(map[string]any)["description"] = strings.Repeat("x", 1000*1024)
	var bigIDs []string
	var last *tu.Resp
	for i := 0; i < 60; i++ {
		last = h.Call(t, "/v1/shares", tu.Req{Token: ws.Token, JSON: map[string]any{"kind": "collection", "entityId": fmt.Sprintf("C%d", i), "snapshot": big}})
		if last.StatusCode != 201 {
			break
		}
		bigIDs = append(bigIDs, last.JSON(t)["shareId"].(string))
	}
	if last.StatusCode != 413 || last.ErrorCode(t) != "quota_exceeded" {
		t.Fatalf("got %d %s", last.StatusCode, last.Body)
	}
	if len(bigIDs) < 45 || len(bigIDs) >= 52 {
		t.Fatalf("%d big shares", len(bigIDs))
	}
	if grow := h.Call(t, "/v1/shares/"+small["shareId"].(string), tu.Req{Method: "PUT", Token: ws.Token, JSON: map[string]any{"snapshot": big}}); grow.StatusCode != 413 {
		t.Fatalf("grow %d", grow.StatusCode)
	}
	h.Call(t, "/v1/shares/"+bigIDs[0], tu.Req{Method: "DELETE", Token: ws.Token})
	if r := h.Call(t, "/v1/shares", tu.Req{Token: ws.Token, JSON: map[string]any{"kind": "collection", "entityId": "again", "snapshot": big}}); r.StatusCode != 201 {
		t.Fatalf("after revoke: %d", r.StatusCode)
	}
}

func TestShares_UnknownOrMalformedIDsReturn404(t *testing.T) {
	h := tu.New(t)
	for _, p := range []string{"/s/AAAAAAAAAAAAAAAAAAAAAA", "/s/short", "/s/AAAAAAAAAAAAAAAAAAAAAA.json"} {
		res := h.Call(t, p, tu.Req{})
		if res.StatusCode != 404 || !strings.Contains(res.Header.Get("X-Robots-Tag"), "noindex") {
			t.Fatalf("%s: %d %q", p, res.StatusCode, res.Header.Get("X-Robots-Tag"))
		}
	}
	j := h.Call(t, "/s/short.json", tu.Req{})
	if j.ErrorCode(t) != "not_found" || j.Header.Get("Access-Control-Allow-Origin") != "*" || j.Header.Get("Cache-Control") != "no-store" ||
		j.Header.Get("Cross-Origin-Resource-Policy") != "cross-origin" {
		t.Fatalf("%v", j.Header)
	}
	page := h.Call(t, "/s/short", tu.Req{})
	if !strings.Contains(string(page.Body), "Share not found") || page.Header.Get("Content-Type") != "text/html; charset=utf-8" || page.Header.Get("Cache-Control") != "no-store" {
		t.Fatalf("%s", page.Body)
	}
}

func TestShares_SnapshotsOver1MBRejectedWith413(t *testing.T) {
	h := tu.New(t)
	ws := h.NewWorkspace(t, "")
	snap := map[string]any{"schemaVersion": 1, "request": tu.SampleRequest(map[string]any{"body": map[string]any{"type": "text", "text": strings.Repeat("x", 1024*1024)}})}
	res := h.Call(t, "/v1/shares", tu.Req{Token: ws.Token, JSON: map[string]any{"kind": "request", "entityId": "x", "snapshot": snap}})
	if res.StatusCode != 413 || res.ErrorCode(t) != "snapshot_too_large" {
		t.Fatalf("got %d %s", res.StatusCode, res.Body)
	}
}

func TestShares_PublicPagesSendNoindexAndPublicMaxAge(t *testing.T) {
	h := tu.New(t)
	ws := h.NewWorkspace(t, "")
	id := createShare(t, h, ws.Token, nil, "")["shareId"].(string)
	for _, p := range []string{"/s/" + id, "/s/" + id + ".json"} {
		res := h.Call(t, p, tu.Req{})
		if !strings.Contains(res.Header.Get("X-Robots-Tag"), "noindex") || res.Header.Get("Cache-Control") != "public, max-age=60" ||
			res.Header.Get("Set-Cookie") != "" || res.Header.Get("X-Content-Type-Options") != "nosniff" {
			t.Fatalf("%s: %v", p, res.Header)
		}
	}
	csp := h.Call(t, "/s/"+id, tu.Req{}).Header.Get("Content-Security-Policy")
	if !strings.Contains(csp, "default-src 'none'") || !strings.Contains(csp, "frame-ancestors 'none'") || strings.Contains(csp, "unsafe") {
		t.Fatalf("csp %q", csp)
	}
}

// ---- Check order (§1.2 Shares) ----

func TestShares_Order_PutToRevokedShareWithTextPlainIs410(t *testing.T) {
	h := tu.New(t)
	ws := h.NewWorkspace(t, "")
	id := createShare(t, h, ws.Token, nil, "")["shareId"].(string)
	h.Call(t, "/v1/shares/"+id, tu.Req{Method: "DELETE", Token: ws.Token})
	res := h.Call(t, "/v1/shares/"+id, tu.Req{Method: "PUT", Token: ws.Token, Raw: tu.Str("not json"), Headers: map[string]string{"Content-Type": "text/plain"}})
	if res.StatusCode != 410 || res.ErrorCode(t) != "gone" {
		t.Fatalf("got %d %s", res.StatusCode, res.Body)
	}
}

func TestShares_Order_PutToAnotherWorkspacesShareWithOversizedBodyIs404(t *testing.T) {
	h := tu.New(t)
	owner := h.NewWorkspace(t, "")
	other := h.NewWorkspace(t, "")
	id := createShare(t, h, owner.Token, nil, "")["shareId"].(string)
	big := strings.Repeat("x", 6*1024*1024)
	res := h.Call(t, "/v1/shares/"+id, tu.Req{Method: "PUT", Token: other.Token, Raw: &big, Headers: map[string]string{"Content-Type": "application/json"}})
	if res.StatusCode != 404 {
		t.Fatalf("got %d", res.StatusCode)
	}
	// A malformed (not 22 base62) but route-valid id is a 404 too, before the body.
	res = h.Call(t, "/v1/shares/short", tu.Req{Method: "PUT", Token: owner.Token, Raw: &big, Headers: map[string]string{"Content-Type": "application/json"}})
	if res.StatusCode != 404 || !strings.Contains(string(res.Body), "Share not found") {
		t.Fatalf("got %d", res.StatusCode)
	}
}

func TestShares_Order_DeleteNeverReadsABody(t *testing.T) {
	h := tu.New(t)
	ws := h.NewWorkspace(t, "")
	id := createShare(t, h, ws.Token, nil, "")["shareId"].(string)
	big := strings.Repeat("\x00garbage", 800*1024)
	res := h.Call(t, "/v1/shares/"+id, tu.Req{Method: "DELETE", Token: ws.Token, Raw: &big})
	if res.StatusCode != 200 {
		t.Fatalf("got %d %s", res.StatusCode, res.Body)
	}
}

func TestShares_CreateIsLimitedPerWorkspace(t *testing.T) {
	h := tu.New(t)
	ws := h.NewWorkspace(t, "")
	for i := 0; i < 30; i++ {
		createShare(t, h, ws.Token, map[string]any{"schemaVersion": 1, "request": map[string]any{"name": "r"}}, "request")
	}
	res := h.Call(t, "/v1/shares", tu.Req{Token: ws.Token, JSON: map[string]any{"kind": "request", "entityId": "x", "snapshot": map[string]any{"schemaVersion": 1, "request": map[string]any{}}}})
	if res.StatusCode != 429 || res.ErrorCode(t) != "rate_limited" || res.Header.Get("Retry-After") == "" {
		t.Fatalf("got %d", res.StatusCode)
	}
	// Another workspace has its own budget.
	other := h.NewWorkspace(t, "")
	createShare(t, h, other.Token, nil, "")
}

func TestShares_InvalidCreatesDoNotUseTheLimit(t *testing.T) {
	h := tu.New(t, tu.WithEnv("REQHARBOR_RATE_SHARE_CREATE_PER_HOUR=2"))
	ws := h.NewWorkspace(t, "")
	for _, body := range []map[string]any{
		{"kind": "folder", "entityId": "x", "snapshot": map[string]any{"schemaVersion": 1}},
		{"kind": "request", "entityId": "bad id!", "snapshot": map[string]any{"schemaVersion": 1, "request": map[string]any{}}},
		{"kind": "request", "entityId": "x", "snapshot": map[string]any{"schemaVersion": 0, "request": map[string]any{}}},
		{"kind": "request", "entityId": "x"},
	} {
		if res := h.Call(t, "/v1/shares", tu.Req{Token: ws.Token, JSON: body}); res.StatusCode != 400 {
			t.Fatalf("invalid create: %d %s", res.StatusCode, res.Body)
		}
	}
	snap := map[string]any{"schemaVersion": 1, "request": map[string]any{"name": "r"}}
	createShare(t, h, ws.Token, snap, "request")
	createShare(t, h, ws.Token, snap, "request")
	if res := h.Call(t, "/v1/shares", tu.Req{Token: ws.Token, JSON: map[string]any{"kind": "request", "entityId": "x", "snapshot": snap}}); res.StatusCode != 429 {
		t.Fatalf("third valid create: %d", res.StatusCode)
	}
}
