package app_test

// M2 teams (06-go-server.md §10 "M2 outline"; 04-tech-design.md §5.3–§5.4 and §12 M2a; the
// 05-qa-review.md "Teams and roles", "Invites" and "IDOR" lists): /v1/teams*, /v1/invites/*, /i,
// and role enforcement in team-space sync and shares.

import (
	"context"
	"fmt"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/mihajlopopara/reqharbor/server/internal/ids"
	"github.com/mihajlopopara/reqharbor/server/internal/jobs"
	"github.com/mihajlopopara/reqharbor/server/internal/roles"
	"github.com/mihajlopopara/reqharbor/server/internal/store"
	tu "github.com/mihajlopopara/reqharbor/server/internal/testutil"
	"github.com/mihajlopopara/reqharbor/server/internal/web"
)

func teamsHarness(t *testing.T, env ...string) *tu.Harness {
	t.Helper()
	return tu.New(t, tu.WithEnv(append([]string{"REQHARBOR_TEAMS=true"}, env...)...))
}

// want fails unless res has the status and, when code is set, the error code.
func want(t *testing.T, res *tu.Resp, status int, code string) {
	t.Helper()
	if res.StatusCode != status || (code != "" && res.ErrorCode(t) != code) {
		t.Fatalf("want %d %s, got %d %s", status, code, res.StatusCode, res.Body)
	}
}

type team struct{ ID, Space string }

func createTeam(t *testing.T, h *tu.Harness, access, name string) team {
	t.Helper()
	res := h.API(t, "POST", "/v1/teams", access, map[string]any{"name": name})
	want(t, res, 201, "")
	v := res.JSON(t)
	return team{ID: v["teamId"].(string), Space: v["spaceId"].(string)}
}

// addMember inserts a membership directly (fast set-up for the matrices).
func addMember(t *testing.T, h *tu.Harness, teamID, userID, role string) {
	t.Helper()
	h.MustExec(t, "INSERT INTO team_members (team_id, user_id, role, joined_at) VALUES (?, ?, ?, ?)", teamID, userID, role, h.Clock.Now().UnixMilli())
}

func roleOf(t *testing.T, h *tu.Harness, teamID, userID string) string {
	t.Helper()
	var r string
	if err := h.DB.Reader.QueryRow("SELECT role FROM team_members WHERE team_id = ? AND user_id = ?", teamID, userID).Scan(&r); err != nil {
		return ""
	}
	return r
}

func newInvite(t *testing.T, h *tu.Harness, access, teamID string, body map[string]any) (token, inviteID string) {
	t.Helper()
	if body == nil {
		body = map[string]any{}
	}
	res := h.API(t, "POST", "/v1/teams/"+teamID+"/invites", access, body)
	want(t, res, 201, "")
	v := res.JSON(t)
	return v["token"].(string), v["inviteId"].(string)
}

func acceptInvite(t *testing.T, h *tu.Harness, access, token string) *tu.Resp {
	t.Helper()
	return h.API(t, "POST", "/v1/invites/accept", access, map[string]any{"token": token})
}

func tick(h *tu.Harness) { h.Clock.Advance(time.Millisecond) }

// ---------------------------------------------------------------- gate and basics

func TestTeams_OffByDefaultAndReportedInDiscovery(t *testing.T) {
	h := tu.New(t)
	tk := h.QuickUser(t, "ada@example.test", "Ada")
	for _, c := range []struct{ method, path string }{
		{"GET", "/v1/teams"}, {"POST", "/v1/teams"}, {"GET", "/v1/teams/x"}, {"POST", "/v1/teams/x/leave"},
		{"POST", "/v1/invites/preview"}, {"POST", "/v1/invites/accept"}, {"POST", "/v1/invites/ticket"},
	} {
		// The switch comes before auth and before the body.
		want(t, h.Call(t, c.path, tu.Req{Method: c.method}), 403, "teams_disabled")
		want(t, h.API(t, c.method, c.path, tk.Access, map[string]any{"name": "Acme"}), 403, "teams_disabled")
	}
	if v := h.Call(t, "/v1/server", tu.Req{}).JSON(t); v["features"].(map[string]any)["teams"] != false {
		t.Fatalf("features %v", v["features"])
	}
	if r := h.Call(t, "/i", tu.Req{}); r.StatusCode != 200 {
		t.Fatalf("/i with teams off: %d", r.StatusCode)
	}

	on := teamsHarness(t)
	if v := on.Call(t, "/v1/server", tu.Req{}).JSON(t); v["features"].(map[string]any)["teams"] != true {
		t.Fatalf("features %v", v["features"])
	}
	want(t, on.Call(t, "/v1/teams", tu.Req{}), 401, "auth_required")
}

func TestTeams_CreateListGetRenameAndTheTeamSpace(t *testing.T) {
	h := teamsHarness(t)
	owner := h.QuickUser(t, "owner@example.test", "Olu")
	now := h.Clock.Now().UnixMilli()

	res := h.API(t, "POST", "/v1/teams", owner.Access, map[string]any{"name": "  Acme  "})
	want(t, res, 201, "")
	v := res.JSON(t)
	tm := team{ID: v["teamId"].(string), Space: v["spaceId"].(string)}
	tu.JSONEq(t, res.Body, map[string]any{"teamId": tm.ID, "name": "Acme", "spaceId": tm.Space, "role": "owner",
		"ownerUserId": owner.User.ID, "memberCount": 1, "bytesUsed": 0, "quotaBytes": 262144000, "createdAt": now})
	if n := h.Count(t, "workspaces", "id = ? AND team_id = ? AND name = 'Acme' AND token_hash = 'space:' || id", tm.Space, tm.ID); n != 1 {
		t.Fatalf("team space row: %d", n)
	}

	res = h.API(t, "GET", "/v1/teams", owner.Access, nil)
	want(t, res, 200, "")
	tu.JSONEq(t, res.Body, map[string]any{
		"teams": []any{map[string]any{"teamId": tm.ID, "name": "Acme", "spaceId": tm.Space, "role": "owner",
			"ownerUserId": owner.User.ID, "memberCount": 1, "bytesUsed": 0, "quotaBytes": 262144000, "createdAt": now}},
		"ownedCount": 1,
		"limits":     map[string]any{"ownedTeams": 10, "teamMembers": 100, "teamInvites": 20},
		"owner":      map[string]any{"bytesUsed": 0, "quotaBytes": nil},
	})
	want(t, h.API(t, "GET", "/v1/teams/"+tm.ID, owner.Access, nil), 200, "")

	// Names: 1–80 UTF-16 code units after trimming, no control characters.
	for _, bad := range []any{"", "   ", strings.Repeat("a", 81), strings.Repeat("😀", 41), "a\nb", 5, nil} {
		want(t, h.API(t, "POST", "/v1/teams", owner.Access, map[string]any{"name": bad}), 400, "validation_failed")
	}
	want(t, h.API(t, "PATCH", "/v1/teams/"+tm.ID, owner.Access, map[string]any{"name": strings.Repeat("😀", 40)}), 200, "")
	res = h.API(t, "PATCH", "/v1/teams/"+tm.ID, owner.Access, map[string]any{"name": "Acme Inc"})
	want(t, res, 200, "")
	if res.JSON(t)["name"] != "Acme Inc" {
		t.Fatalf("rename: %s", res.Body)
	}

	// Rename needs admin; any member reads.
	ed := h.QuickUser(t, "ed@example.test", "Ed")
	addMember(t, h, tm.ID, ed.User.ID, roles.Editor)
	want(t, h.API(t, "PATCH", "/v1/teams/"+tm.ID, ed.Access, map[string]any{"name": "Mine"}), 403, "forbidden_role")
	adm := h.QuickUser(t, "adm@example.test", "Adm")
	addMember(t, h, tm.ID, adm.User.ID, roles.Admin)
	want(t, h.API(t, "PATCH", "/v1/teams/"+tm.ID, adm.Access, map[string]any{"name": "Acme"}), 200, "")
	res = h.API(t, "GET", "/v1/teams/"+tm.ID+"/members", ed.Access, nil)
	want(t, res, 200, "")
	tu.JSONEq(t, res.Body, map[string]any{"members": []any{
		map[string]any{"userId": owner.User.ID, "name": "Olu", "email": "owner@example.test", "avatarUrl": nil, "role": "owner", "joinedAt": now, "isBillingOwner": true},
		map[string]any{"userId": adm.User.ID, "name": "Adm", "email": "adm@example.test", "avatarUrl": nil, "role": "admin", "joinedAt": now, "isBillingOwner": false},
		map[string]any{"userId": ed.User.ID, "name": "Ed", "email": "ed@example.test", "avatarUrl": nil, "role": "editor", "joinedAt": now, "isBillingOwner": false},
	}})

	// The team space is in every member's /v1/me/spaces, after the personal space.
	res = h.API(t, "GET", "/v1/me/spaces", ed.Access, nil)
	spaces := res.JSON(t)["spaces"].([]any)
	if len(spaces) != 2 {
		t.Fatalf("spaces: %s", res.Body)
	}
	tu.JSONEq(t, spaces[1], map[string]any{"spaceId": tm.Space, "kind": "team", "teamId": tm.ID, "teamName": "Acme", "role": "editor",
		"bytesUsed": 0, "quotaBytes": 262144000, "memberCount": 3, "ownerUserId": owner.User.ID})

	// GET /v1/spaces/{id} of a team space: kind team, the caller's role; the fixed team-space
	// version gate needs X-ReqHarbor-Version (426 without), personal spaces do not.
	res = h.API(t, "GET", "/v1/spaces/"+tm.Space, ed.Access, nil)
	want(t, res, 200, "")
	if v := res.JSON(t); v["kind"] != "team" || v["role"] != "editor" || v["name"] != "Acme" {
		t.Fatalf("space: %s", res.Body)
	}
	for _, p := range []string{"/v1/spaces/" + tm.Space, "/v1/spaces/" + tm.Space + "/sync", "/v1/spaces/" + tm.Space + "/shares"} {
		want(t, h.Call(t, p, tu.Req{Token: ed.Access}), 426, "client_too_old")
	}
	if r := h.Call(t, "/v1/spaces/"+h.PersonalSpace(t, ed.Access), tu.Req{Token: ed.Access}); r.StatusCode != 200 {
		t.Fatalf("personal space without the header: %d", r.StatusCode)
	}

	// Personal teams are hidden; a team the caller is not in is 403.
	var personalTeam string
	if err := h.DB.Reader.QueryRow("SELECT id FROM teams WHERE owner_user_id = ? AND personal = 1", owner.User.ID).Scan(&personalTeam); err != nil {
		t.Fatal(err)
	}
	want(t, h.API(t, "GET", "/v1/teams/"+personalTeam, owner.Access, nil), 404, "team_not_found")
	want(t, h.API(t, "PATCH", "/v1/teams/"+personalTeam, owner.Access, map[string]any{"name": "x"}), 404, "team_not_found")
	want(t, h.API(t, "GET", "/v1/teams/01UNKNOWN", owner.Access, nil), 404, "team_not_found")
	stranger := h.QuickUser(t, "stranger@example.test", "S")
	want(t, h.API(t, "GET", "/v1/teams/"+tm.ID, stranger.Access, nil), 403, "not_a_member")
	if r := h.API(t, "GET", "/v1/teams", stranger.Access, nil); !strings.Contains(string(r.Body), `"teams":[]`) {
		t.Fatalf("stranger list: %s", r.Body)
	}
}

