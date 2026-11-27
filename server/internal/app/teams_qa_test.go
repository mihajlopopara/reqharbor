package app_test

// M2 teams, QA pass 1 fixes: roles decided inside the write transactions, ticket caps, ticket
// modes, name and message hardening, per-IP limits on the invite routes and token-free logs.

import (
	"context"
	"strings"
	"sync"
	"testing"

	"github.com/mihajlopopara/reqharbor/server/internal/roles"
	"github.com/mihajlopopara/reqharbor/server/internal/store"
	tu "github.com/mihajlopopara/reqharbor/server/internal/testutil"
)

// hookStore runs a one-shot hook at the start of the next write transaction, after the request
// was authorised: the role changes between authorisation and the write.
type hookStore struct {
	store.Store
	mu   sync.Mutex
	next func()
}

func (s *hookStore) Write(ctx context.Context, fn func(store.WriteTx) error) error {
	s.mu.Lock()
	f := s.next
	s.next = nil
	s.mu.Unlock()
	if f != nil {
		f()
	}
	return s.Store.Write(ctx, fn)
}

func (s *hookStore) beforeNextWrite(f func()) {
	s.mu.Lock()
	s.next = f
	s.mu.Unlock()
}

func hookedTeamsHarness(t *testing.T, env ...string) (*tu.Harness, *hookStore) {
	t.Helper()
	hs := &hookStore{}
	h := tu.New(t, tu.WithEnv(append([]string{"REQHARBOR_TEAMS=true"}, env...)...), tu.WithStoreWrap(func(s store.Store) store.Store {
		hs.Store = s
		return hs
	}))
	return h, hs
}

func setRole(t *testing.T, h *tu.Harness, teamID, userID, role string) func() {
	return func() {
		h.MustExec(t, "UPDATE team_members SET role = ? WHERE team_id = ? AND user_id = ?", role, teamID, userID)
	}
}

func TestTeamSync_RoleIsDecidedInsideThePushTransaction(t *testing.T) {
	h, hs := hookedTeamsHarness(t)
	o := h.QuickUser(t, "o@example.test", "O")
	u := h.QuickUser(t, "u@example.test", "U")
	tm := createTeam(t, h, o.Access, "Acme")
	addMember(t, h, tm.ID, u.User.ID, roles.Admin)
	sync := "/v1/spaces/" + tm.Space + "/sync"
	want(t, h.API(t, "POST", sync, o.Access, tu.Changes(tu.Change("project", "p1", 1, nil), tu.Change("project", "p2", 1, nil))), 200, "")

	// An admin demoted to editor between authorisation and the write: the project tombstone is
	// refused per change.
	hs.beforeNextWrite(setRole(t, h, tm.ID, u.User.ID, roles.Editor))
	r := results(t, h.API(t, "POST", sync, u.Access, tu.Changes(tu.Tombstone("project", "p1", 2), tu.Change("request", "r1", 2, nil))))
	if r[0]["reason"] != "forbidden_role" || r[1]["status"] != "applied" {
		t.Fatalf("demoted admin: %v", r)
	}
	// An editor promoted to admin meanwhile: the tombstone applies.
	hs.beforeNextWrite(setRole(t, h, tm.ID, u.User.ID, roles.Admin))
	setRole(t, h, tm.ID, u.User.ID, roles.Editor)()
	if r := results(t, h.API(t, "POST", sync, u.Access, tu.Changes(tu.Tombstone("project", "p2", 3)))); r[0]["status"] != "applied" {
		t.Fatalf("promoted editor: %v", r)
	}
	// Demoted to viewer meanwhile: the whole push is refused and nothing is stored.
	hs.beforeNextWrite(setRole(t, h, tm.ID, u.User.ID, roles.Viewer))
	want(t, h.API(t, "POST", sync, u.Access, tu.Changes(tu.Change("request", "r2", 4, nil))), 403, "forbidden_role")
	// Removed meanwhile.
	setRole(t, h, tm.ID, u.User.ID, roles.Editor)()
	hs.beforeNextWrite(func() { h.MustExec(t, "DELETE FROM team_members WHERE team_id = ? AND user_id = ?", tm.ID, u.User.ID) })
	want(t, h.API(t, "POST", sync, u.Access, tu.Changes(tu.Change("request", "r2", 4, nil))), 403, "not_a_member")
	if h.Count(t, "entities", "workspace_id = ? AND id = 'r2'", tm.Space) != 0 {
		t.Fatal("a refused push stored")
	}
}

