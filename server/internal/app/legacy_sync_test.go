package app_test

// Port of worker/test/sync.test.ts (06-go-server.md §1.6) plus the S1 additions.

import (
	"context"
	"fmt"
	"net/http"
	"regexp"
	"strings"
	"sync"
	"testing"

	"github.com/mihajlopopara/reqharbor/server/internal/ids"
	"github.com/mihajlopopara/reqharbor/server/internal/jobs"
	tu "github.com/mihajlopopara/reqharbor/server/internal/testutil"
)

// jsonEsc builds JSON escapes at run time (see jsonx's esc).
func jsonEsc(hex ...string) string {
	var b strings.Builder
	for _, h := range hex {
		b.WriteString(`\` + "u" + h)
	}
	return b.String()
}

func ipHeader(ip string) map[string]string { return map[string]string{"CF-Connecting-IP": ip} }

func num(v any) int64 {
	f, _ := v.(float64)
	return int64(f)
}

func TestSync_AC_SYNC_1_CreateWorkspaceReturnsIDAndTokenOnce(t *testing.T) {
	h := tu.New(t)
	res := h.Call(t, "/v1/workspaces", tu.Req{JSON: map[string]any{"name": "  My Workspace  "}, Headers: ipHeader(tu.RandomIP())})
	if res.StatusCode != 201 {
		t.Fatalf("got %d %s", res.StatusCode, res.Body)
	}
	v := res.JSON(t)
	if len(v) != 2 {
		t.Fatalf("keys: %v", v)
	}
	token, _ := v["token"].(string)
	wsID, _ := v["workspaceId"].(string)
	if !regexp.MustCompile(`^rh_[A-Za-z0-9_-]{43}$`).MatchString(token) || !regexp.MustCompile(`^[0-9A-HJKMNP-TV-Z]{26}$`).MatchString(wsID) {
		t.Fatalf("bad ids: %v", v)
	}
	me := h.Call(t, "/v1/workspace", tu.Req{Token: token})
	if me.StatusCode != 200 {
		t.Fatalf("got %d", me.StatusCode)
	}
	m := me.JSON(t)
	tu.JSONEq(t, me.Body, map[string]any{
		"workspaceId": wsID, "name": "My Workspace", "createdAt": m["createdAt"],
		"entityCount": 0, "bytesUsed": 0, "quotaBytes": 50 * 1024 * 1024,
	})
	if num(m["createdAt"]) != h.Clock.Now().UnixMilli() {
		t.Fatalf("createdAt %v", m["createdAt"])
	}
	if strings.Contains(string(me.Body), token) {
		t.Fatal("token readable again")
	}
}

func TestSync_AC_SYNC_1_OnlySHA256OfTokenIsStored(t *testing.T) {
	h := tu.New(t)
	ws := h.NewWorkspace(t, "")
	var hash, dump string
	row := h.DB.Reader.QueryRow("SELECT token_hash, id || name || token_hash || rev || created_at FROM workspaces WHERE id = ?", ws.ID)
	if err := row.Scan(&hash, &dump); err != nil {
		t.Fatal(err)
	}
	if hash != ids.SHA256Hex(ws.Token) {
		t.Fatalf("hash %s", hash)
	}
	if strings.Contains(dump, ws.Token) {
		t.Fatal("token stored")
	}
}

func TestSync_AC_SYNC_1_WorkspaceNameIsValidated(t *testing.T) {
	h := tu.New(t)
	for _, body := range []map[string]any{{}, {"name": ""}, {"name": "   "}, {"name": 42}, {"name": strings.Repeat("x", 201)}, {"name": "　\ufeff"}} {
		res := h.Call(t, "/v1/workspaces", tu.Req{JSON: body, Headers: ipHeader(tu.RandomIP())})
		if res.StatusCode != 400 || res.ErrorCode(t) != "validation_failed" {
			t.Fatalf("%v: got %d %s", body, res.StatusCode, res.Body)
		}
	}
	// Lengths are UTF-16 code units, as JavaScript counts them: 100 emoji are 200 units.
	ok := h.Call(t, "/v1/workspaces", tu.Req{JSON: map[string]any{"name": strings.Repeat("😀", 100)}, Headers: ipHeader(tu.RandomIP())})
	if ok.StatusCode != 201 {
		t.Fatalf("got %d", ok.StatusCode)
	}
	tooLong := h.Call(t, "/v1/workspaces", tu.Req{JSON: map[string]any{"name": strings.Repeat("😀", 100) + "x"}, Headers: ipHeader(tu.RandomIP())})
	if tooLong.StatusCode != 400 {
		t.Fatalf("got %d", tooLong.StatusCode)
	}
}

func TestSync_AC_SYNC_1_CreateIsRateLimitedTo10PerHourPerIP(t *testing.T) {
	h := tu.New(t)
	ip := tu.RandomIP()
	for i := 0; i < 10; i++ {
		if res := h.Call(t, "/v1/workspaces", tu.Req{JSON: map[string]any{"name": fmt.Sprintf("w%d", i)}, Headers: ipHeader(ip)}); res.StatusCode != 201 {
			t.Fatalf("create %d: %d", i, res.StatusCode)
		}
	}
	blocked := h.Call(t, "/v1/workspaces", tu.Req{JSON: map[string]any{"name": "w11"}, Headers: ipHeader(ip)})
	if blocked.StatusCode != 429 || blocked.ErrorCode(t) != "rate_limited" {
		t.Fatalf("got %d", blocked.StatusCode)
	}
	if ra := blocked.Header.Get("Retry-After"); ra == "" || ra == "0" {
		t.Fatalf("Retry-After %q", ra)
	}
	if other := h.Call(t, "/v1/workspaces", tu.Req{JSON: map[string]any{"name": "ok"}, Headers: ipHeader(tu.RandomIP())}); other.StatusCode != 201 {
		t.Fatalf("other IP: %d", other.StatusCode)
	}
	// Validation comes before the limit: invalid names do not use up the budget.
	if res := h.Call(t, "/v1/workspaces", tu.Req{JSON: map[string]any{"name": ""}, Headers: ipHeader(ip)}); res.StatusCode != 400 {
		t.Fatalf("got %d", res.StatusCode)
	}
	// The window drains: one more create is allowed after 6 minutes.
	h.Clock.Advance(360_000_000_000)
	if res := h.Call(t, "/v1/workspaces", tu.Req{JSON: map[string]any{"name": "later"}, Headers: ipHeader(ip)}); res.StatusCode != 201 {
		t.Fatalf("after the window: %d", res.StatusCode)
	}
}

func TestSync_AC_SYNC_7_MissingMalformedUnknownTokensGet401(t *testing.T) {
	h := tu.New(t)
	unknown := "rh_" + strings.Repeat("A", 43)
	for _, hd := range []map[string]string{{}, {"Authorization": "Bearer nope"}, {"Authorization": "Basic " + unknown}, {"Authorization": "Bearer " + unknown}} {
		res := h.Call(t, "/v1/sync", tu.Req{Headers: hd})
		if res.StatusCode != 401 || !strings.Contains(res.Header.Get("WWW-Authenticate"), "Bearer") {
			t.Fatalf("%v: got %d", hd, res.StatusCode)
		}
		v := res.JSON(t)
		e := v["error"].(map[string]any)
		if e["code"] != "unauthorized" || e["message"] == "" {
			t.Fatalf("%s", res.Body)
		}
	}
	// Case-insensitive scheme and trailing whitespace are accepted.
	ws := h.NewWorkspace(t, "")
	if res := h.Call(t, "/v1/sync", tu.Req{Headers: map[string]string{"Authorization": "bearer   " + ws.Token + "  "}}); res.StatusCode != 200 {
		t.Fatalf("got %d", res.StatusCode)
	}
}

func TestSync_AC_SYNC_7_RotationInvalidatesTheOldTokenImmediately(t *testing.T) {
	h := tu.New(t)
	ws := h.NewWorkspace(t, "")
	h.Call(t, "/v1/sync", tu.Req{Token: ws.Token, JSON: tu.Changes(tu.Change("project", "p1", 1000, nil))})
	rot := h.Call(t, "/v1/workspace/token/rotate", tu.Req{Method: "POST", Token: ws.Token})
	if rot.StatusCode != 200 {
		t.Fatalf("got %d", rot.StatusCode)
	}
	fresh := rot.JSON(t)["token"].(string)
	if !strings.HasPrefix(fresh, "rh_") || fresh == ws.Token {
		t.Fatalf("token %q", fresh)
	}
	if res := h.Call(t, "/v1/workspace", tu.Req{Token: ws.Token}); res.StatusCode != 401 {
		t.Fatalf("old token: %d", res.StatusCode)
	}
	me := h.Call(t, "/v1/workspace", tu.Req{Token: fresh})
	if num(me.JSON(t)["entityCount"]) != 1 {
		t.Fatalf("%s", me.Body)
	}
}

func TestSync_QA_R1_007_ConcurrentRotationsExactlyOneWins(t *testing.T) {
	h := tu.New(t)
	ws := h.NewWorkspace(t, "")
	var wg sync.WaitGroup
	results := make([]*tu.Resp, 5)
	for i := range results {
		wg.Add(1)
		go func() {
			defer wg.Done()
			results[i] = h.Call(t, "/v1/workspace/token/rotate", tu.Req{Method: "POST", Token: ws.Token})
		}()
	}
	wg.Wait()
	var ok []*tu.Resp
	for _, r := range results {
		switch r.StatusCode {
		case 200:
			ok = append(ok, r)
		case 401:
		default:
			t.Fatalf("unexpected %d", r.StatusCode)
		}
	}
	if len(ok) != 1 {
		t.Fatalf("%d rotations succeeded", len(ok))
	}
	fresh := ok[0].JSON(t)["token"].(string)
	if h.Call(t, "/v1/sync", tu.Req{Token: ws.Token}).StatusCode != 401 || h.Call(t, "/v1/sync", tu.Req{Token: fresh}).StatusCode != 200 {
		t.Fatal("wrong token state after rotation")
	}
}

func TestSync_TokensAreScopedToTheirOwnWorkspace(t *testing.T) {
	h := tu.New(t)
	a := h.NewWorkspace(t, "A")
	b := h.NewWorkspace(t, "B")
	h.Call(t, "/v1/sync", tu.Req{Token: a.Token, JSON: tu.Changes(tu.Change("request", "r1", 1, nil))})
	pullB := h.Call(t, "/v1/sync", tu.Req{Token: b.Token})
	if got := pullB.JSON(t)["changes"].([]any); len(got) != 0 {
		t.Fatalf("B sees %v", got)
	}
}

func TestSync_AC_SYNC_2_PushThenPullOrderedByRevWithCursor(t *testing.T) {
	h := tu.New(t)
	ws := h.NewWorkspace(t, "")
	pushed := h.Call(t, "/v1/sync", tu.Req{Token: ws.Token, JSON: tu.Changes(
		tu.Change("project", "p1", 1000, map[string]any{"id": "p1", "name": "Default"}),
		tu.Change("collection", "c1", 1001, map[string]any{"id": "c1", "projectId": "p1", "name": "Users"}),
		tu.Change("request", "r1", 1002, map[string]any{"id": "r1", "collectionId": "c1", "method": "QUERY", "url": "/x"}),
	)})
	if pushed.StatusCode != 200 {
		t.Fatalf("got %d %s", pushed.StatusCode, pushed.Body)
	}
	tu.JSONEq(t, pushed.Body, `{"results":[
		{"type":"project","id":"p1","status":"applied","rev":1},
		{"type":"collection","id":"c1","status":"applied","rev":2},
		{"type":"request","id":"r1","status":"applied","rev":3}],"cursor":3}`)

	all := h.Call(t, "/v1/sync?since=0", tu.Req{Token: ws.Token}).JSON(t)
	if all["hasMore"] != false || num(all["cursor"]) != 3 {
		t.Fatalf("%v", all)
	}
	changes := all["changes"].([]any)
	tu.JSONEq(t, changes[2], `{"type":"request","id":"r1","updatedAt":1002,"deleted":false,"deviceId":"",
		"data":{"id":"r1","collectionId":"c1","method":"QUERY","url":"/x"},"rev":3}`)
	var order []string
	for _, c := range changes {
		m := c.(map[string]any)
		order = append(order, fmt.Sprintf("%s/%s/%d", m["type"], m["id"], num(m["rev"])))
	}
	if strings.Join(order, ",") != "project/p1/1,collection/c1/2,request/r1/3" {
		t.Fatalf("order %v", order)
	}
	after := h.Call(t, "/v1/sync?since=2", tu.Req{Token: ws.Token}).JSON(t)["changes"].([]any)
	if len(after) != 1 || after[0].(map[string]any)["id"] != "r1" {
		t.Fatalf("%v", after)
	}
	caught := h.Call(t, "/v1/sync?since=3", tu.Req{Token: ws.Token})
	tu.JSONEq(t, caught.Body, map[string]any{"changes": []any{}, "cursor": 3, "hasMore": false, "syncEpoch": h.App.SyncEpoch()})
}

func TestSync_AC_SYNC_2_PullPagesAnd500ChangesInOnePush(t *testing.T) {
	h := tu.New(t)
	ws := h.NewWorkspace(t, "")
	var cs []map[string]any
	for i := 0; i < 500; i++ {
		cs = append(cs, tu.Change("request", fmt.Sprintf("r%d", i), int64(1000+i), nil))
	}
	pushed := h.Call(t, "/v1/sync", tu.Req{Token: ws.Token, JSON: tu.Changes(cs...)})
	if pushed.StatusCode != 200 {
		t.Fatalf("got %d", pushed.StatusCode)
	}
	results := pushed.JSON(t)["results"].([]any)
	if len(results) != 500 {
		t.Fatalf("%d results", len(results))
	}
	for i, r := range results {
		m := r.(map[string]any)
		if m["status"] != "applied" || num(m["rev"]) != int64(i+1) {
			t.Fatalf("result %d: %v", i, m)
		}
	}
	p1 := h.Call(t, "/v1/sync?since=0&limit=200", tu.Req{Token: ws.Token}).JSON(t)
	if len(p1["changes"].([]any)) != 200 || p1["hasMore"] != true || num(p1["cursor"]) != 200 {
		t.Fatalf("page 1: hasMore %v cursor %v", p1["hasMore"], p1["cursor"])
	}
	p2 := h.Call(t, fmt.Sprintf("/v1/sync?since=%d&limit=500", num(p1["cursor"])), tu.Req{Token: ws.Token}).JSON(t)
	if len(p2["changes"].([]any)) != 300 || p2["hasMore"] != false || num(p2["cursor"]) != 500 {
		t.Fatalf("page 2: hasMore %v cursor %v", p2["hasMore"], p2["cursor"])
	}
	over := h.Call(t, "/v1/sync", tu.Req{Token: ws.Token, JSON: tu.Changes(append(cs, tu.Change("request", "x", 1, nil))...)})
	if over.StatusCode != 413 || over.ErrorCode(t) != "too_many_changes" {
		t.Fatalf("got %d", over.StatusCode)
	}
}

func TestSync_AC_SYNC_3_OnlyTheFiveTypesAreAccepted(t *testing.T) {
	h := tu.New(t)
	ws := h.NewWorkspace(t, "")
	var cs []map[string]any
	for i, typ := range []string{"project", "collection", "folder", "request", "environment"} {
		cs = append(cs, tu.Change(typ, fmt.Sprintf("id%d", i), 1, nil))
	}
	if res := h.Call(t, "/v1/sync", tu.Req{Token: ws.Token, JSON: tu.Changes(cs...)}); res.StatusCode != 200 {
		t.Fatalf("got %d", res.StatusCode)
	}
	for _, typ := range []string{"history", "settings", "workspace", "Request", "protoFile"} {
		bad := h.Call(t, "/v1/sync", tu.Req{Token: ws.Token, JSON: tu.Changes(tu.Change("request", "ok", 5, nil), tu.Change(typ, "x", 1, nil))})
		if bad.StatusCode != 400 || bad.ErrorCode(t) != "unsupported_type" {
			t.Fatalf("%s: got %d", typ, bad.StatusCode)
		}
	}
	// Rejected pushes are atomic: nothing was written.
	if n := num(h.Call(t, "/v1/workspace", tu.Req{Token: ws.Token}).JSON(t)["entityCount"]); n != 5 {
		t.Fatalf("entityCount %d", n)
	}
}

func TestSync_AC_SYNC_4_LWWOlderPushIsStaleAndServerCopyComesBack(t *testing.T) {
	h := tu.New(t)
	ws := h.NewWorkspace(t, "")
	h.Call(t, "/v1/sync", tu.Req{Token: ws.Token, JSON: tu.Changes(tu.Change("request", "r1", 2000, map[string]any{"name": "device B (newer)"}))})
	cursor := num(h.Call(t, "/v1/sync?since=0", tu.Req{Token: ws.Token}).JSON(t)["cursor"])
	stale := h.Call(t, "/v1/sync", tu.Req{Token: ws.Token, JSON: tu.Changes(tu.Change("request", "r1", 1500, map[string]any{"name": "device A (older)"}))})
	tu.JSONEq(t, stale.JSON(t)["results"], []any{map[string]any{"type": "request", "id": "r1", "status": "stale", "rev": cursor + 1}})
	next := h.Call(t, fmt.Sprintf("/v1/sync?since=%d", cursor), tu.Req{Token: ws.Token})
	tu.JSONEq(t, next.JSON(t)["changes"], []any{map[string]any{
		"type": "request", "id": "r1", "updatedAt": 2000, "deleted": false, "deviceId": "", "data": map[string]any{"name": "device B (newer)"}, "rev": cursor + 1,
	}})
	newer := h.Call(t, "/v1/sync", tu.Req{Token: ws.Token, JSON: tu.Changes(tu.Change("request", "r1", 2500, map[string]any{"name": "A again"}))})
	if newer.JSON(t)["results"].([]any)[0].(map[string]any)["status"] != "applied" {
		t.Fatalf("%s", newer.Body)
	}
	final := h.Call(t, "/v1/sync?since=0", tu.Req{Token: ws.Token}).JSON(t)["changes"].([]any)
	if len(final) != 1 {
		t.Fatalf("%v", final)
	}
	tu.JSONEq(t, final[0].(map[string]any)["data"], `{"name":"A again"}`)
}

func status0(t *testing.T, r *tu.Resp) string {
	t.Helper()
	return r.JSON(t)["results"].([]any)[0].(map[string]any)["status"].(string)
}

func TestSync_AC_SYNC_4_EqualUpdatedAtIsApplied(t *testing.T) {
	h := tu.New(t)
	ws := h.NewWorkspace(t, "")
	h.Call(t, "/v1/sync", tu.Req{Token: ws.Token, JSON: tu.Changes(tu.Change("folder", "f1", 1000, map[string]any{"name": "one"}))})
	tie := h.Call(t, "/v1/sync", tu.Req{Token: ws.Token, JSON: tu.Changes(tu.Change("folder", "f1", 1000, map[string]any{"name": "two"}))})
	if status0(t, tie) != "applied" {
		t.Fatalf("%s", tie.Body)
	}
}

func TestSync_AC_SYNC_4_MixedPushReportsAppliedAndStale(t *testing.T) {
	h := tu.New(t)
	ws := h.NewWorkspace(t, "")
	h.Call(t, "/v1/sync", tu.Req{Token: ws.Token, JSON: tu.Changes(tu.Change("request", "a", 5000, nil), tu.Change("request", "b", 5000, nil))})
	res := h.Call(t, "/v1/sync", tu.Req{Token: ws.Token, JSON: tu.Changes(tu.Change("request", "a", 4000, nil), tu.Change("request", "b", 6000, nil), tu.Change("request", "c", 1, nil))})
	tu.JSONEq(t, res.Body, `{"results":[
		{"type":"request","id":"a","status":"stale","rev":3},
		{"type":"request","id":"b","status":"applied","rev":4},
		{"type":"request","id":"c","status":"applied","rev":5}],"cursor":5}`)
}

func TestSync_RevsAreAssignedInPushOrderIncludingStaleBumps(t *testing.T) {
	h := tu.New(t)
	ws := h.NewWorkspace(t, "")
	h.Call(t, "/v1/sync", tu.Req{Token: ws.Token, JSON: tu.Changes(tu.Change("request", "a", 10, nil), tu.Change("request", "b", 10, nil), tu.Change("request", "c", 10, nil))})
	// Push in a different order: b (stale), oversized (rejected, no rev), a (applied), c (stale).
	big := tu.Change("request", "big", 1, map[string]any{"blob": strings.Repeat("x", 512*1024)})
	res := h.Call(t, "/v1/sync", tu.Req{Token: ws.Token, JSON: tu.Changes(tu.Change("request", "b", 5, nil), big, tu.Change("request", "a", 11, nil), tu.Change("request", "c", 9, nil))})
	tu.JSONEq(t, res.Body, `{"results":[
		{"type":"request","id":"b","status":"stale","rev":4},
		{"type":"request","id":"big","status":"rejected","reason":"entity_too_large"},
		{"type":"request","id":"a","status":"applied","rev":5},
		{"type":"request","id":"c","status":"stale","rev":6}],"cursor":6}`)
	pulled := h.Call(t, "/v1/sync?since=3", tu.Req{Token: ws.Token}).JSON(t)["changes"].([]any)
	var got []string
	for _, c := range pulled {
		m := c.(map[string]any)
		got = append(got, fmt.Sprintf("%s@%d=%d", m["id"], num(m["rev"]), num(m["updatedAt"])))
	}
	if strings.Join(got, ",") != "b@4=10,a@5=11,c@6=10" {
		t.Fatalf("%v", got)
	}
	if n := num(h.Call(t, "/v1/sync?since=6", tu.Req{Token: ws.Token}).JSON(t)["cursor"]); n != 6 {
		t.Fatalf("head %d", n)
	}
}

func TestSync_AC_SYNC_5_DeletesPropagateAsTombstones(t *testing.T) {
	h := tu.New(t)
	ws := h.NewWorkspace(t, "")
	h.Call(t, "/v1/sync", tu.Req{Token: ws.Token, JSON: tu.Changes(tu.Change("collection", "c1", 1000, nil), tu.Change("request", "r1", 1000, map[string]any{"collectionId": "c1"}))})
	del := h.Call(t, "/v1/sync", tu.Req{Token: ws.Token, JSON: tu.Changes(tu.Tombstone("collection", "c1", 2000))})
	if status0(t, del) != "applied" {
		t.Fatalf("%s", del.Body)
	}
	pulled := h.Call(t, "/v1/sync?since=2", tu.Req{Token: ws.Token})
	tu.JSONEq(t, pulled.JSON(t)["changes"], `[{"type":"collection","id":"c1","updatedAt":2000,"deleted":true,"deviceId":"","data":null,"rev":3}]`)
	if n := num(h.Call(t, "/v1/workspace", tu.Req{Token: ws.Token}).JSON(t)["entityCount"]); n != 1 {
		t.Fatalf("entityCount %d", n)
	}
	if s := status0(t, h.Call(t, "/v1/sync", tu.Req{Token: ws.Token, JSON: tu.Changes(tu.Change("collection", "c1", 1500, nil))})); s != "stale" {
		t.Fatal(s)
	}
	if s := status0(t, h.Call(t, "/v1/sync", tu.Req{Token: ws.Token, JSON: tu.Changes(tu.Change("collection", "c1", 3000, nil))})); s != "applied" {
		t.Fatal(s)
	}
}

func TestSync_AC_SYNC_5_TombstonesMustNotCarryData(t *testing.T) {
	h := tu.New(t)
	ws := h.NewWorkspace(t, "")
	bad := h.Call(t, "/v1/sync", tu.Req{Token: ws.Token, JSON: map[string]any{"changes": []any{map[string]any{"type": "request", "id": "r", "updatedAt": 1, "deleted": true, "data": map[string]any{"x": 1}}}}})
	if bad.StatusCode != 400 {
		t.Fatalf("got %d", bad.StatusCode)
	}
	// Absent data is fine for a tombstone.
	ok := h.Call(t, "/v1/sync", tu.Req{Token: ws.Token, JSON: map[string]any{"changes": []any{map[string]any{"type": "request", "id": "r", "updatedAt": 1, "deleted": true}}}})
	if ok.StatusCode != 200 {
		t.Fatalf("got %d", ok.StatusCode)
	}
}

func TestSync_AC_ENV_7_SecretEnvironmentValuesAreNeverStored(t *testing.T) {
	h := tu.New(t)
	ws := h.NewWorkspace(t, "")
	env := map[string]any{
		"id": "e1", "name": "Staging", "projectId": "p1",
		"variables": []any{
			map[string]any{"key": "baseUrl", "value": "https://staging.example.com", "enabled": true, "secret": false},
			map[string]any{"key": "token", "value": "super-secret-value", "enabled": true, "secret": true},
			map[string]any{"key": "noValue", "enabled": true, "secret": true},
		},
	}
	h.Call(t, "/v1/sync", tu.Req{Token: ws.Token, JSON: tu.Changes(tu.Change("environment", "e1", 1000, env))})
	pulled := h.Call(t, "/v1/sync", tu.Req{Token: ws.Token}).JSON(t)["changes"].([]any)
	tu.JSONEq(t, pulled[0].(map[string]any)["data"].(map[string]any)["variables"], `[
		{"key":"baseUrl","value":"https://staging.example.com","enabled":true,"secret":false},
		{"key":"token","value":"","enabled":true,"secret":true},
		{"key":"noValue","enabled":true,"secret":true,"value":""}]`)
	var raw string
	if err := h.DB.Reader.QueryRow("SELECT data FROM entities WHERE workspace_id = ?", ws.ID).Scan(&raw); err != nil {
		t.Fatal(err)
	}
	if strings.Contains(raw, "super-secret-value") {
		t.Fatal("secret stored")
	}
}

func TestSync_ValidatesChangeShapeAndQueryParams(t *testing.T) {
	h := tu.New(t)
	ws := h.NewWorkspace(t, "")
	ch := func(over map[string]any) map[string]any {
		c := map[string]any{"type": "request", "id": "a", "updatedAt": 1, "deleted": false, "data": map[string]any{}}
		for k, v := range over {
			c[k] = v
		}
		return map[string]any{"changes": []any{c}}
	}
	cases := []any{
		map[string]any{},
		map[string]any{"changes": "nope"},
		map[string]any{"changes": []any{nil}},
		ch(map[string]any{"id": ""}),
		ch(map[string]any{"id": "a/b"}),
		ch(map[string]any{"updatedAt": -1}),
		ch(map[string]any{"updatedAt": 1.5}),
		ch(map[string]any{"updatedAt": "1"}),
		ch(map[string]any{"updatedAt": 9007199254740992}),
		ch(map[string]any{"deleted": "no"}),
		ch(map[string]any{"data": nil}),
		ch(map[string]any{"data": []any{1}}),
		tu.Changes(tu.Change("request", "dup", 1, nil), tu.Change("request", "dup", 2, nil)),
	}
	for _, body := range cases {
		res := h.Call(t, "/v1/sync", tu.Req{Token: ws.Token, JSON: body})
		if res.StatusCode != 400 {
			t.Fatalf("%v: got %d %s", body, res.StatusCode, res.Body)
		}
		e := res.JSON(t)["error"].(map[string]any)
		if e["code"] == "" || e["message"] == "" {
			t.Fatalf("%s", res.Body)
		}
	}
	// 1e3 is a safe integer in JavaScript, so it is accepted as today.
	exp := `{"changes":[{"type":"request","id":"e","updatedAt":1e3,"deleted":false,"data":{}}]}`
	if res := h.Call(t, "/v1/sync", tu.Req{Token: ws.Token, Raw: &exp, Headers: map[string]string{"Content-Type": "application/json"}}); res.StatusCode != 200 {
		t.Fatalf("1e3: %d %s", res.StatusCode, res.Body)
	}
	for _, qs := range []string{"since=-1", "since=abc", "limit=0", "limit=1.5", "since=9007199254740992"} {
		if res := h.Call(t, "/v1/sync?"+qs, tu.Req{Token: ws.Token}); res.StatusCode != 400 {
			t.Fatalf("%s: %d", qs, res.StatusCode)
		}
	}
	if res := h.Call(t, "/v1/sync?limit=10000", tu.Req{Token: ws.Token}); res.StatusCode != 200 {
		t.Fatalf("clamped limit: %d", res.StatusCode)
	}
	if res := h.Call(t, "/v1/sync?since=&limit=", tu.Req{Token: ws.Token}); res.StatusCode != 200 {
		t.Fatalf("empty params: %d", res.StatusCode)
	}
}

func TestSync_QA_R1_005_EntityOver512KBRejectedPerChange(t *testing.T) {
	h := tu.New(t)
	ws := h.NewWorkspace(t, "")
	big := tu.Change("request", "big", 1, map[string]any{"blob": strings.Repeat("x", 512*1024)})
	fits := tu.Change("request", "fits", 1, map[string]any{"blob": strings.Repeat("x", 500*1024)})
	small := tu.Change("request", "small", 1, map[string]any{"name": "small"})
	res := h.Call(t, "/v1/sync", tu.Req{Token: ws.Token, JSON: tu.Changes(small, big, fits)})
	tu.JSONEq(t, res.Body, `{"results":[
		{"type":"request","id":"small","status":"applied","rev":1},
		{"type":"request","id":"big","status":"rejected","reason":"entity_too_large"},
		{"type":"request","id":"fits","status":"applied","rev":2}],"cursor":2}`)
	pulled := h.Call(t, "/v1/sync?since=0", tu.Req{Token: ws.Token}).JSON(t)["changes"].([]any)
	if len(pulled) != 2 || pulled[0].(map[string]any)["id"] != "small" || pulled[1].(map[string]any)["id"] != "fits" {
		t.Fatalf("pulled %d", len(pulled))
	}
	only := h.Call(t, "/v1/sync", tu.Req{Token: ws.Token, JSON: tu.Changes(tu.Change("request", "big2", 1, map[string]any{"blob": strings.Repeat("y", 600*1024)}))})
	tu.JSONEq(t, only.Body, `{"results":[{"type":"request","id":"big2","status":"rejected","reason":"entity_too_large"}],"cursor":2}`)
	mb := h.Call(t, "/v1/sync", tu.Req{Token: ws.Token, JSON: tu.Changes(tu.Change("request", "mb", 1, map[string]any{"blob": strings.Repeat("é", 300*1024)}))})
	if status0(t, mb) != "rejected" {
		t.Fatalf("%s", mb.Body)
	}
}

func TestSync_QA_R1_004_BodyOver5MBIsAWholeRequest413(t *testing.T) {
	h := tu.New(t)
	ws := h.NewWorkspace(t, "")
	var cs []map[string]any
	for i := 0; i < 11; i++ {
		cs = append(cs, tu.Change("request", fmt.Sprintf("r%d", i), 1, map[string]any{"blob": strings.Repeat("x", 500*1024)}))
	}
	res := h.Call(t, "/v1/sync", tu.Req{Token: ws.Token, JSON: tu.Changes(cs...)})
	if res.StatusCode != 413 || res.ErrorCode(t) != "payload_too_large" {
		t.Fatalf("got %d", res.StatusCode)
	}
	for _, batch := range [][]map[string]any{cs[:6], cs[6:]} {
		r := h.Call(t, "/v1/sync", tu.Req{Token: ws.Token, JSON: tu.Changes(batch...)})
		if r.StatusCode != 200 {
			t.Fatalf("batch: %d", r.StatusCode)
		}
		for _, x := range r.JSON(t)["results"].([]any) {
			if x.(map[string]any)["status"] != "applied" {
				t.Fatalf("%v", x)
			}
		}
	}
}

func withDevice(c map[string]any, dev any) map[string]any {
	c["deviceId"] = dev
	return c
}

func TestSync_QA_R1_026_EqualUpdatedAtResolvedByGreaterDeviceID(t *testing.T) {
	for _, order := range [][2]string{{"A", "B"}, {"B", "A"}} {
		h := tu.New(t)
		ws := h.NewWorkspace(t, "")
		push := func(dev string) *tu.Resp {
			return h.Call(t, "/v1/sync", tu.Req{Token: ws.Token, JSON: tu.Changes(withDevice(tu.Change("request", "r1", 1000, map[string]any{"from": dev}), "01DEVICE"+dev))})
		}
		first, second := push(order[0]), push(order[1])
		if status0(t, first) != "applied" {
			t.Fatal("first not applied")
		}
		want := "stale"
		if order[1] == "B" {
			want = "applied"
		}
		if got := status0(t, second); got != want {
			t.Fatalf("%v: second %s", order, got)
		}
		c := h.Call(t, "/v1/sync?since=0", tu.Req{Token: ws.Token}).JSON(t)["changes"].([]any)
		m := c[0].(map[string]any)
		if len(c) != 1 || m["deviceId"] != "01DEVICEB" || m["data"].(map[string]any)["from"] != "B" {
			t.Fatalf("%v", c)
		}
	}
}

func TestSync_QA_R1_026_MissingDeviceIDCountsAsEmpty(t *testing.T) {
	h := tu.New(t)
	ws := h.NewWorkspace(t, "")
	push := func(c map[string]any) *tu.Resp {
		return h.Call(t, "/v1/sync", tu.Req{Token: ws.Token, JSON: tu.Changes(c)})
	}
	push(withDevice(tu.Change("folder", "f1", 5, map[string]any{"v": 1}), "01DEV"))
	if s := status0(t, push(tu.Change("folder", "f1", 5, map[string]any{"v": "old client"}))); s != "stale" {
		t.Fatal(s)
	}
	if s := status0(t, push(withDevice(tu.Change("folder", "f1", 5, map[string]any{"v": 2}), "01DEV"))); s != "applied" {
		t.Fatal(s)
	}
	if s := status0(t, push(withDevice(tu.Change("folder", "f1", 6, map[string]any{"v": "newer wins"}), nil))); s != "applied" {
		t.Fatal(s)
	}
	c := h.Call(t, "/v1/sync?since=0", tu.Req{Token: ws.Token}).JSON(t)["changes"].([]any)[0].(map[string]any)
	if c["deviceId"] != "" || c["data"].(map[string]any)["v"] != "newer wins" {
		t.Fatalf("%v", c)
	}
	for _, bad := range []any{5, "has space", strings.Repeat("x", 129)} {
		if r := push(withDevice(tu.Change("folder", "f2", 1, nil), bad)); r.StatusCode != 400 {
			t.Fatalf("%v: %d", bad, r.StatusCode)
		}
	}
}

func TestSync_QA_R1_024_EntityPushesCountTowardTheQuota(t *testing.T) {
	h := tu.New(t)
	ws := h.NewWorkspace(t, "")
	blob := strings.Repeat("x", 500*1024)
	var rev int64
	hit := false
	for batch := 0; batch < 14; batch++ {
		var cs []map[string]any
		for i := 0; i < 8; i++ {
			cs = append(cs, tu.Change("request", fmt.Sprintf("r%d-%d", batch, i), 1, map[string]any{"blob": blob}))
		}
		r := h.Call(t, "/v1/sync", tu.Req{Token: ws.Token, JSON: tu.Changes(cs...)})
		if r.StatusCode != 200 {
			if r.StatusCode != 413 || r.ErrorCode(t) != "quota_exceeded" {
				t.Fatalf("got %d %s", r.StatusCode, r.Body)
			}
			hit = true
			break
		}
		rev = num(r.JSON(t)["cursor"])
	}
	if !hit || rev == 0 {
		t.Fatal("quota never hit")
	}
	used := num(h.Call(t, "/v1/workspace", tu.Req{Token: ws.Token}).JSON(t)["bytesUsed"])
	if used <= 45*1024*1024 || used > 50*1024*1024 {
		t.Fatalf("bytesUsed %d", used)
	}
	del := h.Call(t, "/v1/sync", tu.Req{Token: ws.Token, JSON: tu.Changes(tu.Tombstone("request", "r0-0", 2), tu.Change("request", "r0-1", 2, map[string]any{"small": 1}))})
	if del.StatusCode != 200 {
		t.Fatalf("got %d", del.StatusCode)
	}
	if after := num(h.Call(t, "/v1/workspace", tu.Req{Token: ws.Token}).JSON(t)["bytesUsed"]); after >= used {
		t.Fatalf("bytesUsed %d >= %d", after, used)
	}
}

// ---- S1 additions --------------------------------------------------------------------------

func TestSync_QuotaIsEnforcedInsideTheTransaction(t *testing.T) {
	// A 1 MB quota: two concurrent 600 KB-total pushes each fit alone, but not together.
	h := tu.New(t, tu.WithEnv("REQHARBOR_QUOTA_LEGACY_BYTES=1000000"))
	ws := h.NewWorkspace(t, "")
	blob := strings.Repeat("x", 300*1024)
	var wg sync.WaitGroup
	codes := make([]int, 2)
	for i := range codes {
		wg.Add(1)
		go func() {
			defer wg.Done()
			r := h.Call(t, "/v1/sync", tu.Req{Token: ws.Token, JSON: tu.Changes(
				tu.Change("request", fmt.Sprintf("a%d", i), 1, map[string]any{"blob": blob}),
				tu.Change("request", fmt.Sprintf("b%d", i), 1, map[string]any{"blob": blob}),
			)})
			codes[i] = r.StatusCode
		}()
	}
	wg.Wait()
	if !((codes[0] == 200 && codes[1] == 413) || (codes[0] == 413 && codes[1] == 200)) {
		t.Fatalf("codes %v", codes)
	}
}

func TestSync_PullCarriesSyncEpoch(t *testing.T) {
	h := tu.New(t)
	ws := h.NewWorkspace(t, "")
	epoch := h.App.SyncEpoch()
	if !regexp.MustCompile(`^[0-9A-HJKMNP-TV-Z]{26}$`).MatchString(epoch) {
		t.Fatalf("epoch %q", epoch)
	}
	if got := h.Call(t, "/v1/sync", tu.Req{Token: ws.Token}).JSON(t)["syncEpoch"]; got != epoch {
		t.Fatalf("syncEpoch %v", got)
	}
	var stored string
	if err := h.DB.Reader.QueryRow("SELECT value FROM server_state WHERE key = 'sync_epoch'").Scan(&stored); err != nil || stored != epoch {
		t.Fatalf("stored %q %v", stored, err)
	}
	// Init is idempotent: the instance id and epoch survive a second start.
	inst := h.App.InstanceID()
	if err := h.App.Init(context.Background()); err != nil || h.App.SyncEpoch() != epoch || h.App.InstanceID() != inst {
		t.Fatal("state changed on re-init")
	}
}

func TestSync_InvalidJSONValueRejectedPerChangeWhileTheRestApplies(t *testing.T) {
	h := tu.New(t)
	ws := h.NewWorkspace(t, "")
	deep := strings.Repeat(`{"a":`, 101) + "1" + strings.Repeat("}", 101)  // data nests 101 levels
	ok100 := strings.Repeat(`{"a":`, 100) + "1" + strings.Repeat("}", 100) // exactly 100 levels
	body := `{"changes":[
		{"type":"request","id":"ok","updatedAt":1,"deleted":false,"data":{"n":1e308,"big":18446744073709551616}},
		{"type":"request","id":"inf","updatedAt":1,"deleted":false,"data":{"n":1e400}},
		{"type":"request","id":"sur","updatedAt":1,"deleted":false,"data":{"s":"\uD800"}},
		{"type":"request","id":"key","updatedAt":1,"deleted":false,"data":{"\uDC00":1}},
		{"type":"request","id":"deep","updatedAt":1,"deleted":false,"data":` + deep + `},
		{"type":"request","id":"d100","updatedAt":1,"deleted":false,"data":` + ok100 + `},
		{"type":"request","id":"pair","updatedAt":1,"deleted":false,"data":{"s":"` + jsonEsc("D83D", "DE00", "0020", "00e9", "0001") + `"}}
	]}`
	res := h.Call(t, "/v1/sync", tu.Req{Token: ws.Token, Raw: &body, Headers: map[string]string{"Content-Type": "application/json"}})
	tu.JSONEq(t, res.Body, `{"results":[
		{"type":"request","id":"ok","status":"applied","rev":1},
		{"type":"request","id":"inf","status":"rejected","reason":"invalid_json_value"},
		{"type":"request","id":"sur","status":"rejected","reason":"invalid_json_value"},
		{"type":"request","id":"key","status":"rejected","reason":"invalid_json_value"},
		{"type":"request","id":"deep","status":"rejected","reason":"invalid_json_value"},
		{"type":"request","id":"d100","status":"applied","rev":2},
		{"type":"request","id":"pair","status":"applied","rev":3}],"cursor":3}`)
	pull := h.Call(t, "/v1/sync?since=0", tu.Req{Token: ws.Token})
	// Number lexemes are kept verbatim and strings are re-escaped minimally (W1).
	if !strings.Contains(string(pull.Body), `"data":{"n":1e308,"big":18446744073709551616}`) || !strings.Contains(string(pull.Body), `"data":{"s":"😀 é\u0001"}`) {
		t.Fatalf("%s", pull.Body)
	}
	if n := len(pull.JSON(t)["changes"].([]any)); n != 3 {
		t.Fatalf("%d changes", n)
	}
}

func TestSync_DuplicateKeysKeepFirstPositionAndLastValue(t *testing.T) {
	h := tu.New(t)
	ws := h.NewWorkspace(t, "")
	body := `{"changes":[{"type":"request","id":"d","updatedAt":1,"deleted":false,"data":{"a":1,"b":2,"a":3}}]}`
	h.Call(t, "/v1/sync", tu.Req{Token: ws.Token, Raw: &body, Headers: map[string]string{"Content-Type": "application/json"}})
	pull := h.Call(t, "/v1/sync", tu.Req{Token: ws.Token})
	if !strings.Contains(string(pull.Body), `"data":{"a":3,"b":2}`) {
		t.Fatalf("%s", pull.Body)
	}
}

func TestSync_HTMLCharactersAreNotEscapedInResponses(t *testing.T) {
	h := tu.New(t)
	ws := h.NewWorkspace(t, "<A & B>")
	me := h.Call(t, "/v1/workspace", tu.Req{Token: ws.Token})
	if !strings.Contains(string(me.Body), `"name":"<A & B>"`) || strings.HasSuffix(string(me.Body), "\n") {
		t.Fatalf("%q", me.Body)
	}
}

func TestSync_LegacyWorkspacesModes(t *testing.T) {
	routes := []struct {
		method, path string
	}{
		{"POST", "/v1/workspaces"}, {"GET", "/v1/workspace"}, {"POST", "/v1/workspace/token/rotate"},
		{"GET", "/v1/sync"}, {"POST", "/v1/sync"}, {"GET", "/v1/shares"}, {"POST", "/v1/shares"},
		{"PUT", "/v1/shares/AAAAAAAAAAAAAAAAAAAAAA"}, {"DELETE", "/v1/shares/AAAAAAAAAAAAAAAAAAAAAA"},
	}
	// Seed a database with a workspace and a share while open, then reopen it in each mode.
	seed := tu.New(t)
	ws := seed.NewWorkspace(t, "")
	share := seed.Call(t, "/v1/shares", tu.Req{Token: ws.Token, JSON: map[string]any{"kind": "collection", "entityId": "C1", "snapshot": tu.SampleCollectionSnapshot()}}).JSON(t)
	shareID := share["shareId"].(string)

	for _, mode := range []string{"off", "existing", "open", ""} {
		env := []string{"REQHARBOR_LEGACY_WORKSPACES=" + mode}
		h := tu.New(t, tu.WithEnv(env...), tu.WithStore(seed.Store))
		for _, rt := range routes {
			// Without a token and with a garbage body: the 403 comes before auth and the body.
			res := h.Call(t, rt.path, tu.Req{Method: rt.method, Raw: tu.Str("garbage"), Headers: map[string]string{"Content-Type": "text/plain"}})
			disabled := mode == "off" || mode == "" || (mode == "existing" && rt.path == "/v1/workspaces")
			if disabled {
				if res.StatusCode != 403 || res.ErrorCode(t) != "legacy_disabled" {
					t.Fatalf("mode %q %s %s: got %d %s", mode, rt.method, rt.path, res.StatusCode, res.Body)
				}
			} else if res.StatusCode == 403 {
				t.Fatalf("mode %q %s %s: unexpectedly disabled", mode, rt.method, rt.path)
			}
		}
		// Existing tokens keep working with `existing`; shares stay public in every mode.
		pull := h.Call(t, "/v1/sync", tu.Req{Token: ws.Token})
		if (mode == "existing" || mode == "open") != (pull.StatusCode == 200) {
			t.Fatalf("mode %q: pull %d", mode, pull.StatusCode)
		}
		if res := h.Call(t, "/s/"+shareID+".json", tu.Req{}); res.StatusCode != 200 {
			t.Fatalf("mode %q: share %d", mode, res.StatusCode)
		}
		// The method check still comes first.
		if res := h.Call(t, "/v1/sync", tu.Req{Method: "PATCH"}); res.StatusCode != http.StatusMethodNotAllowed {
			t.Fatalf("mode %q: %d", mode, res.StatusCode)
		}
	}
}

func TestSync_LegacyTotalBytesCap(t *testing.T) {
	h := tu.New(t, tu.WithEnv("REQHARBOR_LEGACY_TOTAL_BYTES=2000"))
	ws := h.NewWorkspace(t, "")
	r := h.Call(t, "/v1/sync", tu.Req{Token: ws.Token, JSON: tu.Changes(tu.Change("request", "a", 1, map[string]any{"blob": strings.Repeat("x", 2500)}))})
	if r.StatusCode != 200 {
		t.Fatalf("first write: %d", r.StatusCode)
	}
	if err := h.App.Breaker.Sample(context.Background()); err != nil {
		t.Fatal(err)
	}
	if !h.App.Breaker.LegacyFull() {
		t.Fatal("cap not reached")
	}
	grow := h.Call(t, "/v1/sync", tu.Req{Token: ws.Token, JSON: tu.Changes(tu.Change("request", "b", 1, nil))})
	if grow.StatusCode != 507 || grow.ErrorCode(t) != "server_full" {
		t.Fatalf("growing push: %d %s", grow.StatusCode, grow.Body)
	}
	create := h.Call(t, "/v1/workspaces", tu.Req{JSON: map[string]any{"name": "x"}, Headers: ipHeader(tu.RandomIP())})
	if create.StatusCode != 507 || create.ErrorCode(t) != "server_full" {
		t.Fatalf("create: %d", create.StatusCode)
	}
	share := h.Call(t, "/v1/shares", tu.Req{Token: ws.Token, JSON: map[string]any{"kind": "collection", "entityId": "C1", "snapshot": tu.SampleCollectionSnapshot()}})
	if share.StatusCode != 507 {
		t.Fatalf("share: %d", share.StatusCode)
	}
	// Deletes, shrinking writes and reads keep working.
	if r := h.Call(t, "/v1/sync", tu.Req{Token: ws.Token, JSON: tu.Changes(tu.Change("request", "a", 2, map[string]any{"small": 1}))}); r.StatusCode != 200 {
		t.Fatalf("shrink: %d", r.StatusCode)
	}
	if r := h.Call(t, "/v1/sync", tu.Req{Token: ws.Token, JSON: tu.Changes(tu.Tombstone("request", "a", 3))}); r.StatusCode != 200 {
		t.Fatalf("delete: %d", r.StatusCode)
	}
	if r := h.Call(t, "/v1/sync", tu.Req{Token: ws.Token}); r.StatusCode != 200 {
		t.Fatalf("pull: %d", r.StatusCode)
	}
	// After the next sample the cap is no longer reached.
	_ = h.App.Breaker.Sample(context.Background())
	if r := h.Call(t, "/v1/sync", tu.Req{Token: ws.Token, JSON: tu.Changes(tu.Change("request", "b", 1, nil))}); r.StatusCode != 200 {
		t.Fatalf("after freeing space: %d", r.StatusCode)
	}
}

func TestSync_BreakerRefusesGrowingWritesOnly(t *testing.T) {
	for _, lim := range []jobs.BreakerLimits{{MaxDBBytes: 1}, {MinFreeDiskBytes: 1 << 62}} {
		h := tu.New(t)
		ws := h.NewWorkspace(t, "")
		h.Call(t, "/v1/sync", tu.Req{Token: ws.Token, JSON: tu.Changes(tu.Change("request", "a", 1, nil))})
		h.App.Breaker.SetLimits(lim)
		if err := h.App.Breaker.Sample(context.Background()); err != nil {
			t.Fatal(err)
		}
		if !h.App.Breaker.Open() {
			t.Fatalf("%+v: breaker closed", lim)
		}
		if r := h.Call(t, "/v1/sync", tu.Req{Token: ws.Token, JSON: tu.Changes(tu.Change("request", "b", 1, nil))}); r.StatusCode != 507 || r.ErrorCode(t) != "server_full" {
			t.Fatalf("%+v: grow %d", lim, r.StatusCode)
		}
		if r := h.Call(t, "/v1/workspaces", tu.Req{JSON: map[string]any{"name": "x"}, Headers: ipHeader(tu.RandomIP())}); r.StatusCode != 507 {
			t.Fatalf("%+v: create %d", lim, r.StatusCode)
		}
		if r := h.Call(t, "/v1/shares", tu.Req{Token: ws.Token, JSON: map[string]any{"kind": "collection", "entityId": "C1", "snapshot": tu.SampleCollectionSnapshot()}}); r.StatusCode != 507 {
			t.Fatalf("%+v: share %d", lim, r.StatusCode)
		}
		if r := h.Call(t, "/v1/sync", tu.Req{Token: ws.Token, JSON: tu.Changes(tu.Tombstone("request", "a", 2))}); r.StatusCode != 200 {
			t.Fatalf("%+v: delete %d", lim, r.StatusCode)
		}
		if r := h.Call(t, "/v1/sync", tu.Req{Token: ws.Token}); r.StatusCode != 200 {
			t.Fatalf("%+v: read %d", lim, r.StatusCode)
		}
		if lim.MinFreeDiskBytes > 0 {
			if r := h.Call(t, "/readyz", tu.Req{}); r.StatusCode != 503 || !strings.Contains(string(r.Body), `"disk":"low"`) {
				t.Fatalf("readyz %d %s", r.StatusCode, r.Body)
			}
		}
		h.App.Breaker.SetLimits(jobs.BreakerLimits{})
		_ = h.App.Breaker.Sample(context.Background())
		if r := h.Call(t, "/v1/sync", tu.Req{Token: ws.Token, JSON: tu.Changes(tu.Change("request", "b", 1, nil))}); r.StatusCode != 200 {
			t.Fatalf("%+v: after closing %d", lim, r.StatusCode)
		}
	}
}