// QA: the owned-team count uses personal = 0, and transfer checks the receiver's count.
func TestTeams_OwnedTeamLimitExcludesPersonalAndAppliesToTransfer(t *testing.T) {
	h := teamsHarness(t, "REQHARBOR_MAX_OWNED_TEAMS=2")
	a := h.QuickUser(t, "a@example.test", "A")
	createTeam(t, h, a.Access, "One")
	createTeam(t, h, a.Access, "Two")
	res := h.API(t, "POST", "/v1/teams", a.Access, map[string]any{"name": "Three"})
	want(t, res, 409, "limit_teams")
	if !strings.Contains(string(res.Body), "up to 2 teams") {
		t.Fatalf("message: %s", res.Body)
	}

	b := h.QuickUser(t, "b@example.test", "Bea")
	tb := createTeam(t, h, b.Access, "B's")
	addMember(t, h, tb.ID, a.User.ID, roles.Editor)
	res = h.API(t, "POST", "/v1/teams/"+tb.ID+"/transfer", b.Access, map[string]any{"userId": a.User.ID})
	want(t, res, 409, "limit_teams")
	if !strings.Contains(string(res.Body), "A already owns 2 teams") {
		t.Fatalf("message: %s", res.Body)
	}
	if roleOf(t, h, tb.ID, b.User.ID) != "owner" || roleOf(t, h, tb.ID, a.User.ID) != "editor" {
		t.Fatal("a refused transfer changed roles")
	}
}

// ---------------------------------------------------------------- role matrices

// The role-change rule, stated from the product role table (02 §6.6) rather than from the code:
// owners give anyone any role; admins change only editors and viewers and grant up to admin;
// anyone may lower their own role.
func expectRoleChange(actor, target, requested string, self bool) bool {
	rank := map[string]int{"viewer": 1, "editor": 2, "admin": 3, "owner": 4}
	if actor == "owner" {
		return true
	}
	if self && rank[requested] < rank[target] {
		return true
	}
	return !self && actor == "admin" && (target == "editor" || target == "viewer") && requested != "owner"
}

var allRoles = []string{"owner", "admin", "editor", "viewer"}

func TestTeams_RoleChangeMatrix(t *testing.T) {
	h := teamsHarness(t, "REQHARBOR_MAX_OWNED_TEAMS=0")
	billing := h.QuickUser(t, "billing@example.test", "Billing")
	actor := h.QuickUser(t, "actor@example.test", "Actor")
	targetU := h.QuickUser(t, "target@example.test", "Target")
	n := 0
	for _, ra := range allRoles {
		for _, rt := range allRoles {
			for _, rn := range allRoles {
				n++
				tm := createTeam(t, h, billing.Access, fmt.Sprintf("M%d", n))
				addMember(t, h, tm.ID, actor.User.ID, ra)
				addMember(t, h, tm.ID, targetU.User.ID, rt)
				ok := expectRoleChange(ra, rt, rn, false)
				res := h.API(t, "PATCH", "/v1/teams/"+tm.ID+"/members/"+targetU.User.ID, actor.Access, map[string]any{"role": rn})
				got := roleOf(t, h, tm.ID, targetU.User.ID)
				if ok {
					if res.StatusCode != 200 || got != rn || res.JSON(t)["role"] != rn {
						t.Errorf("%s sets %s → %s: %d %s (role now %s)", ra, rt, rn, res.StatusCode, res.Body, got)
					}
				} else if res.StatusCode != 403 || res.ErrorCode(t) != "forbidden_role" || got != rt {
					t.Errorf("%s sets %s → %s: want 403, got %d %s (role now %s)", ra, rt, rn, res.StatusCode, res.Body, got)
				}
			}
		}
	}
	// Self-changes: anyone may lower their own role; only owners may raise it (an owner is
	// already at the top). Owners here are co-owners, never the billing owner.
	for _, r0 := range allRoles {
		for _, rn := range allRoles {
			n++
			tm := createTeam(t, h, billing.Access, fmt.Sprintf("S%d", n))
			addMember(t, h, tm.ID, actor.User.ID, r0)
			ok := expectRoleChange(r0, r0, rn, true)
			res := h.API(t, "PATCH", "/v1/teams/"+tm.ID+"/members/"+actor.User.ID, actor.Access, map[string]any{"role": rn})
			got := roleOf(t, h, tm.ID, actor.User.ID)
			if ok && (res.StatusCode != 200 || got != rn) || !ok && (res.StatusCode != 403 || got != r0) {
				t.Errorf("self %s → %s: ok=%v, got %d %s (role now %s)", r0, rn, ok, res.StatusCode, res.Body, got)
			}
		}
	}
	// Validation and unknown members.
	tm := createTeam(t, h, billing.Access, "V")
	want(t, h.API(t, "PATCH", "/v1/teams/"+tm.ID+"/members/"+billing.User.ID, billing.Access, map[string]any{"role": "god"}), 400, "validation_failed")
	want(t, h.API(t, "PATCH", "/v1/teams/"+tm.ID+"/members/"+targetU.User.ID, billing.Access, map[string]any{"role": "viewer"}), 404, "member_not_found")
}

func TestTeams_RemoveMatrix(t *testing.T) {
	h := teamsHarness(t, "REQHARBOR_MAX_OWNED_TEAMS=0")
	billing := h.QuickUser(t, "billing@example.test", "Billing")
	actor := h.QuickUser(t, "actor@example.test", "Actor")
	targetU := h.QuickUser(t, "target@example.test", "Target")
	for i, ra := range allRoles {
		for j, rt := range allRoles {
			tm := createTeam(t, h, billing.Access, fmt.Sprintf("R%d%d", i, j))
			addMember(t, h, tm.ID, actor.User.ID, ra)
			addMember(t, h, tm.ID, targetU.User.ID, rt)
			ok := ra == "owner" || (ra == "admin" && (rt == "editor" || rt == "viewer"))
			tick(h)
			res := h.API(t, "DELETE", "/v1/teams/"+tm.ID+"/members/"+targetU.User.ID, actor.Access, nil)
			gone := roleOf(t, h, tm.ID, targetU.User.ID) == ""
			departures := h.Count(t, "team_departures", "team_id = ? AND user_id = ? AND reason = 'removed' AND by_user_id = ? AND space_id = ?",
				tm.ID, targetU.User.ID, actor.User.ID, tm.Space)
			if ok {
				if res.StatusCode != 200 || !gone || departures != 1 {
					t.Errorf("%s removes %s: %d %s (gone %v, departures %d)", ra, rt, res.StatusCode, res.Body, gone, departures)
				}
				tu.JSONEq(t, res.Body, map[string]any{"userId": targetU.User.ID, "revokedInvites": 0})
			} else if res.StatusCode != 403 || res.ErrorCode(t) != "forbidden_role" || gone || departures != 0 {
				t.Errorf("%s removes %s: want 403, got %d %s", ra, rt, res.StatusCode, res.Body)
			}
		}
	}
	tm := createTeam(t, h, billing.Access, "X")
	want(t, h.API(t, "DELETE", "/v1/teams/"+tm.ID+"/members/"+targetU.User.ID, billing.Access, nil), 404, "member_not_found")
	// Removing yourself is leave.
	want(t, h.API(t, "DELETE", "/v1/teams/"+tm.ID+"/members/"+billing.User.ID, billing.Access, nil), 403, "forbidden_role")
	addMember(t, h, tm.ID, targetU.User.ID, "viewer")
	want(t, h.API(t, "DELETE", "/v1/teams/"+tm.ID+"/members/"+targetU.User.ID, billing.Access, map[string]any{"revokeInvites": "yes"}), 400, "validation_failed")
}