func TestTeamShares_RoleIsDecidedInsideTheShareTransaction(t *testing.T) {
	h, hs := hookedTeamsHarness(t)
	o := h.QuickUser(t, "o@example.test", "O")
	e := h.QuickUser(t, "e@example.test", "E")
	a := h.QuickUser(t, "a@example.test", "A")
	tm := createTeam(t, h, o.Access, "Acme")
	addMember(t, h, tm.ID, e.User.ID, roles.Editor)
	addMember(t, h, tm.ID, a.User.ID, roles.Admin)
	p := "/v1/spaces/" + tm.Space + "/shares"
	body := map[string]any{"kind": "collection", "entityId": "c1", "snapshot": tu.SampleCollectionSnapshot()}

	// Create: an editor demoted to viewer meanwhile.
	hs.beforeNextWrite(setRole(t, h, tm.ID, e.User.ID, roles.Viewer))
	want(t, h.API(t, "POST", p, e.Access, body), 403, "forbidden_role")
	if h.Count(t, "shares", "workspace_id = ?", tm.Space) != 0 {
		t.Fatal("share stored")
	}
	setRole(t, h, tm.ID, e.User.ID, roles.Editor)()
	res := h.API(t, "POST", p, e.Access, body)
	want(t, res, 201, "")
	sid := res.JSON(t)["shareId"].(string)

	// Update of the editor's share by an admin demoted to editor meanwhile.
	hs.beforeNextWrite(setRole(t, h, tm.ID, a.User.ID, roles.Editor))
	want(t, h.API(t, "PUT", p+"/"+sid, a.Access, map[string]any{"snapshot": tu.SampleCollectionSnapshot()}), 403, "forbidden_role")
	// Revoke by an admin removed meanwhile; the share stays.
	setRole(t, h, tm.ID, a.User.ID, roles.Admin)()
	hs.beforeNextWrite(func() { h.MustExec(t, "DELETE FROM team_members WHERE team_id = ? AND user_id = ?", tm.ID, a.User.ID) })
	want(t, h.API(t, "DELETE", p+"/"+sid, a.Access, nil), 403, "not_a_member")
	// Revoke of their own share by an editor demoted to viewer meanwhile.
	hs.beforeNextWrite(setRole(t, h, tm.ID, e.User.ID, roles.Viewer))
	want(t, h.API(t, "DELETE", p+"/"+sid, e.Access, nil), 403, "forbidden_role")
	if h.Count(t, "shares", "id = ? AND revoked_at IS NULL", sid) != 1 {
		t.Fatal("refused revoke revoked")
	}
}

// ticketSignup signs up through the pages with a ticket and no invite code.
func ticketSignup(t *testing.T, h *tu.Harness, ticket, email string) *tu.Resp {
	t.Helper()
	b := h.Browser()
	p := tu.NewPKCE()
	q := p.Query()
	q.Set("ticket", ticket)
	ar := tu.AR(t, b.Get(t, "/auth/authorize?"+q.Encode()))
	return b.Signup(t, ar, "N", email, tu.DefaultPassword, "")
}

func mintTicket(t *testing.T, h *tu.Harness, token string) *tu.Resp {
	t.Helper()
	return h.Call(t, "/v1/invites/ticket", tu.Req{JSON: map[string]any{"token": token}, Headers: map[string]string{"CF-Connecting-IP": tu.RandomIP()}})
}

