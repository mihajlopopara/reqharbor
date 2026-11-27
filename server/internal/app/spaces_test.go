package app_test

// S6 personal spaces (06-go-server.md §4, §10 S6): /v1/me*, /v1/spaces/{id}, space sync and
// space shares.

import (
	"context"
	"encoding/json"
	"errors"
	"net/http"
	"strings"
	"testing"
	"time"

	"github.com/mihajlopopara/reqharbor/server/internal/jobs"
	"github.com/mihajlopopara/reqharbor/server/internal/store"
	tu "github.com/mihajlopopara/reqharbor/server/internal/testutil"
)

func sizeOf(t *testing.T, v any) int64 {
	t.Helper()
	b, err := json.Marshal(v)
	if err != nil {
		t.Fatal(err)
	}
	return int64(len(b))
}

func spaceGet(t *testing.T, h *tu.Harness, access, spaceID string) map[string]any {
	t.Helper()
	res := h.Call(t, "/v1/spaces/"+spaceID, tu.Req{Token: access})
	if res.StatusCode != 200 {
		t.Fatalf("GET space: %d %s", res.StatusCode, res.Body)
	}
	return res.JSON(t)
}

func results(t *testing.T, res *tu.Resp) []map[string]any {
	t.Helper()
	if res.StatusCode != 200 {
		t.Fatalf("push: %d %s", res.StatusCode, res.Body)
	}
	raw, _ := res.JSON(t)["results"].([]any)
	out := make([]map[string]any, len(raw))
	for i, r := range raw {
		out[i], _ = r.(map[string]any)
	}
	return out
}

func TestSpaces_PersonalSpaceIsCreatedAtSignup(t *testing.T) {
	h := tu.New(t)
	tk := h.CreateUser(t, "ada@example.test")
	space := h.PersonalSpace(t, tk.Access)

	// One hidden personal team, the owner membership, and the space with the sentinel token hash.
	if n := h.Count(t, "teams", "personal = 1 AND owner_user_id = ?", tk.User.ID); n != 1 {
		t.Fatalf("personal teams: %d", n)
	}
	if n := h.Count(t, "team_members m JOIN workspaces w ON w.team_id = m.team_id", "w.id = ? AND m.user_id = ? AND m.role = 'owner'", space, tk.User.ID); n != 1 {
		t.Fatalf("owner membership: %d", n)
	}
	if n := h.Count(t, "workspaces", "id = ? AND token_hash = 'space:' || id AND team_id IS NOT NULL", space); n != 1 {
		t.Fatalf("space row: %d", n)
	}

	res := h.Call(t, "/v1/me/spaces", tu.Req{Token: tk.Access})
	tu.JSONEq(t, res.Body, map[string]any{
		"spaces": []any{map[string]any{"spaceId": space, "kind": "personal", "teamId": nil, "teamName": nil,
			"role": "owner", "bytesUsed": 0, "quotaBytes": 104857600, "memberCount": 1, "ownerUserId": tk.User.ID}},
		"owner":    map[string]any{"bytesUsed": 0, "quotaBytes": nil},
		"departed": []any{},
	})
	got := spaceGet(t, h, tk.Access, space)
	tu.JSONEq(t, got, map[string]any{"spaceId": space, "kind": "personal", "name": "Personal", "createdAt": h.Clock.Now().UnixMilli(),
		"entityCount": 0, "bytesUsed": 0, "quotaBytes": 104857600, "role": "owner"})
}

func TestSpaces_Me(t *testing.T) {
	h := tu.New(t)
	tk := h.CreateUser(t, "ada@example.test")
	res := h.Call(t, "/v1/me", tu.Req{Token: tk.Access})
	if res.StatusCode != 200 {
		t.Fatalf("%d %s", res.StatusCode, res.Body)
	}
	tu.JSONEq(t, res.Body, map[string]any{
		"user":   map[string]any{"id": tk.User.ID, "email": "ada@example.test", "name": "User", "avatarUrl": nil, "isAdmin": false},
		"method": "password", "sessionId": tk.SessionID,
	})
	if res.Header.Get("Cache-Control") != "no-store" {
		t.Fatalf("Cache-Control %q", res.Header.Get("Cache-Control"))
	}
	for _, path := range []string{"/v1/me", "/v1/me/sessions", "/v1/me/spaces"} {
		r := h.Call(t, path, tu.Req{})
		if r.StatusCode != 401 || r.ErrorCode(t) != "auth_required" || !strings.Contains(r.Header.Get("WWW-Authenticate"), `error="invalid_token"`) {
			t.Fatalf("%s without a token: %d %s %q", path, r.StatusCode, r.Body, r.Header.Get("WWW-Authenticate"))
		}
	}
	if r := h.Call(t, "/v1/me", tu.Req{Method: "POST", Token: tk.Access}); r.StatusCode != 405 || r.Header.Get("Allow") != "GET" {
		t.Fatalf("POST /v1/me: %d %q", r.StatusCode, r.Header.Get("Allow"))
	}
}