// DEV-8 and AC-TEAM-3: the billing owner can't be demoted or removed without a transfer, and a
// team always keeps an owner.
func TestTeams_BillingOwnerNeedsTransferAndLastOwnerStays(t *testing.T) {
	h := teamsHarness(t)
	b := h.QuickUser(t, "b@example.test", "Bea")
	c := h.QuickUser(t, "c@example.test", "Cy")
	tm := createTeam(t, h, b.Access, "Acme")
	path := "/v1/teams/" + tm.ID

	// Sole owner: last_owner for demoting yourself and for leaving.
	want(t, h.API(t, "PATCH", path+"/members/"+b.User.ID, b.Access, map[string]any{"role": "admin"}), 409, "last_owner")
	want(t, h.API(t, "POST", path+"/leave", b.Access, nil), 409, "last_owner")
	want(t, h.API(t, "PATCH", path+"/members/"+b.User.ID, b.Access, map[string]any{"role": "owner"}), 200, "") // unchanged

	// With a co-owner: the billing owner still needs a transfer, whoever asks.
	addMember(t, h, tm.ID, c.User.ID, roles.Owner)
	want(t, h.API(t, "PATCH", path+"/members/"+b.User.ID, c.Access, map[string]any{"role": "admin"}), 409, "transfer_required")
	want(t, h.API(t, "DELETE", path+"/members/"+b.User.ID, c.Access, nil), 409, "transfer_required")
	want(t, h.API(t, "PATCH", path+"/members/"+b.User.ID, b.Access, map[string]any{"role": "viewer"}), 409, "transfer_required")
	want(t, h.API(t, "POST", path+"/leave", b.Access, nil), 409, "transfer_required")
	// Only the billing owner deletes or transfers.
	want(t, h.API(t, "DELETE", path, c.Access, nil), 403, "forbidden_role")
	want(t, h.API(t, "POST", path+"/transfer", c.Access, map[string]any{"userId": b.User.ID}), 403, "forbidden_role")
	if roleOf(t, h, tm.ID, b.User.ID) != "owner" {
		t.Fatal("billing owner changed")
	}
	// A co-owner may leave (the billing owner stays) or lower their own role.
	want(t, h.API(t, "PATCH", path+"/members/"+c.User.ID, c.Access, map[string]any{"role": "admin"}), 200, "")
	want(t, h.API(t, "POST", path+"/leave", c.Access, nil), 204, "")
	if h.Count(t, "team_departures", "team_id = ? AND user_id = ? AND reason = 'left'", tm.ID, c.User.ID) != 1 {
		t.Fatal("no left departure")
	}
	want(t, h.API(t, "POST", path+"/leave", c.Access, nil), 403, "not_a_member")
}

func TestTeams_TransferOwnership(t *testing.T) {
	h := teamsHarness(t, "REQHARBOR_QUOTA_OWNER_BYTES=2000")
	b := h.QuickUser(t, "b@example.test", "Bea")
	m := h.QuickUser(t, "m@example.test", "Max")
	tm := createTeam(t, h, b.Access, "Acme")
	path := "/v1/teams/" + tm.ID + "/transfer"
	want(t, h.API(t, "POST", path, b.Access, map[string]any{"userId": b.User.ID}), 400, "validation_failed")
	want(t, h.API(t, "POST", path, b.Access, map[string]any{}), 400, "validation_failed")
	want(t, h.API(t, "POST", path, b.Access, map[string]any{"userId": m.User.ID}), 404, "member_not_found")
	addMember(t, h, tm.ID, m.User.ID, roles.Viewer)

	// The receiver's aggregate (personal + owned teams) plus this team must fit their quota.
	big := strings.Repeat("x", 900)
	want(t, h.API(t, "POST", "/v1/spaces/"+tm.Space+"/sync", b.Access, tu.Changes(tu.Change("request", "r1", 1, map[string]any{"n": big}))), 200, "")
	mp := h.PersonalSpace(t, m.Access)
	want(t, h.API(t, "POST", "/v1/spaces/"+mp+"/sync", m.Access, tu.Changes(tu.Change("request", "r1", 1, map[string]any{"n": big + big[:200]}))), 200, "")
	res := h.API(t, "POST", path, b.Access, map[string]any{"userId": m.User.ID})
	want(t, res, 413, "quota_exceeded")
	if !strings.Contains(string(res.Body), "Max doesn't have room for Acme: they need ") {
		t.Fatalf("message: %s", res.Body)
	}
	want(t, h.API(t, "POST", "/v1/spaces/"+mp+"/sync", m.Access, tu.Changes(tu.Tombstone("request", "r1", 2))), 200, "")

	res = h.API(t, "POST", path, b.Access, map[string]any{"userId": m.User.ID})
	want(t, res, 200, "")
	if v := res.JSON(t); v["role"] != "admin" || v["ownerUserId"] != m.User.ID {
		t.Fatalf("transfer response: %s", res.Body)
	}
	if roleOf(t, h, tm.ID, m.User.ID) != "owner" || roleOf(t, h, tm.ID, b.User.ID) != "admin" ||
		h.Count(t, "teams", "id = ? AND owner_user_id = ?", tm.ID, m.User.ID) != 1 {
		t.Fatal("roles after transfer")
	}
	// The team's storage now counts against the new owner.
	if v := h.API(t, "GET", "/v1/me/spaces", m.Access, nil).JSON(t); v["owner"].(map[string]any)["bytesUsed"].(float64) < 900 {
		t.Fatalf("new owner's aggregate: %v", v["owner"])
	}
}

// A transfer racing a removal of the receiver: whichever runs first, the other fails, and the
// team keeps exactly one billing owner who is an owner.
func TestTeams_TransferRacingRemovalLeavesAnOwner(t *testing.T) {
	h := teamsHarness(t, "REQHARBOR_MAX_OWNED_TEAMS=0")
	o := h.QuickUser(t, "o@example.test", "O")
	m := h.QuickUser(t, "m@example.test", "M")
	for i := 0; i < 15; i++ {
		tm := createTeam(t, h, o.Access, fmt.Sprintf("Race %d", i))
		addMember(t, h, tm.ID, m.User.ID, roles.Admin)
		var wg sync.WaitGroup
		var transfer, remove *tu.Resp
		wg.Add(2)
		go func() {
			defer wg.Done()
			transfer = h.API(t, "POST", "/v1/teams/"+tm.ID+"/transfer", o.Access, map[string]any{"userId": m.User.ID})
		}()
		go func() {
			defer wg.Done()
			remove = h.API(t, "DELETE", "/v1/teams/"+tm.ID+"/members/"+m.User.ID, o.Access, nil)
		}()
		wg.Wait()
		ok := 0
		for _, r := range []*tu.Resp{transfer, remove} {
			if r.StatusCode == 200 {
				ok++
			}
		}
		if ok != 1 {
			t.Fatalf("round %d: transfer %d %s, remove %d %s", i, transfer.StatusCode, transfer.Body, remove.StatusCode, remove.Body)
		}
		if transfer.StatusCode != 200 && (transfer.StatusCode != 404 || transfer.ErrorCode(t) != "member_not_found") {
			t.Fatalf("losing transfer: %d %s", transfer.StatusCode, transfer.Body)
		}
		if remove.StatusCode != 200 && (remove.StatusCode != 403 || remove.ErrorCode(t) != "forbidden_role") {
			t.Fatalf("losing removal: %d %s", remove.StatusCode, remove.Body)
		}
		if n := h.Count(t, "team_members", "team_id = ? AND role = 'owner'", tm.ID); n < 1 {
			t.Fatalf("round %d: %d owners", i, n)
		}
		if n := h.Count(t, "teams t JOIN team_members m ON m.team_id = t.id AND m.user_id = t.owner_user_id", "t.id = ? AND m.role = 'owner'", tm.ID); n != 1 {
			t.Fatalf("round %d: billing owner is not an owner member", i)
		}
	}
}

// ---------------------------------------------------------------- IDOR