// A limited link opens at most as many signups as it has uses left: used tickets plus uses must
// stay below max_uses, when minting and when signing up.
func TestInvites_LimitedLinksCapTheirTickets(t *testing.T) {
	h := teamsHarness(t)
	o := h.QuickUser(t, "o@example.test", "O")
	tm := createTeam(t, h, o.Access, "Acme")
	one, _ := newInvite(t, h, o.Access, tm.ID, map[string]any{"maxUses": 1})
	t1 := mintTicket(t, h, one).JSON(t)["ticket"].(string)
	t2 := mintTicket(t, h, one).JSON(t)["ticket"].(string) // nothing spent yet
	if r := ticketSignup(t, h, t1, "first@example.test"); r.StatusCode != 303 {
		t.Fatalf("first: %d", r.StatusCode)
	}
	// The single use is taken by that signup: no more tickets, and the second one is dead.
	want(t, mintTicket(t, h, one), 410, "invite_invalid")
	if r := ticketSignup(t, h, t2, "second@example.test"); r.StatusCode != 400 {
		t.Fatalf("second ticket of a single-use link: %d", r.StatusCode)
	}
	if h.Count(t, "users", "email = 'second@example.test'") != 0 {
		t.Fatal("second account created")
	}
	// The person who signed up with the ticket can still accept the link.
	first := h.SignIn(t, "first@example.test", tu.DefaultPassword)
	want(t, acceptInvite(t, h, first.Access, one), 200, "")

	// A 2-use link: one accept by an existing user plus one ticket signup use it up.
	two, _ := newInvite(t, h, o.Access, tm.ID, map[string]any{"maxUses": 2})
	want(t, acceptInvite(t, h, h.QuickUser(t, "x@example.test", "X").Access, two), 200, "")
	t3 := mintTicket(t, h, two).JSON(t)["ticket"].(string)
	if r := ticketSignup(t, h, t3, "third@example.test"); r.StatusCode != 303 {
		t.Fatalf("third: %d", r.StatusCode)
	}
	want(t, mintTicket(t, h, two), 410, "invite_invalid")
	// Unlimited links keep minting.
	many, _ := newInvite(t, h, o.Access, tm.ID, map[string]any{"maxUses": nil})
	for i := 0; i < 3; i++ {
		want(t, mintTicket(t, h, many), 200, "")
	}
}

// Tickets count only in invite mode: in open mode the account stays an open signup (so the
// REQHARBOR_OPEN_SIGNUP_TOTAL_BYTES class and cap apply) and the ticket is not spent.
func TestInvites_TicketsAreIgnoredInOpenMode(t *testing.T) {
	h := teamsHarness(t, "REQHARBOR_SIGNUP=open")
	o := h.QuickUser(t, "o@example.test", "O")
	tm := createTeam(t, h, o.Access, "Acme")
	tok, _ := newInvite(t, h, o.Access, tm.ID, nil)
	ticket := mintTicket(t, h, tok).JSON(t)["ticket"].(string)
	if r := ticketSignup(t, h, ticket, "open@example.test"); r.StatusCode != 303 {
		t.Fatalf("open signup: %d", r.StatusCode)
	}
	if h.Count(t, "users", "email = 'open@example.test' AND signup_via = 'open'") != 1 || h.Count(t, "signup_tickets", "used_at IS NOT NULL") != 0 {
		t.Fatal("open mode must ignore the ticket")
	}
}