func TestSpaces_MeSessionsListAndRevoke(t *testing.T) {
	h := tu.New(t)
	first := h.CreateUser(t, "ada@example.test")
	h.Clock.Advance(time.Minute)
	second := h.SignIn(t, "ada@example.test", tu.DefaultPassword)
	other := h.CreateUser(t, "bob@example.test")

	res := h.Call(t, "/v1/me/sessions", tu.Req{Token: second.Access})
	if res.StatusCode != 200 {
		t.Fatalf("%d %s", res.StatusCode, res.Body)
	}
	day := h.Clock.Now().UnixMilli() / 86_400_000 * 86_400_000
	tu.JSONEq(t, res.Body, map[string]any{"sessions": []any{
		map[string]any{"id": second.SessionID, "deviceName": "MacBook Pro (macOS)", "createdAt": h.Clock.Now().UnixMilli(), "lastUsedDay": day, "current": true},
		map[string]any{"id": first.SessionID, "deviceName": "MacBook Pro (macOS)", "createdAt": h.Clock.Now().Add(-time.Minute).UnixMilli(), "lastUsedDay": day, "current": false},
	}})

	// Another user's session id is a 404 and revokes nothing; so is an unknown id.
	for _, id := range []string{other.SessionID, "01UNKNOWN0000000000000000X"} {
		r := h.Call(t, "/v1/me/sessions/"+id, tu.Req{Method: "DELETE", Token: second.Access})
		if r.StatusCode != 404 || r.ErrorCode(t) != "not_found" {
			t.Fatalf("revoke %s: %d %s", id, r.StatusCode, r.Body)
		}
	}
	if r := h.Call(t, "/v1/me", tu.Req{Token: other.Access}); r.StatusCode != 200 {
		t.Fatalf("other user's session was revoked: %d", r.StatusCode)
	}

	// Revoking the first device: 204, then that device gets 401 on its next call.
	if r := h.Call(t, "/v1/me/sessions/"+first.SessionID, tu.Req{Method: "DELETE", Token: second.Access}); r.StatusCode != 204 || len(r.Body) != 0 {
		t.Fatalf("revoke: %d %s", r.StatusCode, r.Body)
	}
	if r := h.Call(t, "/v1/me", tu.Req{Token: first.Access}); r.StatusCode != 401 || r.ErrorCode(t) != "auth_required" {
		t.Fatalf("revoked device: %d %s", r.StatusCode, r.Body)
	}
	if r := h.Refresh(t, first.Refresh); r.StatusCode != 400 || r.ErrorCode(t) != "invalid_grant" {
		t.Fatalf("revoked refresh: %d %s", r.StatusCode, r.Body)
	}
	if r := h.Call(t, "/v1/me/sessions/"+first.SessionID, tu.Req{Method: "DELETE", Token: second.Access}); r.StatusCode != 404 {
		t.Fatalf("second revoke: %d", r.StatusCode)
	}
	res = h.Call(t, "/v1/me/sessions", tu.Req{Token: second.Access})
	if list, _ := res.JSON(t)["sessions"].([]any); len(list) != 1 {
		t.Fatalf("after revoke: %s", res.Body)
	}
	// The current session can revoke itself (it is logout).
	if r := h.Call(t, "/v1/me/sessions/"+second.SessionID, tu.Req{Method: "DELETE", Token: second.Access}); r.StatusCode != 204 {
		t.Fatalf("revoke self: %d", r.StatusCode)
	}
	if r := h.Call(t, "/v1/me", tu.Req{Token: second.Access}); r.StatusCode != 401 {
		t.Fatalf("after revoking self: %d", r.StatusCode)
	}
}

func TestSpaces_SyncRoundTripWithUpdatedByServerTimeAndEpoch(t *testing.T) {
	h := tu.New(t)
	tk := h.CreateUser(t, "ada@example.test")
	space := h.PersonalSpace(t, tk.Access)
	now := h.Clock.Now().UnixMilli()

	c := tu.Change("request", "r1", now, nil)
	c["deviceId"] = "01DEVICEA"
	res := h.SpacePush(t, tk.Access, space, c, tu.Change("collection", "c1", now, nil))
	tu.JSONEq(t, res.Body, map[string]any{
		"results": []any{
			map[string]any{"type": "request", "id": "r1", "status": "applied", "rev": 1},
			map[string]any{"type": "collection", "id": "c1", "status": "applied", "rev": 2},
		},
		"cursor": 2, "serverTime": now,
	})
	// An older write is stale and keeps the stored version and its author.
	res = h.SpacePush(t, tk.Access, space, tu.Change("request", "r1", now-1, map[string]any{"older": true}))
	tu.JSONEq(t, res.Body, map[string]any{"results": []any{map[string]any{"type": "request", "id": "r1", "status": "stale", "rev": 3}}, "cursor": 3, "serverTime": now})

	pull := h.SpacePull(t, tk.Access, space, "since=0")
	if pull.StatusCode != 200 {
		t.Fatalf("pull: %d %s", pull.StatusCode, pull.Body)
	}
	by := map[string]any{"userId": tk.User.ID, "name": "User"}
	tu.JSONEq(t, pull.Body, map[string]any{
		"changes": []any{
			map[string]any{"type": "collection", "id": "c1", "updatedAt": now, "deleted": false, "deviceId": "", "data": map[string]any{"id": "c1", "name": "collection c1"}, "rev": 2, "updatedBy": by},
			map[string]any{"type": "request", "id": "r1", "updatedAt": now, "deleted": false, "deviceId": "01DEVICEA", "data": map[string]any{"id": "r1", "name": "request r1"}, "rev": 3, "updatedBy": by},
		},
		"cursor": 3, "hasMore": false, "syncEpoch": h.App.SyncEpoch(),
	})
	if h.App.SyncEpoch() == "" {
		t.Fatal("no sync epoch")
	}
	// updatedBy is the author's current name.
	h.MustExec(t, "UPDATE users SET name = 'Ada L.' WHERE id = ?", tk.User.ID)
	pull = h.SpacePull(t, tk.Access, space, "since=2")
	if !strings.Contains(string(pull.Body), `"updatedBy":{"userId":"`+tk.User.ID+`","name":"Ada L."}`) {
		t.Fatalf("renamed author: %s", pull.Body)
	}
	// Tombstones round-trip with an author too.
	h.SpacePush(t, tk.Access, space, tu.Tombstone("collection", "c1", now+1))
	pull = h.SpacePull(t, tk.Access, space, "since=3")
	tu.JSONEq(t, pull.Body, map[string]any{"changes": []any{map[string]any{"type": "collection", "id": "c1", "updatedAt": now + 1,
		"deleted": true, "deviceId": "", "data": nil, "rev": 4, "updatedBy": map[string]any{"userId": tk.User.ID, "name": "Ada L."}}},
		"cursor": 4, "hasMore": false, "syncEpoch": h.App.SyncEpoch()})
	if got := spaceGet(t, h, tk.Access, space); num(got["entityCount"]) != 1 {
		t.Fatalf("entityCount %v", got["entityCount"])
	}
}