func TestTeams_CrossTeamIDSubstitutionIs404(t *testing.T) {
	h := teamsHarness(t)
	x := h.QuickUser(t, "x@example.test", "X")
	y := h.QuickUser(t, "y@example.test", "Y")
	a := createTeam(t, h, x.Access, "A")
	b := createTeam(t, h, y.Access, "B")
	_, invB := newInvite(t, h, y.Access, b.ID, nil)
	tokB, _ := newInvite(t, h, y.Access, b.ID, map[string]any{"maxUses": 5})

	// Another team's invite id through my team: 404, and it stays usable.
	want(t, h.API(t, "DELETE", "/v1/teams/"+a.ID+"/invites/"+invB, x.Access, nil), 404, "invite_not_found")
	if h.Count(t, "team_invites", "id = ? AND revoked_at IS NULL", invB) != 1 {
		t.Fatal("B's invite was revoked through A")
	}
	// A user who is not in my team: 404 for every member route.
	want(t, h.API(t, "PATCH", "/v1/teams/"+a.ID+"/members/"+y.User.ID, x.Access, map[string]any{"role": "viewer"}), 404, "member_not_found")
	want(t, h.API(t, "DELETE", "/v1/teams/"+a.ID+"/members/"+y.User.ID, x.Access, nil), 404, "member_not_found")
	want(t, h.API(t, "POST", "/v1/teams/"+a.ID+"/transfer", x.Access, map[string]any{"userId": y.User.ID}), 404, "member_not_found")
	if roleOf(t, h, b.ID, y.User.ID) != "owner" {
		t.Fatal("Y's role changed")
	}
	// Another team's routes: 403 not_a_member, nothing changes.
	want(t, h.API(t, "GET", "/v1/teams/"+b.ID+"/invites", x.Access, nil), 403, "not_a_member")
	want(t, h.API(t, "DELETE", "/v1/teams/"+b.ID+"/invites/"+invB, x.Access, nil), 403, "not_a_member")
	want(t, h.API(t, "DELETE", "/v1/teams/"+b.ID, x.Access, nil), 403, "not_a_member")
	// Another space's share id through my space: 404.
	want(t, acceptInvite(t, h, x.Access, tokB), 200, "") // X joins B as an editor
	res := h.API(t, "POST", "/v1/spaces/"+b.Space+"/shares", y.Access, map[string]any{"kind": "collection", "entityId": "c1", "snapshot": tu.SampleCollectionSnapshot()})
	want(t, res, 201, "")
	sid := res.JSON(t)["shareId"].(string)
	want(t, h.API(t, "PUT", "/v1/spaces/"+a.Space+"/shares/"+sid, x.Access, map[string]any{"snapshot": tu.SampleCollectionSnapshot()}), 404, "not_found")
	want(t, h.API(t, "DELETE", "/v1/spaces/"+a.Space+"/shares/"+sid, x.Access, nil), 404, "not_found")
	// Another user's session id: 404.
	want(t, h.API(t, "DELETE", "/v1/me/sessions/"+y.SessionID, x.Access, nil), 404, "not_found")
	if h.Count(t, "shares", "id = ? AND revoked_at IS NULL", sid) != 1 || h.Count(t, "sessions", "id = ? AND revoked_at IS NULL", y.SessionID) != 1 {
		t.Fatal("cross-scope ids changed something")
	}
}

// ---------------------------------------------------------------- invites

func TestInvites_CreateListPreviewRevoke(t *testing.T) {
	h := teamsHarness(t)
	o := h.QuickUser(t, "o@example.test", "Priya Shah")
	tm := createTeam(t, h, o.Access, "Acme")
	now := h.Clock.Now().UnixMilli()

	res := h.API(t, "POST", "/v1/teams/"+tm.ID+"/invites", o.Access, map[string]any{})
	want(t, res, 201, "")
	v := res.JSON(t)
	token, iid := v["token"].(string), v["inviteId"].(string)
	if !strings.HasPrefix(token, "rhi_") || len(token) != 47 || v["url"] != "https://reqharbor.test/i#"+token {
		t.Fatalf("token/url: %s", res.Body)
	}
	tu.JSONEq(t, res.Body, map[string]any{"inviteId": iid, "role": "editor", "createdAt": now, "expiresAt": now + 7*86_400_000,
		"maxUses": 1, "uses": 0, "createdBy": map[string]any{"userId": o.User.ID, "name": "Priya Shah"}, "token": token, "url": "https://reqharbor.test/i#" + token})
	if h.Count(t, "team_invites", "id = ? AND token_hash = ?", iid, ids.SHA256Hex(token)) != 1 || h.Count(t, "team_invites", "token_hash = ?", token) != 0 {
		t.Fatal("only the token's hash is stored")
	}
	tok2, _ := newInvite(t, h, o.Access, tm.ID, map[string]any{"role": "viewer", "maxUses": nil, "expiresInDays": 30})

	res = h.API(t, "GET", "/v1/teams/"+tm.ID+"/invites", o.Access, nil)
	want(t, res, 200, "")
	if list := res.JSON(t)["invites"].([]any); len(list) != 2 {
		t.Fatalf("list: %s", res.Body)
	}
	if strings.Contains(string(res.Body), "rhi_") {
		t.Fatal("the list never shows tokens")
	}

	// Preview needs no auth and says only the team, inviter, role and expiry.
	res = h.Call(t, "/v1/invites/preview", tu.Req{JSON: map[string]any{"token": token}})
	want(t, res, 200, "")
	tu.JSONEq(t, res.Body, map[string]any{"teamName": "Acme", "inviterName": "Priya Shah", "role": "editor", "expiresAt": now + 7*86_400_000})
	for _, bad := range []any{"", "rhi_short", "rh_" + token[4:], strings.Replace(token, "rhi_", "rht_", 1), 5, nil} {
		want(t, h.Call(t, "/v1/invites/preview", tu.Req{JSON: map[string]any{"token": bad}}), 410, "invite_invalid")
	}
	want(t, h.Call(t, "/v1/invites/preview", tu.Req{JSON: map[string]any{"token": ids.Token("rhi_")}}), 410, "invite_invalid")

	// Revoke: immediate and idempotent; the link is then invalid everywhere.
	h.Clock.Advance(time.Second)
	res = h.API(t, "DELETE", "/v1/teams/"+tm.ID+"/invites/"+iid, o.Access, nil)
	want(t, res, 200, "")
	revokedAt := h.Clock.Now().UnixMilli()
	tu.JSONEq(t, res.Body, map[string]any{"inviteId": iid, "revokedAt": revokedAt})
	h.Clock.Advance(time.Second)
	tu.JSONEq(t, h.API(t, "DELETE", "/v1/teams/"+tm.ID+"/invites/"+iid, o.Access, nil).Body, map[string]any{"inviteId": iid, "revokedAt": revokedAt})
	want(t, h.API(t, "DELETE", "/v1/teams/"+tm.ID+"/invites/nope", o.Access, nil), 404, "invite_not_found")
	want(t, h.Call(t, "/v1/invites/preview", tu.Req{JSON: map[string]any{"token": token}}), 410, "invite_invalid")
	u := h.QuickUser(t, "u@example.test", "U")
	want(t, acceptInvite(t, h, u.Access, token), 410, "invite_invalid")

	// Accept: the role of the link; a member accepting again keeps their role.
	res = acceptInvite(t, h, u.Access, tok2)
	want(t, res, 200, "")
	tu.JSONEq(t, res.Body, map[string]any{"alreadyMember": false, "teamId": tm.ID, "teamName": "Acme", "spaceId": tm.Space, "role": "viewer"})
	res = acceptInvite(t, h, o.Access, tok2)
	tu.JSONEq(t, res.Body, map[string]any{"alreadyMember": true, "teamId": tm.ID, "teamName": "Acme", "spaceId": tm.Space, "role": "owner"})
	if roleOf(t, h, tm.ID, o.User.ID) != "owner" {
		t.Fatal("already-member accept changed the role")
	}
	want(t, h.Call(t, "/v1/invites/accept", tu.Req{JSON: map[string]any{"token": tok2}}), 401, "auth_required")

	// Expiry and used-up links.
	tok3, _ := newInvite(t, h, o.Access, tm.ID, map[string]any{"expiresInDays": 1})
	h.Clock.Advance(24 * time.Hour)
	want(t, h.Call(t, "/v1/invites/preview", tu.Req{JSON: map[string]any{"token": tok3}}), 410, "invite_invalid")
	tok4, _ := newInvite(t, h, o.Access, tm.ID, nil)
	want(t, acceptInvite(t, h, h.QuickUser(t, "v@example.test", "V").Access, tok4), 200, "")
	want(t, acceptInvite(t, h, h.QuickUser(t, "w@example.test", "W").Access, tok4), 410, "invite_invalid")

	// Validation.
	for _, body := range []map[string]any{{"role": "owner"}, {"role": 1}, {"maxUses": 0}, {"maxUses": 10001}, {"maxUses": "5"},
		{"maxUses": 1.5}, {"expiresInDays": 0}, {"expiresInDays": 31}, {"expiresInDays": "7"}} {
		want(t, h.API(t, "POST", "/v1/teams/"+tm.ID+"/invites", o.Access, body), 400, "validation_failed")
	}
}

func TestInvites_AdminLinksAreSingleUseAndOwnerOnly(t *testing.T) {
	h := teamsHarness(t)
	o := h.QuickUser(t, "o@example.test", "O")
	adm := h.QuickUser(t, "a@example.test", "A")
	ed := h.QuickUser(t, "e@example.test", "E")
	vw := h.QuickUser(t, "v@example.test", "V")
	tm := createTeam(t, h, o.Access, "Acme")
	addMember(t, h, tm.ID, adm.User.ID, roles.Admin)
	addMember(t, h, tm.ID, ed.User.ID, roles.Editor)
	addMember(t, h, tm.ID, vw.User.ID, roles.Viewer)
	p := "/v1/teams/" + tm.ID + "/invites"

	want(t, h.API(t, "POST", p, o.Access, map[string]any{"role": "admin", "maxUses": 5}), 400, "validation_failed")
	want(t, h.API(t, "POST", p, o.Access, map[string]any{"role": "admin", "maxUses": nil}), 400, "validation_failed")
	want(t, h.API(t, "POST", p, adm.Access, map[string]any{"role": "admin", "maxUses": 1}), 403, "forbidden_role")
	want(t, h.API(t, "POST", p, o.Access, map[string]any{"role": "admin", "maxUses": 1}), 201, "")
	want(t, h.API(t, "POST", p, adm.Access, map[string]any{"role": "editor", "maxUses": 25}), 201, "")
	for _, u := range []tu.Tokens{ed, vw} {
		want(t, h.API(t, "POST", p, u.Access, map[string]any{}), 403, "forbidden_role")
		want(t, h.API(t, "GET", p, u.Access, nil), 403, "forbidden_role")
	}
	// The 0005 CHECK is the backstop: an admin link with more than one use, or unlimited, is refused.
	for _, maxUses := range []any{nil, 2} {
		_, err := h.DB.Writer.Exec(`INSERT INTO team_invites (id, team_id, token_hash, role, created_at, expires_at, max_uses)
		     VALUES (?, ?, ?, 'admin', 1, 2, ?)`, ids.ULID(h.Clock.Now()), tm.ID, ids.Token("h"), maxUses)
		if err == nil || !strings.Contains(err.Error(), "CHECK") {
			t.Fatalf("admin link with max_uses %v: %v", maxUses, err)
		}
	}
}