func TestTeams_NamesRejectFormatCharactersAndMessagesQuoteNames(t *testing.T) {
	h := teamsHarness(t, "REQHARBOR_MAX_OWNED_TEAMS=1")
	o := h.QuickUser(t, "o@example.test", "O")
	for _, bad := range []string{"Acme\u202egnp.exe", "\u200bAcme", "A\u2066B", "x\ufeffy"} {
		want(t, h.API(t, "POST", "/v1/teams", o.Access, map[string]any{"name": bad}), 400, "validation_failed")
	}
	tm := createTeam(t, h, o.Access, "Acme")
	want(t, h.API(t, "PATCH", "/v1/teams/"+tm.ID, o.Access, map[string]any{"name": "Ac\u202eme"}), 400, "validation_failed")

	// A member whose name looks like a format string is quoted verbatim.
	m := h.QuickUser(t, "m@example.test", "%d %s %!")
	createTeam(t, h, m.Access, "M's")
	addMember(t, h, tm.ID, m.User.ID, roles.Editor)
	res := h.API(t, "POST", "/v1/teams/"+tm.ID+"/transfer", o.Access, map[string]any{"userId": m.User.ID})
	want(t, res, 409, "limit_teams")
	if msg := res.JSON(t)["error"].(map[string]any)["message"]; msg != "%d %s %! already owns 1 teams, the most this server allows" {
		t.Fatalf("message %q", msg)
	}
}

func TestInvites_PreviewAndTicketAreLimitedPerIP(t *testing.T) {
	h := teamsHarness(t, "REQHARBOR_RATE_AUTH_PER_MIN=2")
	o := h.QuickUser(t, "o@example.test", "O")
	tm := createTeam(t, h, o.Access, "Acme")
	tok, _ := newInvite(t, h, o.Access, tm.ID, map[string]any{"maxUses": nil})
	call := func(path, ip string) *tu.Resp {
		return h.Call(t, path, tu.Req{JSON: map[string]any{"token": tok}, Headers: map[string]string{"CF-Connecting-IP": ip}})
	}
	for _, path := range []string{"/v1/invites/preview", "/v1/invites/ticket"} {
		ip := tu.RandomIP()
		want(t, call(path, ip), 200, "")
		want(t, call(path, ip), 200, "")
		res := call(path, ip)
		want(t, res, 429, "rate_limited")
		if res.Header.Get("Retry-After") == "" {
			t.Fatalf("%s: no Retry-After", path)
		}
		want(t, call(path, tu.RandomIP()), 200, "") // another IP bucket
	}
}

// Invite tokens and signup tickets never reach a log line: links carry them in the fragment, the
// API takes them in bodies, and the access log records route patterns only.
func TestInvites_TokensNeverAppearInLogs(t *testing.T) {
	h := teamsHarness(t)
	o := h.QuickUser(t, "o@example.test", "O")
	u := h.QuickUser(t, "u@example.test", "U")
	tm := createTeam(t, h, o.Access, "Acme")
	tok, iid := newInvite(t, h, o.Access, tm.ID, map[string]any{"maxUses": 5})
	want(t, h.Call(t, "/v1/invites/preview", tu.Req{JSON: map[string]any{"token": tok}}), 200, "")
	ticket := mintTicket(t, h, tok).JSON(t)["ticket"].(string)
	ticketSignup(t, h, ticket, "new@example.test")
	want(t, acceptInvite(t, h, u.Access, tok), 200, "")
	want(t, h.Call(t, "/v1/invites/accept", tu.Req{JSON: map[string]any{"token": tok + "x"}, Token: u.Access}), 410, "invite_invalid")
	want(t, h.Call(t, "/i", tu.Req{}), 200, "")
	want(t, h.API(t, "DELETE", "/v1/teams/"+tm.ID+"/invites/"+iid, o.Access, nil), 200, "")
	logs := h.Logs.String()
	if logs == "" || !strings.Contains(logs, "/v1/invites/accept") {
		t.Fatalf("expected access-log lines, got %q", logs)
	}
	for _, secret := range []string{tok, tok[4:], ticket, ticket[4:]} {
		if strings.Contains(logs, secret) {
			t.Fatalf("a token reached the log: %s", logs)
		}
	}
}