func TestSpaces_LegacyResponsesCarryNoAccountFields(t *testing.T) {
	h := tu.New(t)
	ws := h.NewWorkspace(t, "")
	res := h.Call(t, "/v1/sync", tu.Req{Token: ws.Token, JSON: tu.Changes(tu.Change("request", "r1", 1, nil))})
	if res.StatusCode != 200 || strings.Contains(string(res.Body), "serverTime") {
		t.Fatalf("legacy push: %d %s", res.StatusCode, res.Body)
	}
	if n := h.Count(t, "entities", "workspace_id = ? AND updated_by = ''", ws.ID); n != 1 {
		t.Fatalf("legacy entity has an author: %d", n)
	}
	pull := h.Call(t, "/v1/sync", tu.Req{Token: ws.Token})
	if pull.StatusCode != 200 || strings.Contains(string(pull.Body), "updatedBy") {
		t.Fatalf("legacy pull: %s", pull.Body)
	}
	share := h.Call(t, "/v1/shares", tu.Req{Token: ws.Token, JSON: map[string]any{"kind": "collection", "entityId": "C1",
		"snapshot": tu.SampleCollectionSnapshot(), "options": map[string]any{"includeCredentials": true}}})
	if share.StatusCode != 201 || len(share.JSON(t)) != 4 {
		t.Fatalf("legacy share create: %d %s", share.StatusCode, share.Body)
	}
	list := h.Call(t, "/v1/shares", tu.Req{Token: ws.Token})
	if strings.Contains(string(list.Body), "createdBy") || strings.Contains(string(list.Body), "options") {
		t.Fatalf("legacy share list: %s", list.Body)
	}
	if n := h.Count(t, "shares", "workspace_id = ? AND created_by IS NULL AND options IS NULL", ws.ID); n != 1 {
		t.Fatalf("legacy share stored account fields: %d", n)
	}
}

// A push mixing a request and a future protoFile applies the request and rejects the other per
// change, with the quota counting only the request; the same push on legacy /v1/sync is a 400.
func TestSpaces_UnsupportedTypeIsRejectedPerChange(t *testing.T) {
	reqData := map[string]any{"id": "r1", "name": "request r1"}
	h := tu.New(t, tu.WithEnv("REQHARBOR_QUOTA_PERSONAL_BYTES=1000"))
	tk := h.CreateUser(t, "ada@example.test")
	space := h.PersonalSpace(t, tk.Access)
	proto := map[string]any{"type": "protoFile", "id": "p1", "updatedAt": 1, "deleted": false,
		"data": map[string]any{"id": "p1", "source": strings.Repeat("x", 5000)}}
	body := tu.Changes(tu.Change("request", "r1", 1, reqData), proto)

	res := h.Call(t, "/v1/spaces/"+space+"/sync", tu.Req{Token: tk.Access, JSON: body})
	tu.JSONEq(t, res.Body, map[string]any{
		"results": []any{
			map[string]any{"type": "request", "id": "r1", "status": "applied", "rev": 1},
			map[string]any{"type": "protoFile", "id": "p1", "status": "rejected", "reason": "unsupported_type"},
		},
		"cursor": 1, "serverTime": h.Clock.Now().UnixMilli(),
	})
	if got := spaceGet(t, h, tk.Access, space); num(got["bytesUsed"]) != sizeOf(t, reqData) || num(got["entityCount"]) != 1 {
		t.Fatalf("usage counts the rejected change: %v", got)
	}
	if n := h.Count(t, "entities", "type = 'protoFile'"); n != 0 {
		t.Fatalf("protoFile stored: %d", n)
	}
	// An unknown type still has to be a well-formed change.
	bad := map[string]any{"type": "protoFile", "id": "p1", "updatedAt": 1, "deleted": false, "data": "text"}
	if r := h.SpacePush(t, tk.Access, space, bad); r.StatusCode != 400 || r.ErrorCode(t) != "validation_failed" {
		t.Fatalf("malformed unknown type: %d %s", r.StatusCode, r.Body)
	}
	if r := h.SpacePush(t, tk.Access, space, map[string]any{"type": 7, "id": "x", "updatedAt": 1, "deleted": false, "data": map[string]any{}}); r.StatusCode != 400 || r.ErrorCode(t) != "unsupported_type" {
		t.Fatalf("non-string type: %d %s", r.StatusCode, r.Body)
	}

	ws := h.NewWorkspace(t, "")
	legacy := h.Call(t, "/v1/sync", tu.Req{Token: ws.Token, JSON: body})
	if legacy.StatusCode != 400 || legacy.ErrorCode(t) != "unsupported_type" {
		t.Fatalf("legacy: %d %s", legacy.StatusCode, legacy.Body)
	}
}