func TestInvites_MemberAndInviteLimits(t *testing.T) {
	h := teamsHarness(t, "REQHARBOR_MAX_TEAM_MEMBERS=2", "REQHARBOR_MAX_TEAM_INVITES=2")
	o := h.QuickUser(t, "o@example.test", "O")
	tm := createTeam(t, h, o.Access, "Acme")
	p := "/v1/teams/" + tm.ID + "/invites"
	t1, _ := newInvite(t, h, o.Access, tm.ID, map[string]any{"maxUses": 5})
	_, i2 := newInvite(t, h, o.Access, tm.ID, nil)
	res := h.API(t, "POST", p, o.Access, map[string]any{})
	want(t, res, 409, "limit_invites")
	if !strings.Contains(string(res.Body), "Acme already has 2 active invite links") {
		t.Fatalf("message: %s", res.Body)
	}
	want(t, h.API(t, "DELETE", p+"/"+i2, o.Access, nil), 200, "")
	newInvite(t, h, o.Access, tm.ID, nil) // a revoked link frees its slot

	want(t, acceptInvite(t, h, h.QuickUser(t, "a@example.test", "A").Access, t1), 200, "")
	res = acceptInvite(t, h, h.QuickUser(t, "b@example.test", "B").Access, t1)
	want(t, res, 409, "limit_members")
	if !strings.Contains(string(res.Body), "Acme has reached 2 members") {
		t.Fatalf("message: %s", res.Body)
	}
	if h.Count(t, "team_invites", "token_hash = ? AND uses = 1", ids.SHA256Hex(t1)) != 1 {
		t.Fatal("a refused accept used the link")
	}
}

func TestInvites_ThirtyCreatesPerTeamPerDay(t *testing.T) {
	h := teamsHarness(t, "REQHARBOR_MAX_TEAM_INVITES=0")
	o := h.QuickUser(t, "o@example.test", "O")
	tm := createTeam(t, h, o.Access, "Acme")
	for i := 0; i < 30; i++ {
		newInvite(t, h, o.Access, tm.ID, nil)
	}
	res := h.API(t, "POST", "/v1/teams/"+tm.ID+"/invites", o.Access, map[string]any{})
	want(t, res, 429, "rate_limited")
	if res.Header.Get("Retry-After") == "" {
		t.Fatal("no Retry-After")
	}
	h.Clock.Advance(24 * time.Hour)
	newInvite(t, h, o.Access, tm.ID, nil)
}

func TestInvites_ParallelAcceptsOfASingleUseLinkExactlyOneWins(t *testing.T) {
	h := teamsHarness(t)
	o := h.QuickUser(t, "o@example.test", "O")
	tm := createTeam(t, h, o.Access, "Acme")
	token, _ := newInvite(t, h, o.Access, tm.ID, map[string]any{"maxUses": 1})
	users := make([]tu.Tokens, 10)
	for i := range users {
		users[i] = h.QuickUser(t, fmt.Sprintf("u%d@example.test", i), "U")
	}
	results := make([]*tu.Resp, len(users))
	var wg sync.WaitGroup
	start := make(chan struct{})
	for i := range users {
		wg.Add(1)
		go func(i int) {
			defer wg.Done()
			<-start
			results[i] = acceptInvite(t, h, users[i].Access, token)
		}(i)
	}
	close(start)
	wg.Wait()
	won := 0
	for _, r := range results {
		switch {
		case r.StatusCode == 200:
			won++
		case r.StatusCode != 410 || r.ErrorCode(t) != "invite_invalid":
			t.Fatalf("loser: %d %s", r.StatusCode, r.Body)
		}
	}
	if won != 1 || h.Count(t, "team_members", "team_id = ?", tm.ID) != 2 || h.Count(t, "team_invites", "token_hash = ? AND uses = 1", ids.SHA256Hex(token)) != 1 {
		t.Fatalf("winners %d", won)
	}
}

// QA "Invite abuse after removal or demotion": a removal blocks links created before it for that
// account; the removed or demoted admin's own links are revoked at once; revokeInvites revokes
// the rest, which is what stops an alt account.
func TestInvites_RemovalBlocksOldLinksAndRevokesTheMembersOwn(t *testing.T) {
	h := teamsHarness(t)
	o := h.QuickUser(t, "o@example.test", "O")
	u := h.QuickUser(t, "u@example.test", "U")
	tm := createTeam(t, h, o.Access, "Acme")
	old, _ := newInvite(t, h, o.Access, tm.ID, map[string]any{"maxUses": 10})
	want(t, acceptInvite(t, h, u.Access, old), 200, "")
	tick(h)
	want(t, h.API(t, "DELETE", "/v1/teams/"+tm.ID+"/members/"+u.User.ID, o.Access, nil), 200, "")
	tick(h)
	// The removed user's pre-removal link: 410, while it still works for others.
	want(t, acceptInvite(t, h, u.Access, old), 410, "invite_invalid")
	want(t, h.Call(t, "/v1/invites/preview", tu.Req{JSON: map[string]any{"token": old}}), 200, "")
	// A link created after the removal works.
	fresh, _ := newInvite(t, h, o.Access, tm.ID, nil)
	want(t, acceptInvite(t, h, u.Access, fresh), 200, "")
	if roleOf(t, h, tm.ID, u.User.ID) != "editor" {
		t.Fatal("rejoin")
	}

	// Leaving is not a removal: the same old link lets a leaver back in.
	v := h.QuickUser(t, "v@example.test", "V")
	want(t, acceptInvite(t, h, v.Access, old), 200, "")
	tick(h)
	want(t, h.API(t, "POST", "/v1/teams/"+tm.ID+"/leave", v.Access, nil), 204, "")
	tick(h)
	want(t, acceptInvite(t, h, v.Access, old), 200, "")

	// A removed admin's own links are revoked, whatever revokeInvites says.
	adm := h.QuickUser(t, "adm@example.test", "Adm")
	addMember(t, h, tm.ID, adm.User.ID, roles.Admin)
	hisA, _ := newInvite(t, h, adm.Access, tm.ID, map[string]any{"maxUses": 5})
	hisB, _ := newInvite(t, h, adm.Access, tm.ID, nil)
	tick(h)
	res := h.API(t, "DELETE", "/v1/teams/"+tm.ID+"/members/"+adm.User.ID, o.Access, map[string]any{"revokeInvites": false})
	tu.JSONEq(t, res.Body, map[string]any{"userId": adm.User.ID, "revokedInvites": 2})
	alt := h.QuickUser(t, "alt@example.test", "Alt")
	want(t, acceptInvite(t, h, alt.Access, hisA), 410, "invite_invalid")
	want(t, acceptInvite(t, h, alt.Access, hisB), 410, "invite_invalid")
	want(t, h.Call(t, "/v1/invites/preview", tu.Req{JSON: map[string]any{"token": hisA}}), 410, "invite_invalid")

	// A demoted admin's links are revoked in the same transaction as the demotion.
	adm2 := h.QuickUser(t, "adm2@example.test", "Adm2")
	addMember(t, h, tm.ID, adm2.User.ID, roles.Admin)
	his2, _ := newInvite(t, h, adm2.Access, tm.ID, map[string]any{"maxUses": 5})
	want(t, h.API(t, "PATCH", "/v1/teams/"+tm.ID+"/members/"+adm2.User.ID, o.Access, map[string]any{"role": "editor"}), 200, "")
	want(t, acceptInvite(t, h, alt.Access, his2), 410, "invite_invalid")

	// An alt account holding a link the owner made before a removal: revokeInvites revokes it.
	ownersLink, _ := newInvite(t, h, o.Access, tm.ID, map[string]any{"maxUses": 5})
	tick(h)
	res = h.API(t, "DELETE", "/v1/teams/"+tm.ID+"/members/"+adm2.User.ID, o.Access, map[string]any{"revokeInvites": true})
	want(t, res, 200, "")
	if res.JSON(t)["revokedInvites"].(float64) < 2 { // old and ownersLink at least
		t.Fatalf("revokeInvites: %s", res.Body)
	}
	want(t, acceptInvite(t, h, alt.Access, ownersLink), 410, "invite_invalid")
	want(t, acceptInvite(t, h, alt.Access, old), 410, "invite_invalid")
}

// ---------------------------------------------------------------- role enforcement in sync

func TestTeamSync_RoleEnforcement(t *testing.T) {
	h := teamsHarness(t)
	o := h.QuickUser(t, "o@example.test", "O")
	adm := h.QuickUser(t, "a@example.test", "A")
	ed := h.QuickUser(t, "e@example.test", "E")
	vw := h.QuickUser(t, "v@example.test", "V")
	tm := createTeam(t, h, o.Access, "Acme")
	addMember(t, h, tm.ID, adm.User.ID, roles.Admin)
	addMember(t, h, tm.ID, ed.User.ID, roles.Editor)
	addMember(t, h, tm.ID, vw.User.ID, roles.Viewer)
	sync := "/v1/spaces/" + tm.Space + "/sync"

	want(t, h.API(t, "POST", sync, o.Access, tu.Changes(tu.Change("project", "p1", 1, nil), tu.Change("project", "p2", 1, nil),
		tu.Change("collection", "c1", 1, nil))), 200, "")

	// A viewer's push is refused whole, before the body is even read.
	want(t, h.API(t, "POST", sync, vw.Access, tu.Changes(tu.Change("request", "r1", 2, nil))), 403, "forbidden_role")
	want(t, h.Call(t, sync, tu.Req{Method: "POST", Token: vw.Access, Raw: tu.Str("{not json"),
		Headers: map[string]string{"X-ReqHarbor-Version": tu.TeamClientVersion}}), 403, "forbidden_role")
	want(t, h.API(t, "GET", sync, vw.Access, nil), 200, "")
	if h.Count(t, "entities", "workspace_id = ? AND id = 'r1'", tm.Space) != 0 {
		t.Fatal("viewer push stored")
	}

	// An editor's project tombstone is a per-change forbidden_role; the rest applies, including
	// other deletes (editors may delete content; admin-only is the project container).
	res := h.API(t, "POST", sync, ed.Access, tu.Changes(tu.Change("request", "r1", 2, nil), tu.Tombstone("project", "p1", 2),
		tu.Tombstone("collection", "c1", 2)))
	got := results(t, res)
	if got[0]["status"] != "applied" || got[1]["status"] != "rejected" || got[1]["reason"] != "forbidden_role" || got[1]["rev"] != nil ||
		got[2]["status"] != "applied" {
		t.Fatalf("editor push: %s", res.Body)
	}
	if h.Count(t, "entities", "workspace_id = ? AND id = 'p1' AND deleted = 0", tm.Space) != 1 {
		t.Fatal("project deleted by an editor")
	}
	// An editor may still create and rename projects.
	want(t, h.API(t, "POST", sync, ed.Access, tu.Changes(tu.Change("project", "p1", 3, map[string]any{"id": "p1", "name": "Renamed"}))), 200, "")
	// Admins and owners delete projects.
	for _, c := range []struct {
		who     tu.Tokens
		project string
	}{{adm, "p1"}, {o, "p2"}} {
		res = h.API(t, "POST", sync, c.who.Access, tu.Changes(tu.Tombstone("project", c.project, 9)))
		if r := results(t, res); r[0]["status"] != "applied" {
			t.Fatalf("admin+ project delete: %s", res.Body)
		}
	}
	// The owner of a personal space deletes projects as before.
	ps := h.PersonalSpace(t, ed.Access)
	h.API(t, "POST", "/v1/spaces/"+ps+"/sync", ed.Access, tu.Changes(tu.Change("project", "pp", 1, nil)))
	if r := results(t, h.API(t, "POST", "/v1/spaces/"+ps+"/sync", ed.Access, tu.Changes(tu.Tombstone("project", "pp", 2)))); r[0]["status"] != "applied" {
		t.Fatal("personal project delete")
	}

	// A removed member gets 403 not_a_member; a demoted one is a viewer at once.
	want(t, h.API(t, "PATCH", "/v1/teams/"+tm.ID+"/members/"+ed.User.ID, o.Access, map[string]any{"role": "viewer"}), 200, "")
	want(t, h.API(t, "POST", sync, ed.Access, tu.Changes(tu.Change("request", "r9", 9, nil))), 403, "forbidden_role")
	want(t, h.API(t, "DELETE", "/v1/teams/"+tm.ID+"/members/"+ed.User.ID, o.Access, nil), 200, "")
	want(t, h.API(t, "POST", sync, ed.Access, tu.Changes(tu.Change("request", "r9", 9, nil))), 403, "not_a_member")
	want(t, h.API(t, "GET", sync, ed.Access, nil), 403, "not_a_member")
}

func TestTeamSync_GlobalEnvironmentIsNotAllowedInTeamSpaces(t *testing.T) {
	h := teamsHarness(t)
	o := h.QuickUser(t, "o@example.test", "O")
	tm := createTeam(t, h, o.Access, "Acme")
	env := func(id string, project any) map[string]any {
		return tu.Change("environment", id, 1, map[string]any{"id": id, "projectId": project, "name": "Env",
			"variables": []any{map[string]any{"key": "token", "value": "s3cret", "enabled": true, "secret": true}}})
	}
	noProject := tu.Change("environment", "e4", 1, map[string]any{"id": "e4", "name": "Env"})
	res := h.API(t, "POST", "/v1/spaces/"+tm.Space+"/sync", o.Access, tu.Changes(env("e1", nil), env("e2", "p1"), env("e3", ""), noProject,
		tu.Tombstone("environment", "e5", 1)))
	r := results(t, res)
	for i, wantStatus := range []string{"rejected", "applied", "rejected", "rejected", "applied"} {
		if r[i]["status"] != wantStatus || (wantStatus == "rejected" && r[i]["reason"] != "global_env_not_allowed") {
			t.Fatalf("change %d: %s", i, res.Body)
		}
	}
	var data string
	if err := h.DB.Reader.QueryRow("SELECT data FROM entities WHERE workspace_id = ? AND id = 'e2'", tm.Space).Scan(&data); err != nil || strings.Contains(data, "s3cret") {
		t.Fatalf("project environment stored %q (%v); secrets must be blanked", data, err)
	}
	// The personal space still takes the Global environment.
	ps := h.PersonalSpace(t, o.Access)
	if r := results(t, h.API(t, "POST", "/v1/spaces/"+ps+"/sync", o.Access, tu.Changes(env("g", nil)))); r[0]["status"] != "applied" {
		t.Fatal("personal global env")
	}
}

func TestTeamSync_DeviceLocalPathsAreBlankedInTeamSpacesOnly(t *testing.T) {
	h := teamsHarness(t)
	o := h.QuickUser(t, "o@example.test", "O")
	tm := createTeam(t, h, o.Access, "Acme")
	req := func(id string) map[string]any {
		return tu.Change("request", id, 1, tu.SampleRequest(map[string]any{"id": id, "body": map[string]any{
			"type": "binary", "binaryPath": "/Users/ada/secret-plans.bin",
			"form": []any{
				map[string]any{"key": "doc", "value": "/Users/ada/cv.pdf", "kind": "file", "enabled": true},
				map[string]any{"key": "note", "value": "keep me", "kind": "text", "enabled": true},
			},
		}}))
	}
	want(t, h.API(t, "POST", "/v1/spaces/"+tm.Space+"/sync", o.Access, tu.Changes(req("r1"))), 200, "")
	ps := h.PersonalSpace(t, o.Access)
	want(t, h.API(t, "POST", "/v1/spaces/"+ps+"/sync", o.Access, tu.Changes(req("r1"))), 200, "")

	body := func(space string) (map[string]any, int64) {
		res := h.API(t, "GET", "/v1/spaces/"+space+"/sync", o.Access, nil)
		ch := res.JSON(t)["changes"].([]any)[0].(map[string]any)
		var size int64
		if err := h.DB.Reader.QueryRow("SELECT size FROM entities WHERE workspace_id = ? AND id = 'r1'", space).Scan(&size); err != nil {
			t.Fatal(err)
		}
		return ch["data"].(map[string]any)["body"].(map[string]any), size
	}
	tb, tsize := body(tm.Space)
	form := tb["form"].([]any)
	if tb["binaryPath"] != "" || form[0].(map[string]any)["value"] != "" || form[1].(map[string]any)["value"] != "keep me" || form[0].(map[string]any)["key"] != "doc" {
		t.Fatalf("team body: %v", tb)
	}
	pb, psize := body(ps)
	if pb["binaryPath"] != "/Users/ada/secret-plans.bin" || pb["form"].([]any)[0].(map[string]any)["value"] != "/Users/ada/cv.pdf" {
		t.Fatalf("personal body: %v", pb)
	}
	// The size (and so the quota) is measured after blanking.
	if psize-tsize != int64(len("/Users/ada/secret-plans.bin")+len("/Users/ada/cv.pdf")) {
		t.Fatalf("sizes: team %d personal %d", tsize, psize)
	}
	if u := spaceGet(t, h, o.Access, ps)["bytesUsed"].(float64); int64(u) != psize {
		t.Fatalf("bytesUsed %v", u)
	}
}

func TestTeamShares_EditorsOwnSharesAdminsAnyViewersNone(t *testing.T) {
	h := teamsHarness(t)
	o := h.QuickUser(t, "o@example.test", "O")
	adm := h.QuickUser(t, "a@example.test", "A")
	e1 := h.QuickUser(t, "e1@example.test", "E1")
	e2 := h.QuickUser(t, "e2@example.test", "E2")
	vw := h.QuickUser(t, "v@example.test", "V")
	tm := createTeam(t, h, o.Access, "Acme")
	addMember(t, h, tm.ID, adm.User.ID, roles.Admin)
	addMember(t, h, tm.ID, e1.User.ID, roles.Editor)
	addMember(t, h, tm.ID, e2.User.ID, roles.Editor)
	addMember(t, h, tm.ID, vw.User.ID, roles.Viewer)
	p := "/v1/spaces/" + tm.Space + "/shares"
	body := map[string]any{"kind": "collection", "entityId": "c1", "snapshot": tu.SampleCollectionSnapshot()}

	want(t, h.API(t, "POST", p, vw.Access, body), 403, "forbidden_role")
	res := h.API(t, "POST", p, e1.Access, body)
	want(t, res, 201, "")
	sid := res.JSON(t)["shareId"].(string)
	upd := map[string]any{"snapshot": tu.SampleCollectionSnapshot()}

	for _, c := range []struct {
		who    tu.Tokens
		status int
	}{{vw, 403}, {e2, 403}, {e1, 200}, {adm, 200}, {o, 200}} {
		res := h.API(t, "PUT", p+"/"+sid, c.who.Access, upd)
		if res.StatusCode != c.status || (c.status == 403 && res.ErrorCode(t) != "forbidden_role") {
			t.Fatalf("PUT by %s: %d %s", c.who.User.Name, res.StatusCode, res.Body)
		}
	}
	want(t, h.API(t, "DELETE", p+"/"+sid, vw.Access, nil), 403, "forbidden_role")
	want(t, h.API(t, "DELETE", p+"/"+sid, e2.Access, nil), 403, "forbidden_role")
	want(t, h.API(t, "GET", p, vw.Access, nil), 200, "")
	want(t, h.API(t, "DELETE", p+"/"+sid, adm.Access, nil), 200, "")
	// Revoked: the role check still comes first, then 410 for an update.
	want(t, h.API(t, "PUT", p+"/"+sid, e2.Access, upd), 403, "forbidden_role")
	want(t, h.API(t, "PUT", p+"/"+sid, e1.Access, upd), 410, "gone")

	res = h.API(t, "POST", p, e2.Access, body)
	sid2 := res.JSON(t)["shareId"].(string)
	want(t, h.API(t, "DELETE", p+"/"+sid2, e2.Access, nil), 200, "")
	// An unknown share is 404 for everyone.
	want(t, h.API(t, "DELETE", p+"/"+strings.Repeat("A", 22), vw.Access, nil), 404, "not_found")
}