// Per-change clock_skew at +5 min + 1 ms while the rest applies; the rejected change takes no
// rev and does not count against the quota.
func TestSpaces_ClockSkewIsRejectedPerChange(t *testing.T) {
	h := tu.New(t, tu.WithEnv("REQHARBOR_QUOTA_PERSONAL_BYTES=200"))
	tk := h.CreateUser(t, "ada@example.test")
	space := h.PersonalSpace(t, tk.Access)
	now := h.Clock.Now().UnixMilli()
	limit := now + 5*60*1000
	big := map[string]any{"id": "big", "blob": strings.Repeat("x", 500)} // over the quota on its own

	res := h.SpacePush(t, tk.Access, space,
		tu.Change("request", "edge", limit, nil),
		tu.Change("request", "big", limit+1, big),
		tu.Tombstone("folder", "f1", limit+1),
		tu.Change("request", "ok", now, nil))
	tu.JSONEq(t, res.Body, map[string]any{
		"results": []any{
			map[string]any{"type": "request", "id": "edge", "status": "applied", "rev": 1},
			map[string]any{"type": "request", "id": "big", "status": "rejected", "reason": "clock_skew"},
			map[string]any{"type": "folder", "id": "f1", "status": "rejected", "reason": "clock_skew"},
			map[string]any{"type": "request", "id": "ok", "status": "applied", "rev": 2},
		},
		"cursor": 2, "serverTime": now,
	})
	// The bound follows the server clock.
	h.Clock.Advance(time.Millisecond)
	res = h.SpacePush(t, tk.Access, space, tu.Change("request", "late", limit+1, nil))
	if rs := results(t, res); rs[0]["status"] != "applied" {
		t.Fatalf("after the clock moved: %s", res.Body)
	}
}

func TestSpaces_PersonalQuota(t *testing.T) {
	h := tu.New(t, tu.WithEnv("REQHARBOR_QUOTA_PERSONAL_BYTES=1000"))
	tk := h.CreateUser(t, "ada@example.test")
	space := h.PersonalSpace(t, tk.Access)
	a := map[string]any{"blob": strings.Repeat("a", 800)}
	if rs := results(t, h.SpacePush(t, tk.Access, space, tu.Change("request", "a", 1, a))); rs[0]["status"] != "applied" {
		t.Fatal(rs)
	}
	over := h.SpacePush(t, tk.Access, space, tu.Change("request", "b", 1, map[string]any{"blob": strings.Repeat("b", 300)}))
	if over.StatusCode != 413 || over.ErrorCode(t) != "quota_exceeded" {
		t.Fatalf("over quota: %d %s", over.StatusCode, over.Body)
	}
	// Deletes and shrinking writes always pass.
	if r := h.SpacePush(t, tk.Access, space, tu.Change("request", "a", 2, map[string]any{"small": 1})); r.StatusCode != 200 {
		t.Fatalf("shrink: %d", r.StatusCode)
	}
	if r := h.SpacePush(t, tk.Access, space, tu.Tombstone("request", "a", 3)); r.StatusCode != 200 {
		t.Fatalf("delete: %d", r.StatusCode)
	}
	// Shares count against the same quota.
	snap := tu.SampleCollectionSnapshot()
	snap["collection"].(map[string]any)["description"] = strings.Repeat("d", 2000)
	sh := h.Call(t, "/v1/spaces/"+space+"/shares", tu.Req{Token: tk.Access, JSON: map[string]any{"kind": "collection", "entityId": "C1", "snapshot": snap}})
	if sh.StatusCode != 413 || sh.ErrorCode(t) != "quota_exceeded" {
		t.Fatalf("share over quota: %d %s", sh.StatusCode, sh.Body)
	}

	// 0 means unlimited: quotaBytes is null.
	h2 := tu.New(t, tu.WithEnv("REQHARBOR_QUOTA_PERSONAL_BYTES=0"))
	tk2 := h2.CreateUser(t, "bob@example.test")
	space2 := h2.PersonalSpace(t, tk2.Access)
	if got := spaceGet(t, h2, tk2.Access, space2); got["quotaBytes"] != nil {
		t.Fatalf("unlimited quota: %v", got["quotaBytes"])
	}
	if r := h2.SpacePush(t, tk2.Access, space2, tu.Change("request", "a", 1, map[string]any{"blob": strings.Repeat("a", 400_000)})); r.StatusCode != 200 {
		t.Fatalf("unlimited push: %d %s", r.StatusCode, r.Body)
	}
}

func TestSpaces_BreakerRefusesGrowingWritesButAllowsDeletes(t *testing.T) {
	h := tu.New(t)
	tk := h.CreateUser(t, "ada@example.test")
	space := h.PersonalSpace(t, tk.Access)
	results(t, h.SpacePush(t, tk.Access, space, tu.Change("request", "a", 1, map[string]any{"blob": strings.Repeat("a", 100)})))
	h.App.Breaker.SetLimits(jobs.BreakerLimits{MaxDBBytes: 1})
	if err := h.App.Breaker.Sample(context.Background()); err != nil {
		t.Fatal(err)
	}
	if r := h.SpacePush(t, tk.Access, space, tu.Change("request", "b", 1, nil)); r.StatusCode != 507 || r.ErrorCode(t) != "server_full" {
		t.Fatalf("grow: %d %s", r.StatusCode, r.Body)
	}
	if r := h.Call(t, "/v1/spaces/"+space+"/shares", tu.Req{Token: tk.Access, JSON: map[string]any{"kind": "collection", "entityId": "C1", "snapshot": tu.SampleCollectionSnapshot()}}); r.StatusCode != 507 {
		t.Fatalf("share: %d", r.StatusCode)
	}
	if r := h.SpacePush(t, tk.Access, space, tu.Change("request", "a", 2, map[string]any{"s": 1})); r.StatusCode != 200 {
		t.Fatalf("shrink: %d", r.StatusCode)
	}
	if r := h.SpacePush(t, tk.Access, space, tu.Tombstone("request", "a", 3)); r.StatusCode != 200 {
		t.Fatalf("delete: %d", r.StatusCode)
	}
	if r := h.SpacePull(t, tk.Access, space, ""); r.StatusCode != 200 {
		t.Fatalf("read: %d", r.StatusCode)
	}
	h.App.Breaker.SetLimits(jobs.BreakerLimits{})
	_ = h.App.Breaker.Sample(context.Background())
	if r := h.SpacePush(t, tk.Access, space, tu.Change("request", "b", 1, nil)); r.StatusCode != 200 {
		t.Fatalf("after closing: %d", r.StatusCode)
	}
}

// openSignup signs up without a code (REQHARBOR_SIGNUP=open).
func openSignup(t *testing.T, h *tu.Harness, email string) (*tu.Resp, tu.PKCE) {
	t.Helper()
	b := h.Browser()
	p := tu.NewPKCE()
	ar := b.Authorize(t, p)
	return b.Signup(t, ar, "Open", email, tu.DefaultPassword, ""), p
}

// REQHARBOR_OPEN_SIGNUP_TOTAL_BYTES: growing writes of open-signup accounts return 507 once the
// sampled total reaches the cap, invited and admin accounts are not counted, and open signup
// shows server_user_limit.
func TestSpaces_OpenSignupTotalBytesCap(t *testing.T) {
	h := tu.New(t, tu.WithEnv("REQHARBOR_SIGNUP=open", "REQHARBOR_OPEN_SIGNUP_TOTAL_BYTES=1000"))
	admin := h.CreateAdmin(t, "admin@example.test")
	res, p := openSignup(t, h, "open@example.test")
	open := tu.ParseTokens(t, h.Exchange(t, tu.LoopbackCode(t, res, p), p))
	invited := h.CreateUser(t, "invited@example.test")
	openSpace := h.PersonalSpace(t, open.Access)

	blob := map[string]any{"blob": strings.Repeat("o", 1200)}
	if rs := results(t, h.SpacePush(t, open.Access, openSpace, tu.Change("request", "a", 1, blob))); rs[0]["status"] != "applied" {
		t.Fatal(rs)
	}
	// Invited and admin usage never counts.
	for _, tk := range []tu.Tokens{admin, invited} {
		results(t, h.SpacePush(t, tk.Access, h.PersonalSpace(t, tk.Access), tu.Change("request", "x", 1, blob)))
	}
	if err := h.App.Breaker.Sample(context.Background()); err != nil {
		t.Fatal(err)
	}
	if !h.App.Breaker.OpenSignupFull() {
		t.Fatal("cap not reached")
	}
	if r := h.SpacePush(t, open.Access, openSpace, tu.Change("request", "b", 1, nil)); r.StatusCode != 507 || r.ErrorCode(t) != "server_full" {
		t.Fatalf("open account grows: %d %s", r.StatusCode, r.Body)
	}
	if r := h.Call(t, "/v1/spaces/"+openSpace+"/shares", tu.Req{Token: open.Access, JSON: map[string]any{"kind": "collection", "entityId": "C1", "snapshot": tu.SampleCollectionSnapshot()}}); r.StatusCode != 507 {
		t.Fatalf("open account share: %d", r.StatusCode)
	}
	if r := h.SpacePush(t, open.Access, openSpace, tu.Tombstone("request", "a", 2)); r.StatusCode != 200 {
		t.Fatalf("open account delete: %d", r.StatusCode)
	}
	for _, tk := range []tu.Tokens{admin, invited} {
		if r := h.SpacePush(t, tk.Access, h.PersonalSpace(t, tk.Access), tu.Change("request", "y", 1, blob)); r.StatusCode != 200 {
			t.Fatalf("%s grows: %d %s", tk.User.Email, r.StatusCode, r.Body)
		}
	}
	// Open signup pauses; an invite still works.
	res, _ = openSignup(t, h, "late@example.test")
	if res.StatusCode != 403 || !strings.Contains(string(res.Body), "isn&#39;t accepting new accounts") {
		t.Fatalf("open signup while full: %d %s", res.StatusCode, res.Body)
	}
	if n := h.Count(t, "users", "email = 'late@example.test'"); n != 0 {
		t.Fatal("account created while full")
	}
	h.CreateUser(t, "late-invited@example.test")
	// Once the open accounts free space, the next sample lifts both.
	_ = h.App.Breaker.Sample(context.Background())
	if h.App.Breaker.OpenSignupFull() {
		t.Fatal("still full after the delete")
	}
	if r := h.SpacePush(t, open.Access, openSpace, tu.Change("request", "b", 1, nil)); r.StatusCode != 200 {
		t.Fatalf("after freeing space: %d", r.StatusCode)
	}
}