func TestTeamSync_TeamQuotaAndOwnerAggregate(t *testing.T) {
	h := teamsHarness(t, "REQHARBOR_QUOTA_TEAM_BYTES=800", "REQHARBOR_QUOTA_OWNER_BYTES=1000")
	o := h.QuickUser(t, "o@example.test", "Olu")
	m := h.QuickUser(t, "m@example.test", "Max")
	tm := createTeam(t, h, o.Access, "Acme")
	addMember(t, h, tm.ID, m.User.ID, roles.Editor)
	sync := "/v1/spaces/" + tm.Space + "/sync"
	data := func(n int) map[string]any { return map[string]any{"x": strings.Repeat("y", n)} }

	// The team quota.
	res := h.API(t, "POST", sync, m.Access, tu.Changes(tu.Change("request", "big", 1, data(900))))
	want(t, res, 413, "quota_exceeded")
	if !strings.Contains(string(res.Body), "quota of 800 bytes") {
		t.Fatalf("message: %s", res.Body)
	}
	want(t, h.API(t, "POST", sync, m.Access, tu.Changes(tu.Change("request", "a", 1, data(500)))), 200, "")

	// The owner's aggregate counts the personal space and every owned team, whoever pushes.
	ps := h.PersonalSpace(t, o.Access)
	want(t, h.API(t, "POST", "/v1/spaces/"+ps+"/sync", o.Access, tu.Changes(tu.Change("request", "p", 1, data(400)))), 200, "")
	res = h.API(t, "POST", sync, m.Access, tu.Changes(tu.Change("request", "b", 1, data(90))))
	want(t, res, 413, "quota_exceeded")
	if !strings.Contains(string(res.Body), "for the spaces Olu owns") {
		t.Fatalf("message names the owner: %s", res.Body)
	}
	// Shares count too; deletes always pass.
	want(t, h.API(t, "POST", "/v1/spaces/"+tm.Space+"/shares", m.Access, map[string]any{"kind": "request", "entityId": "a",
		"snapshot": map[string]any{"schemaVersion": 1, "request": data(80)}}), 413, "quota_exceeded")
	want(t, h.API(t, "POST", sync, m.Access, tu.Changes(tu.Tombstone("request", "a", 2))), 200, "")
	v := h.API(t, "GET", "/v1/me/spaces", o.Access, nil).JSON(t)
	if owner := v["owner"].(map[string]any); owner["quotaBytes"].(float64) != 1000 || owner["bytesUsed"].(float64) < 400 {
		t.Fatalf("owner aggregate: %v", owner)
	}
	// The member's own aggregate is separate (they own no team).
	if owner := h.API(t, "GET", "/v1/me/spaces", m.Access, nil).JSON(t)["owner"].(map[string]any); owner["bytesUsed"].(float64) != 0 {
		t.Fatalf("member aggregate: %v", owner)
	}
}

// ---------------------------------------------------------------- delete and departures

func TestTeams_DeleteCascadesAndSharePagesGo404(t *testing.T) {
	h := teamsHarness(t)
	o := h.QuickUser(t, "o@example.test", "O")
	m := h.QuickUser(t, "m@example.test", "M")
	tm := createTeam(t, h, o.Access, "Acme")
	addMember(t, h, tm.ID, m.User.ID, roles.Editor)
	want(t, h.API(t, "POST", "/v1/spaces/"+tm.Space+"/sync", m.Access, tu.Changes(tu.Change("request", "r1", 1, nil))), 200, "")
	res := h.API(t, "POST", "/v1/spaces/"+tm.Space+"/shares", m.Access, map[string]any{"kind": "collection", "entityId": "c1", "snapshot": tu.SampleCollectionSnapshot()})
	sid := res.JSON(t)["shareId"].(string)
	tok, _ := newInvite(t, h, o.Access, tm.ID, map[string]any{"maxUses": 5})
	h.Call(t, "/v1/invites/ticket", tu.Req{JSON: map[string]any{"token": tok}})
	if r := h.Call(t, "/s/"+sid, tu.Req{}); r.StatusCode != 200 {
		t.Fatalf("share page before: %d", r.StatusCode)
	}

	tick(h)
	want(t, h.API(t, "DELETE", "/v1/teams/"+tm.ID, m.Access, nil), 403, "forbidden_role")
	want(t, h.API(t, "DELETE", "/v1/teams/"+tm.ID, o.Access, nil), 204, "")
	at := h.Clock.Now().UnixMilli()
	for _, c := range []struct{ table, where string }{
		{"teams", "id = ?"}, {"team_members", "team_id = ?"}, {"team_invites", "team_id = ?"},
		{"workspaces", "team_id = ?"}, {"signup_tickets s JOIN team_invites i ON i.id = s.invite_id", "i.team_id = ?"},
	} {
		if n := h.Count(t, c.table, c.where, tm.ID); n != 0 {
			t.Errorf("%s: %d rows left", c.table, n)
		}
	}
	if h.Count(t, "entities", "workspace_id = ?", tm.Space) != 0 || h.Count(t, "shares", "workspace_id = ?", tm.Space) != 0 || h.Count(t, "signup_tickets", "") != 0 {
		t.Fatal("space rows left")
	}
	if r := h.Call(t, "/s/"+sid, tu.Req{}); r.StatusCode != 404 {
		t.Fatalf("share page after: %d", r.StatusCode)
	}
	if r := h.Call(t, "/s/"+sid+".json", tu.Req{}); r.StatusCode != 404 {
		t.Fatalf("share json after: %d", r.StatusCode)
	}
	want(t, h.API(t, "GET", "/v1/spaces/"+tm.Space, m.Access, nil), 404, "space_not_found")
	want(t, h.API(t, "GET", "/v1/teams/"+tm.ID, m.Access, nil), 404, "team_not_found")
	// Every member, the owner too, learns that it was deleted.
	for _, u := range []tu.Tokens{o, m} {
		v := h.API(t, "GET", "/v1/me/spaces", u.Access, nil).JSON(t)
		tu.JSONEq(t, v["departed"], []any{map[string]any{"spaceId": tm.Space, "teamId": tm.ID, "teamName": "Acme", "reason": "deleted", "at": at}})
		if len(v["spaces"].([]any)) != 1 {
			t.Fatalf("spaces after delete: %v", v["spaces"])
		}
	}
}

func TestTeams_DepartedInMySpaces(t *testing.T) {
	h := teamsHarness(t)
	o := h.QuickUser(t, "o@example.test", "O")
	u := h.QuickUser(t, "u@example.test", "U")
	tm := createTeam(t, h, o.Access, "Acme")
	departed := func() []any {
		return h.API(t, "GET", "/v1/me/spaces", u.Access, nil).JSON(t)["departed"].([]any)
	}
	addMember(t, h, tm.ID, u.User.ID, roles.Editor)
	tick(h)
	want(t, h.API(t, "DELETE", "/v1/teams/"+tm.ID+"/members/"+u.User.ID, o.Access, nil), 200, "")
	tu.JSONEq(t, departed(), []any{map[string]any{"spaceId": tm.Space, "teamId": tm.ID, "teamName": "Acme", "reason": "removed", "at": h.Clock.Now().UnixMilli()}})
	tick(h)
	tok, _ := newInvite(t, h, o.Access, tm.ID, nil)
	want(t, acceptInvite(t, h, u.Access, tok), 200, "")
	if len(departed()) != 0 {
		t.Fatal("a member again has no departure")
	}
	tick(h)
	want(t, h.API(t, "POST", "/v1/teams/"+tm.ID+"/leave", u.Access, nil), 204, "")
	if d := departed(); len(d) != 1 || d[0].(map[string]any)["reason"] != "left" {
		t.Fatalf("departed after leave: %v", d)
	}
}

// ---------------------------------------------------------------- signup tickets