func TestSpaces_Authorisation(t *testing.T) {
	h := tu.New(t)
	ada := h.CreateUser(t, "ada@example.test")
	bob := h.CreateUser(t, "bob@example.test")
	adaSpace := h.PersonalSpace(t, ada.Access)
	ws := h.NewWorkspace(t, "")

	cases := []struct {
		name, path, token, method string
		status                    int
		code                      string
	}{
		{"another user's space", "/v1/spaces/" + adaSpace, bob.Access, "GET", 403, "not_a_member"},
		{"another user's sync", "/v1/spaces/" + adaSpace + "/sync", bob.Access, "GET", 403, "not_a_member"},
		{"another user's push", "/v1/spaces/" + adaSpace + "/sync", bob.Access, "POST", 403, "not_a_member"},
		{"another user's shares", "/v1/spaces/" + adaSpace + "/shares", bob.Access, "GET", 403, "not_a_member"},
		{"unknown space", "/v1/spaces/01UNKNOWN0000000000000000X/sync", ada.Access, "GET", 404, "space_not_found"},
		{"legacy workspace id", "/v1/spaces/" + ws.ID + "/sync", ada.Access, "GET", 404, "space_not_found"},
		{"legacy workspace detail", "/v1/spaces/" + ws.ID, ada.Access, "GET", 404, "space_not_found"},
		{"rh_ token on a space", "/v1/spaces/" + adaSpace + "/sync", ws.Token, "GET", 401, "auth_required"},
		{"no token", "/v1/spaces/" + adaSpace + "/sync", "", "GET", 401, "auth_required"},
		{"refresh token as access", "/v1/spaces/" + adaSpace + "/sync", ada.Refresh, "GET", 401, "auth_required"},
		{"rha_ token on legacy sync", "/v1/sync", ada.Access, "GET", 401, "unauthorized"},
		{"rha_ token on legacy workspace", "/v1/workspace", ada.Access, "GET", 401, "unauthorized"},
		{"space sentinel on legacy", "/v1/sync", "space:" + adaSpace, "GET", 401, "unauthorized"},
	}
	for _, c := range cases {
		r := h.Call(t, c.path, tu.Req{Method: c.method, Token: c.token})
		if r.StatusCode != c.status || r.ErrorCode(t) != c.code {
			t.Errorf("%s: %d %s", c.name, r.StatusCode, r.Body)
		}
		if c.status == 401 && r.Header.Get("WWW-Authenticate") == "" {
			t.Errorf("%s: no WWW-Authenticate", c.name)
		}
	}
	// Authorisation comes before the body: a 403 even for a body that would be a 415.
	r := h.Call(t, "/v1/spaces/"+adaSpace+"/sync", tu.Req{Method: "POST", Token: bob.Access, Raw: tu.Str("x"), Headers: map[string]string{"Content-Type": "text/plain"}})
	if r.StatusCode != 403 {
		t.Fatalf("authz before body: %d", r.StatusCode)
	}
	// The sentinel hash can never be found by the legacy lookup.
	err := h.Store.Read(context.Background(), func(tx store.ReadTx) error {
		_, err := tx.WorkspaceByTokenHash("space:" + adaSpace)
		return err
	})
	if !errors.Is(err, store.ErrNotFound) {
		t.Fatalf("sentinel lookup: %v", err)
	}
	// Bob can still use his own space.
	if r := h.SpacePull(t, bob.Access, h.PersonalSpace(t, bob.Access), ""); r.StatusCode != 200 {
		t.Fatalf("own space: %d", r.StatusCode)
	}
}

func TestSpaces_ExpiredAccessAndDisabledUser(t *testing.T) {
	h := tu.New(t)
	tk := h.CreateUser(t, "ada@example.test")
	space := h.PersonalSpace(t, tk.Access)
	h.Clock.Advance(time.Hour)
	for _, path := range []string{"/v1/spaces/" + space + "/sync", "/v1/me"} {
		r := h.Call(t, path, tu.Req{Token: tk.Access})
		if r.StatusCode != 401 || r.ErrorCode(t) != "session_expired" || !strings.Contains(r.Header.Get("WWW-Authenticate"), "invalid_token") {
			t.Fatalf("%s expired: %d %s", path, r.StatusCode, r.Body)
		}
	}
	fresh := tu.ParseTokens(t, h.Refresh(t, tk.Refresh))
	if r := h.SpacePull(t, fresh.Access, space, ""); r.StatusCode != 200 {
		t.Fatalf("after refresh: %d", r.StatusCode)
	}
	h.MustExec(t, "UPDATE users SET disabled_at = 1 WHERE id = ?", tk.User.ID)
	if r := h.SpacePull(t, fresh.Access, space, ""); r.StatusCode != 401 || r.ErrorCode(t) != "auth_required" {
		t.Fatalf("disabled: %d %s", r.StatusCode, r.Body)
	}
}

// REQHARBOR_MIN_CLIENT_VERSION gates /v1/spaces/* (before auth) and never the legacy routes or
// /v1/me.
func TestSpaces_VersionGate(t *testing.T) {
	h := tu.New(t, tu.WithEnv("REQHARBOR_MIN_CLIENT_VERSION=1.2.0"))
	h.ClientVersion = "1.2.0"
	tk := h.CreateUser(t, "ada@example.test")
	space := h.PersonalSpace(t, tk.Access)
	path := "/v1/spaces/" + space + "/sync"
	for _, c := range []struct {
		version, token string
		status         int
	}{
		{"", tk.Access, 426},
		{"1.1.9", tk.Access, 426},
		{"1.2.0-beta.1", tk.Access, 426},
		{"", "", 426}, // before auth
		{"1.2.0", tk.Access, 200},
		{"1.10.0", tk.Access, 200},
		{"1.2.0", "", 401},
	} {
		headers := map[string]string{}
		if c.version != "" {
			headers["X-ReqHarbor-Version"] = c.version
		}
		r := h.Call(t, path, tu.Req{Token: c.token, Headers: headers})
		if r.StatusCode != c.status {
			t.Errorf("version %q token %v: %d %s", c.version, c.token != "", r.StatusCode, r.Body)
		}
		if c.status == 426 && (r.ErrorCode(t) != "client_too_old" || !strings.Contains(string(r.Body), "1.2.0")) {
			t.Errorf("426 body: %s", r.Body)
		}
	}
	for _, p := range []string{"/v1/spaces/" + space, "/v1/spaces/" + space + "/shares"} {
		if r := h.Call(t, p, tu.Req{Token: tk.Access}); r.StatusCode != 426 {
			t.Errorf("%s: %d", p, r.StatusCode)
		}
	}
	// Not gated: /v1/me*, and every legacy route.
	for _, p := range []string{"/v1/me", "/v1/me/spaces", "/v1/me/sessions"} {
		if r := h.Call(t, p, tu.Req{Token: tk.Access}); r.StatusCode != 200 {
			t.Errorf("%s gated: %d", p, r.StatusCode)
		}
	}
	ws := h.NewWorkspace(t, "") // POST /v1/workspaces without the header
	if r := h.Call(t, "/v1/sync", tu.Req{Token: ws.Token}); r.StatusCode != 200 {
		t.Fatalf("legacy gated: %d", r.StatusCode)
	}
	if r := h.Call(t, "/v1/sync", tu.Req{Token: ws.Token, JSON: tu.Changes(tu.Change("request", "a", 1, nil))}); r.StatusCode != 200 {
		t.Fatalf("legacy push gated: %d", r.StatusCode)
	}
}

func TestSpaces_SyncRateLimitPerSession(t *testing.T) {
	h := tu.New(t, tu.WithEnv("REQHARBOR_RATE_SYNC_PER_MIN=2"))
	first := h.CreateUser(t, "ada@example.test")
	second := h.SignIn(t, "ada@example.test", tu.DefaultPassword)
	space := h.PersonalSpace(t, first.Access)
	for i := 0; i < 2; i++ {
		if r := h.SpacePull(t, first.Access, space, ""); r.StatusCode != 200 {
			t.Fatalf("pull %d: %d", i, r.StatusCode)
		}
	}
	r := h.SpacePush(t, first.Access, space, tu.Change("request", "a", 1, nil))
	if r.StatusCode != 429 || r.ErrorCode(t) != "rate_limited" || r.Header.Get("Retry-After") == "" {
		t.Fatalf("third call: %d %s", r.StatusCode, r.Body)
	}
	if r := h.SpacePull(t, second.Access, space, ""); r.StatusCode != 200 {
		t.Fatalf("other session: %d", r.StatusCode)
	}
	if r := h.Call(t, "/v1/spaces/"+space, tu.Req{Token: first.Access}); r.StatusCode != 200 {
		t.Fatalf("non-sync route limited: %d", r.StatusCode)
	}
	h.Clock.Advance(time.Minute)
	if r := h.SpacePull(t, first.Access, space, ""); r.StatusCode != 200 {
		t.Fatalf("after a minute: %d", r.StatusCode)
	}
}