func TestInvites_SignupTicketOpensInviteModeSignupOnce(t *testing.T) {
	h := teamsHarness(t) // REQHARBOR_SIGNUP=invite (the default)
	o := h.QuickUser(t, "o@example.test", "O")
	tm := createTeam(t, h, o.Access, "Acme")
	tok, _ := newInvite(t, h, o.Access, tm.ID, map[string]any{"maxUses": 5})

	res := h.Call(t, "/v1/invites/ticket", tu.Req{JSON: map[string]any{"token": tok}})
	want(t, res, 200, "")
	ticket := res.JSON(t)["ticket"].(string)
	if !strings.HasPrefix(ticket, "rht_") || res.JSON(t)["expiresAt"].(float64) != float64(h.Clock.Now().Add(10*time.Minute).UnixMilli()) {
		t.Fatalf("ticket: %s", res.Body)
	}
	want(t, h.Call(t, "/v1/invites/ticket", tu.Req{JSON: map[string]any{"token": "rhi_nope"}}), 410, "invite_invalid")

	// signup opens the pages with the ticket (if any), checks whether the signup form asks for an
	// invite code, and posts the form without one.
	signup := func(ticket, email string, wantCodeField bool) *tu.Resp {
		b := h.Browser()
		p := tu.NewPKCE()
		q := p.Query()
		if ticket != "" {
			q.Set("ticket", ticket)
		}
		page := b.Get(t, "/auth/authorize?"+q.Encode())
		if page.StatusCode != 200 {
			t.Fatalf("authorize: %d %s", page.StatusCode, page.Body)
		}
		ar := tu.AR(t, page)
		if form := b.Get(t, "/auth/signup?ar="+ar); strings.Contains(string(form.Body), `name="code"`) != wantCodeField {
			t.Fatalf("%s: code field %v expected: %s", email, wantCodeField, form.Body)
		}
		return b.Signup(t, ar, "Newbie", email, tu.DefaultPassword, "")
	}
	// Without a ticket, invite mode needs a code.
	if r := signup("", "nobody@example.test", true); r.StatusCode != 400 || !strings.Contains(string(r.Body), "Enter the invite code") {
		t.Fatalf("no ticket: %d", r.StatusCode)
	}
	r := signup(ticket, "first@example.test", false)
	if r.StatusCode != 303 {
		t.Fatalf("ticket signup: %d %s", r.StatusCode, r.Body)
	}
	if h.Count(t, "users", "email = 'first@example.test' AND signup_via = 'invite'") != 1 {
		t.Fatal("ticket signup is an invited account")
	}
	// Single use.
	if r := signup(ticket, "second@example.test", true); r.StatusCode != 400 {
		t.Fatalf("reused ticket: %d", r.StatusCode)
	}
	// Expired.
	res = h.Call(t, "/v1/invites/ticket", tu.Req{JSON: map[string]any{"token": tok}})
	late := res.JSON(t)["ticket"].(string)
	h.Clock.Advance(10*time.Minute + time.Millisecond)
	if r := signup(late, "late@example.test", true); r.StatusCode != 400 {
		t.Fatalf("expired ticket: %d", r.StatusCode)
	}
	// A ticket of a revoked link no longer works.
	res = h.Call(t, "/v1/invites/ticket", tu.Req{JSON: map[string]any{"token": tok}})
	revoked := res.JSON(t)["ticket"].(string)
	h.MustExec(t, "UPDATE team_invites SET revoked_at = 1")
	if r := signup(revoked, "revoked@example.test", true); r.StatusCode != 400 {
		t.Fatalf("revoked link's ticket: %d", r.StatusCode)
	}
	// A malformed ticket is a bad authorize request.
	p := tu.NewPKCE()
	q := p.Query()
	q.Set("ticket", "rht_short")
	if r := h.Browser().Get(t, "/auth/authorize?"+q.Encode()); r.StatusCode != 400 {
		t.Fatalf("malformed ticket: %d", r.StatusCode)
	}
	// The ticket never appears in stored auth requests, only its hash.
	if h.Count(t, "auth_requests", "ticket_hash = ?", ticket) != 0 || h.Count(t, "auth_requests", "ticket_hash = ?", ids.SHA256Hex(ticket)) == 0 {
		t.Fatal("ticket stored in clear")
	}
}

func TestInvites_SignupTicketDoesNotOpenClosedSignup(t *testing.T) {
	h := teamsHarness(t, "REQHARBOR_SIGNUP=closed")
	o := h.QuickUser(t, "o@example.test", "O")
	tm := createTeam(t, h, o.Access, "Acme")
	tok, _ := newInvite(t, h, o.Access, tm.ID, nil)
	ticket := h.Call(t, "/v1/invites/ticket", tu.Req{JSON: map[string]any{"token": tok}}).JSON(t)["ticket"].(string)
	b := h.Browser()
	p := tu.NewPKCE()
	q := p.Query()
	q.Set("ticket", ticket)
	ar := tu.AR(t, b.Get(t, "/auth/authorize?"+q.Encode()))
	if r := b.Signup(t, ar, "N", "n@example.test", tu.DefaultPassword, ""); r.StatusCode != 403 || !strings.Contains(string(r.Body), "accepting new accounts") {
		t.Fatalf("closed: %d %s", r.StatusCode, r.Body)
	}
}

// ---------------------------------------------------------------- /i

func TestInvitePage_CSPAndNoTeamDetailsWithoutScript(t *testing.T) {
	h := teamsHarness(t)
	o := h.QuickUser(t, "o@example.test", "O")
	createTeam(t, h, o.Access, "Secret Project X")
	res := h.Call(t, "/i", tu.Req{})
	want(t, res, 200, "")
	csp := res.Header.Get("Content-Security-Policy")
	wantCSP := "default-src 'none'; style-src 'sha256-" + web.HashBase64(web.AuthCSS) + "'; script-src 'sha256-" + web.HashBase64(web.InviteJS) +
		"'; connect-src 'self'; img-src data:; form-action 'none'; frame-ancestors 'none'; base-uri 'none'"
	if csp != wantCSP {
		t.Fatalf("CSP\n got %s\nwant %s", csp, wantCSP)
	}
	body := string(res.Body)
	if strings.Count(body, "<script>"+web.InviteJS+"</script>") != 1 || strings.Count(body, "<script") != 1 ||
		strings.Count(body, "<style>"+web.AuthCSS+"</style>") != 1 || strings.Count(body, "<style") != 1 {
		t.Fatal("the page must carry exactly the hashed inline style and script")
	}
	for k, v := range map[string]string{"Content-Type": "text/html; charset=utf-8", "Cache-Control": "no-store",
		"X-Robots-Tag": "noindex, nofollow", "X-Frame-Options": "DENY", "Referrer-Policy": "no-referrer"} {
		if res.Header.Get(k) != v {
			t.Errorf("%s: %q", k, res.Header.Get(k))
		}
	}
	if len(res.Cookies()) != 0 || strings.Contains(body, "Secret Project X") || !strings.Contains(body, "Teams › Join team") {
		t.Fatal("the static page holds only the instructions")
	}
	if strings.Contains(strings.ToLower(web.InviteJS), "innerhtml") || strings.Contains(strings.ToLower(web.InviteJS), "</script") {
		t.Fatal("invite.js must only set textContent")
	}
	want(t, h.Call(t, "/i", tu.Req{Method: "POST"}), 405, "method_not_allowed")
}

// ---------------------------------------------------------------- GC

func TestTeams_GCRemovesOldInvitesDeparturesAndTickets(t *testing.T) {
	h := teamsHarness(t)
	o := h.QuickUser(t, "o@example.test", "O")
	u := h.QuickUser(t, "u@example.test", "U")
	tm := createTeam(t, h, o.Access, "Acme")
	tok, _ := newInvite(t, h, o.Access, tm.ID, map[string]any{"expiresInDays": 1, "maxUses": 5})
	h.Call(t, "/v1/invites/ticket", tu.Req{JSON: map[string]any{"token": tok}})
	live, _ := newInvite(t, h, o.Access, tm.ID, map[string]any{"expiresInDays": 30})
	addMember(t, h, tm.ID, u.User.ID, roles.Viewer)
	want(t, h.API(t, "DELETE", "/v1/teams/"+tm.ID+"/members/"+u.User.ID, o.Access, nil), 200, "")

	gc := func() store.GCStats {
		st, err := jobs.GC(context.Background(), h.Store, h.Clock)
		if err != nil {
			t.Fatal(err)
		}
		return st
	}
	h.Clock.Advance(11 * time.Minute)
	if st := gc(); st.Tickets != 1 || st.Invites != 0 || st.Departures != 0 {
		t.Fatalf("after 11 min: %+v", st)
	}
	// Just before 31 days: the 1-day link expired 30 days ago minus a minute; the removal is
	// 31 days old minus a minute. Both stay.
	h.Clock.Advance(31*24*time.Hour - 12*time.Minute)
	if st := gc(); st.Invites != 0 || st.Departures != 0 {
		t.Fatalf("just before 31 days: %+v", st)
	}
	h.Clock.Advance(2 * time.Minute)
	if st := gc(); st.Invites != 1 || st.Departures != 1 {
		t.Fatalf("after 31 days: %+v", st)
	}
	if h.Count(t, "team_invites", "token_hash = ?", ids.SHA256Hex(live)) != 1 {
		t.Fatal("the 30-day link expired a day ago; it is kept 30 days")
	}
}

// The role helpers agree with the product table.
func TestRoles_Rank(t *testing.T) {
	if !(roles.Rank("owner") > roles.Rank("admin") && roles.Rank("admin") > roles.Rank("editor") && roles.Rank("editor") > roles.Rank("viewer") && roles.Rank("x") == 0) {
		t.Fatal("ranks")
	}
	if roles.AtLeast("x", "x") || !roles.AtLeast("admin", "editor") || roles.AtLeast("viewer", "editor") {
		t.Fatal("AtLeast")
	}
}