func TestSpaces_SharesCarryCreatedByAndOptions(t *testing.T) {
	h := tu.New(t)
	ada := h.CreateUser(t, "ada@example.test")
	bob := h.CreateUser(t, "bob@example.test")
	space := h.PersonalSpace(t, ada.Access)
	bobSpace := h.PersonalSpace(t, bob.Access)
	base := "/v1/spaces/" + space + "/shares"
	snap := tu.SampleCollectionSnapshot()

	res := h.Call(t, base, tu.Req{Token: ada.Access, JSON: map[string]any{"kind": "collection", "entityId": "C1", "snapshot": snap,
		"options": map[string]any{"includeCredentials": true, "environmentId": "ENV1", "future": 1}}})
	if res.StatusCode != 201 {
		t.Fatalf("create: %d %s", res.StatusCode, res.Body)
	}
	c := res.JSON(t)
	id, _ := c["shareId"].(string)
	now := h.Clock.Now().UnixMilli()
	by := map[string]any{"userId": ada.User.ID, "name": "User"}
	tu.JSONEq(t, res.Body, map[string]any{"shareId": id, "url": "https://reqharbor.test/s/" + id, "createdAt": now, "updatedAt": now,
		"createdBy": by, "options": map[string]any{"includeCredentials": true, "environmentId": "ENV1"}})
	if !strings.Contains(string(res.Body), `"options":{"includeCredentials":true,"environmentId":"ENV1"}`) {
		t.Fatalf("options not canonical: %s", res.Body)
	}
	// Without options: null.
	h.Clock.Advance(time.Second)
	res = h.Call(t, base, tu.Req{Token: ada.Access, JSON: map[string]any{"kind": "request", "entityId": "R1", "snapshot": map[string]any{
		"schemaVersion": 1, "request": tu.SampleRequest(nil), "environment": nil}}})
	if res.StatusCode != 201 || res.JSON(t)["options"] != nil {
		t.Fatalf("create without options: %d %s", res.StatusCode, res.Body)
	}
	reqShare, _ := res.JSON(t)["shareId"].(string)

	list := h.Call(t, base, tu.Req{Token: ada.Access})
	tu.JSONEq(t, list.Body, map[string]any{"shares": []any{
		map[string]any{"shareId": reqShare, "url": "https://reqharbor.test/s/" + reqShare, "kind": "request", "entityId": "R1", "name": "List users",
			"createdAt": now + 1000, "updatedAt": now + 1000, "createdBy": by, "options": nil},
		map[string]any{"shareId": id, "url": "https://reqharbor.test/s/" + id, "kind": "collection", "entityId": "C1", "name": "Users API",
			"createdAt": now, "updatedAt": now, "createdBy": by, "options": map[string]any{"includeCredentials": true, "environmentId": "ENV1"}},
	}})
	if n := h.Count(t, "shares", "id = ? AND workspace_id = ? AND created_by = ?", id, space, ada.User.ID); n != 1 {
		t.Fatalf("stored created_by: %d", n)
	}

	// Update: options absent keeps them, an object replaces them.
	h.Clock.Advance(time.Second)
	res = h.Call(t, base+"/"+id, tu.Req{Method: "PUT", Token: ada.Access, JSON: map[string]any{"snapshot": snap}})
	if res.StatusCode != 200 || !strings.Contains(string(res.Body), `"options":{"includeCredentials":true,"environmentId":"ENV1"}`) ||
		num(res.JSON(t)["updatedAt"]) != now+2000 {
		t.Fatalf("update keeps options: %d %s", res.StatusCode, res.Body)
	}
	res = h.Call(t, base+"/"+id, tu.Req{Method: "PUT", Token: ada.Access, JSON: map[string]any{"snapshot": snap, "options": map[string]any{"environmentId": nil}}})
	if res.StatusCode != 200 || !strings.Contains(string(res.Body), `"createdBy":{"userId":"`+ada.User.ID+`","name":"User"},"options":{"includeCredentials":false,"environmentId":null}`) {
		t.Fatalf("update replaces options: %d %s", res.StatusCode, res.Body)
	}
	// Invalid options are a 400, on create and update.
	for _, opts := range []any{"yes", map[string]any{"includeCredentials": "true"}, map[string]any{"environmentId": "bad id"}} {
		r := h.Call(t, base, tu.Req{Token: ada.Access, JSON: map[string]any{"kind": "collection", "entityId": "C2", "snapshot": snap, "options": opts}})
		if r.StatusCode != 400 || r.ErrorCode(t) != "validation_failed" {
			t.Errorf("create options %v: %d %s", opts, r.StatusCode, r.Body)
		}
		r = h.Call(t, base+"/"+id, tu.Req{Method: "PUT", Token: ada.Access, JSON: map[string]any{"snapshot": snap, "options": opts}})
		if r.StatusCode != 400 {
			t.Errorf("update options %v: %d", opts, r.StatusCode)
		}
	}
	// The public page serves a space share like any other.
	if r := h.Call(t, "/s/"+id+".json", tu.Req{}); r.StatusCode != 200 {
		t.Fatalf("public json: %d", r.StatusCode)
	}

	// Another space's share through this space is a 404 (before the body); a non-member is a 403.
	if r := h.Call(t, "/v1/spaces/"+bobSpace+"/shares/"+id, tu.Req{Method: "PUT", Token: bob.Access, Raw: tu.Str(strings.Repeat("x", 100)), Headers: map[string]string{"Content-Type": "text/plain"}}); r.StatusCode != 404 {
		t.Fatalf("other space's share: %d %s", r.StatusCode, r.Body)
	}
	if r := h.Call(t, "/v1/spaces/"+bobSpace+"/shares/"+id, tu.Req{Method: "DELETE", Token: bob.Access}); r.StatusCode != 404 {
		t.Fatalf("revoke other space's share: %d", r.StatusCode)
	}
	if r := h.Call(t, base+"/"+id, tu.Req{Method: "DELETE", Token: bob.Access}); r.StatusCode != 403 {
		t.Fatalf("non-member revoke: %d", r.StatusCode)
	}
	if r := h.Call(t, base+"/not-a-share-id!", tu.Req{Method: "DELETE", Token: ada.Access}); r.StatusCode != 404 || r.ErrorCode(t) != "not_found" {
		t.Fatalf("bad share path: %d", r.StatusCode)
	}

	// Revoke never reads a body; then PUT is 410 before the body.
	res = h.Call(t, base+"/"+id, tu.Req{Method: "DELETE", Token: ada.Access, Raw: tu.Str("garbage")})
	if res.StatusCode != 200 || res.JSON(t)["shareId"] != id {
		t.Fatalf("revoke: %d %s", res.StatusCode, res.Body)
	}
	if r := h.Call(t, base+"/"+id, tu.Req{Method: "PUT", Token: ada.Access, Raw: tu.Str("x"), Headers: map[string]string{"Content-Type": "text/plain"}}); r.StatusCode != 410 {
		t.Fatalf("update revoked: %d", r.StatusCode)
	}
	if r := h.Call(t, "/s/"+id+".json", tu.Req{}); r.StatusCode != 410 {
		t.Fatalf("public json after revoke: %d", r.StatusCode)
	}
	list = h.Call(t, base, tu.Req{Token: ada.Access})
	if shares, _ := list.JSON(t)["shares"].([]any); len(shares) != 1 {
		t.Fatalf("list after revoke: %s", list.Body)
	}
	// A space share is not visible through a legacy token, and the reverse.
	ws := h.NewWorkspace(t, "")
	if r := h.Call(t, "/v1/shares/"+reqShare, tu.Req{Method: "DELETE", Token: ws.Token}); r.StatusCode != 404 {
		t.Fatalf("legacy token on a space share: %d", r.StatusCode)
	}
	if r := h.Call(t, "/v1/spaces/"+space+"/shares", tu.Req{Method: "PATCH", Token: ada.Access}); r.StatusCode != http.StatusMethodNotAllowed || r.Header.Get("Allow") != "GET, POST" {
		t.Fatalf("405: %d %q", r.StatusCode, r.Header.Get("Allow"))
	}
}

func TestSpaces_ShareCreateLimitPerSpace(t *testing.T) {
	h := tu.New(t, tu.WithEnv("REQHARBOR_RATE_SHARE_CREATE_PER_HOUR=1"))
	ada := h.CreateUser(t, "ada@example.test")
	space := h.PersonalSpace(t, ada.Access)
	body := map[string]any{"kind": "collection", "entityId": "C1", "snapshot": tu.SampleCollectionSnapshot()}
	if r := h.Call(t, "/v1/spaces/"+space+"/shares", tu.Req{Token: ada.Access, JSON: map[string]any{"kind": "nope"}}); r.StatusCode != 400 {
		t.Fatalf("invalid: %d", r.StatusCode)
	}
	if r := h.Call(t, "/v1/spaces/"+space+"/shares", tu.Req{Token: ada.Access, JSON: body}); r.StatusCode != 201 {
		t.Fatalf("first: %d %s", r.StatusCode, r.Body)
	}
	if r := h.Call(t, "/v1/spaces/"+space+"/shares", tu.Req{Token: ada.Access, JSON: body}); r.StatusCode != 429 {
		t.Fatalf("second: %d", r.StatusCode)
	}
}
