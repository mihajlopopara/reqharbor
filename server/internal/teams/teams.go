// Package teams serves team management (06-go-server.md §10 "M2 outline", 04-tech-design.md §5.3
// and §5.4): /v1/teams*, /v1/invites/{preview,accept,ticket} and the hosted invite page /i.
//
// Every mutation runs in one write transaction that re-reads the team and the caller's membership,
// so the single serialised writer makes each one atomic: a role change, a removal, a transfer or an
// invite accept either sees the state another one left or runs before it, never in between. That is
// the Go form of 04's guarded single-statement SQL.
package teams

import (
	"errors"
	"fmt"
	"log/slog"
	"net/http"
	"strings"
	"unicode"

	"github.com/mihajlopopara/reqharbor/server/internal/auth"
	"github.com/mihajlopopara/reqharbor/server/internal/clientip"
	"github.com/mihajlopopara/reqharbor/server/internal/clock"
	"github.com/mihajlopopara/reqharbor/server/internal/config"
	"github.com/mihajlopopara/reqharbor/server/internal/httpx"
	"github.com/mihajlopopara/reqharbor/server/internal/ids"
	"github.com/mihajlopopara/reqharbor/server/internal/jsonx"
	"github.com/mihajlopopara/reqharbor/server/internal/jsx"
	"github.com/mihajlopopara/reqharbor/server/internal/ratelimit"
	"github.com/mihajlopopara/reqharbor/server/internal/roles"
	"github.com/mihajlopopara/reqharbor/server/internal/spaces"
	"github.com/mihajlopopara/reqharbor/server/internal/store"
)

// MaxNameUnits is the longest team name, in UTF-16 code units after trimming (AC-TEAM-1: 1–80).
const MaxNameUnits = 80

// Deps are the collaborators of the team routes.
type Deps struct {
	Store    store.Store
	Clock    clock.Clock
	Config   *config.Config
	Auth     spaces.Authenticator
	ClientIP *clientip.Resolver
	Logger   *slog.Logger
	// InviteLimiter limits the unauthenticated POST /v1/invites/preview and /ticket per client IP
	// bucket (REQHARBOR_RATE_AUTH_PER_MIN per minute).
	InviteLimiter *ratelimit.Limiter
}

// Register adds the routes to mux.
func Register(mux *http.ServeMux, d *Deps) {
	mux.Handle("/v1/teams", httpx.Methods{
		{Name: "GET", Handler: d.gated(d.user(d.list))},
		{Name: "POST", Handler: d.gated(d.user(d.create))},
	})
	mux.Handle("/v1/teams/{id}", httpx.Methods{
		{Name: "GET", Handler: d.gated(d.member(d.get))},
		{Name: "PATCH", Handler: d.gated(d.member(d.rename))},
		{Name: "DELETE", Handler: d.gated(d.member(d.deleteTeam))},
	})
	mux.Handle("/v1/teams/{id}/members", httpx.Methods{{Name: "GET", Handler: d.gated(d.member(d.members))}})
	mux.Handle("/v1/teams/{id}/members/{uid}", httpx.Methods{
		{Name: "PATCH", Handler: d.gated(d.member(d.changeRole))},
		{Name: "DELETE", Handler: d.gated(d.member(d.removeMember))},
	})
	mux.Handle("/v1/teams/{id}/transfer", httpx.Methods{{Name: "POST", Handler: d.gated(d.member(d.transfer))}})
	mux.Handle("/v1/teams/{id}/leave", httpx.Methods{{Name: "POST", Handler: d.gated(d.member(d.leave))}})
	mux.Handle("/v1/teams/{id}/invites", httpx.Methods{
		{Name: "GET", Handler: d.gated(d.member(d.listInvites))},
		{Name: "POST", Handler: d.gated(d.member(d.createInvite))},
	})
	mux.Handle("/v1/teams/{id}/invites/{iid}", httpx.Methods{{Name: "DELETE", Handler: d.gated(d.member(d.revokeInvite))}})
	mux.Handle("/v1/invites/preview", httpx.Methods{{Name: "POST", Handler: d.gated(d.preview)}})
	mux.Handle("/v1/invites/accept", httpx.Methods{{Name: "POST", Handler: d.gated(d.user(d.accept))}})
	mux.Handle("/v1/invites/ticket", httpx.Methods{{Name: "POST", Handler: d.gated(d.ticket)}})
	mux.Handle("/i", httpx.Methods{{Name: "GET", Handler: http.HandlerFunc(d.invitePage)}})
}

// ---------------------------------------------------------------- errors

func teamsDisabled() *httpx.APIError {
	return httpx.NewError(403, "teams_disabled", "Teams are not enabled on this server")
}

func teamNotFound() *httpx.APIError { return httpx.NewError(404, "team_not_found", "Team not found") }

func notAMember() *httpx.APIError {
	return httpx.NewError(403, "not_a_member", "You are not a member of this team")
}

func memberNotFound() *httpx.APIError {
	return httpx.NewError(404, "member_not_found", "That user is not a member of this team")
}

func lastOwner() *httpx.APIError {
	return httpx.NewError(409, "last_owner", "A team needs at least one owner")
}

func transferRequired() *httpx.APIError {
	return httpx.NewError(409, "transfer_required", "The team's owner must transfer ownership first")
}

// ---------------------------------------------------------------- plumbing

// gated is the REQHARBOR_TEAMS switch: 403 teams_disabled before auth and before the body.
func (d *Deps) gated(h httpx.Func) httpx.Func {
	return func(w http.ResponseWriter, r *http.Request) error {
		if !d.Config.Teams {
			return teamsDisabled()
		}
		return h(w, r)
	}
}

type userFunc func(w http.ResponseWriter, r *http.Request, p *auth.Principal) error

func (d *Deps) user(h userFunc) httpx.Func {
	return func(w http.ResponseWriter, r *http.Request) error {
		p, err := d.Auth.Authenticate(r)
		if err != nil {
			return err
		}
		return h(w, r, p)
	}
}

// Ctx is an authenticated request about one team the caller belongs to.
type Ctx struct {
	P    *auth.Principal
	Team *store.Team
	Me   *store.Member
}

func (c *Ctx) uid() string { return c.P.User.ID }

type memberFunc func(w http.ResponseWriter, r *http.Request, c *Ctx) error

// load finds team id and userID's membership: an unknown or personal team is 404 team_not_found
// (personal teams are hidden), no membership 403 not_a_member.
func load(tx store.ReadTx, teamID, userID string) (*store.Team, *store.Member, error) {
	t, err := tx.Team(teamID)
	if errors.Is(err, store.ErrNotFound) || (err == nil && t.Personal) {
		return nil, nil, teamNotFound()
	}
	if err != nil {
		return nil, nil, err
	}
	m, err := tx.TeamMember(teamID, userID)
	if errors.Is(err, store.ErrNotFound) {
		return nil, nil, notAMember()
	}
	if err != nil {
		return nil, nil, err
	}
	return t, m, nil
}

// member authenticates and loads the team and the caller's membership. Mutations load both again
// inside their write transaction.
func (d *Deps) member(h memberFunc) httpx.Func {
	return d.user(func(w http.ResponseWriter, r *http.Request, p *auth.Principal) error {
		c := &Ctx{P: p}
		err := d.Store.Read(r.Context(), func(tx store.ReadTx) error {
			var err error
			c.Team, c.Me, err = load(tx, r.PathValue("id"), p.User.ID)
			return err
		})
		if err != nil {
			return err
		}
		return h(w, r, c)
	})
}

// write runs fn in a write transaction after re-loading the team and the caller's membership.
func (d *Deps) write(r *http.Request, c *Ctx, fn func(tx store.WriteTx, team *store.Team, me *store.Member) error) error {
	return d.Store.Write(r.Context(), func(tx store.WriteTx) error {
		team, me, err := load(tx, c.Team.ID, c.uid())
		if err != nil {
			return err
		}
		return fn(tx, team, me)
	})
}

func (d *Deps) now() int64 { return clock.Millis(d.Clock.Now()) }

// requireRole is 403 forbidden_role when role ranks below min.
func requireRole(role, min, message string) error {
	if !roles.AtLeast(role, min) {
		return roles.Forbidden(message)
	}
	return nil
}

// readBody reads a JSON object body.
func readBody(w http.ResponseWriter, r *http.Request) (*jsonx.Value, error) {
	return httpx.ReadJSONObject(w, r, httpx.MaxBodyBytes)
}

// readOptionalBody reads a JSON object body, or returns an empty object when the request has none
// (Content-Length 0).
func readOptionalBody(w http.ResponseWriter, r *http.Request) (*jsonx.Value, error) {
	if r.ContentLength == 0 {
		return &jsonx.Value{Kind: jsonx.Object}, nil
	}
	return readBody(w, r)
}

// teamName validates a team name: a string of 1–80 UTF-16 code units after trimming, without
// control or format (Cf) characters.
func teamName(body *jsonx.Value) (string, error) {
	raw, ok := body.Get("name").Str()
	if !ok {
		return "", httpx.BadRequest("`name` must be a string")
	}
	name := jsx.Trim(raw)
	if n := jsx.UTF16Len(name); n < 1 || n > MaxNameUnits {
		return "", httpx.BadRequest("`name` must be 1 to 80 characters")
	}
	// Control characters, and format characters (unicode.Cf) such as the bidi override U+202E,
	// which could make a team name read as something else in teammates' apps.
	if strings.IndexFunc(name, func(r rune) bool { return unicode.IsControl(r) || unicode.Is(unicode.Cf, r) }) >= 0 {
		return "", httpx.BadRequest("`name` must not contain control or formatting characters")
	}
	return name, nil
}

// ---------------------------------------------------------------- JSON shapes

// TeamJSON describes a team as one member sees it. QuotaBytes is null when unlimited.
type TeamJSON struct {
	TeamID      string `json:"teamId"`
	Name        string `json:"name"`
	SpaceID     string `json:"spaceId"`
	Role        string `json:"role"`
	OwnerUserID string `json:"ownerUserId"`
	MemberCount int64  `json:"memberCount"`
	BytesUsed   int64  `json:"bytesUsed"`
	QuotaBytes  *int64 `json:"quotaBytes"`
	CreatedAt   int64  `json:"createdAt"`
}

func (d *Deps) teamQuota() *int64 {
	if d.Config.QuotaTeamBytes <= 0 {
		return nil
	}
	q := d.Config.QuotaTeamBytes
	return &q
}

func (d *Deps) teamJSON(tx store.ReadTx, t *store.Team, role string, members int64) (*TeamJSON, error) {
	used, err := tx.WorkspaceUsage(t.WorkspaceID)
	if err != nil {
		return nil, err
	}
	if members < 0 {
		if members, err = tx.MemberCount(t.ID); err != nil {
			return nil, err
		}
	}
	return &TeamJSON{TeamID: t.ID, Name: t.Name, SpaceID: t.WorkspaceID, Role: role, OwnerUserID: t.OwnerUserID,
		MemberCount: members, BytesUsed: used, QuotaBytes: d.teamQuota(), CreatedAt: t.CreatedAt}, nil
}

// MemberJSON is one member of a team.
type MemberJSON struct {
	UserID         string  `json:"userId"`
	Name           string  `json:"name"`
	Email          string  `json:"email"`
	AvatarURL      *string `json:"avatarUrl"`
	Role           string  `json:"role"`
	JoinedAt       int64   `json:"joinedAt"`
	IsBillingOwner bool    `json:"isBillingOwner"`
}

func memberJSON(m *store.Member, t *store.Team) MemberJSON {
	return MemberJSON{UserID: m.UserID, Name: m.Name, Email: m.Email, AvatarURL: m.AvatarURL, Role: m.Role,
		JoinedAt: m.JoinedAt, IsBillingOwner: m.UserID == t.OwnerUserID}
}

// limitOrNull is a REQHARBOR_MAX_* value, null when 0 (no cap).
func limitOrNull(v int64) *int64 {
	if v <= 0 {
		return nil
	}
	return &v
}

// ---------------------------------------------------------------- /v1/teams

// ListResponse is GET /v1/teams.
type ListResponse struct {
	Teams      []TeamJSON   `json:"teams"`
	OwnedCount int64        `json:"ownedCount"`
	Limits     Limits       `json:"limits"`
	Owner      spaces.Usage `json:"owner"`
}

// Limits are the team limits of this server; null means no cap.
type Limits struct {
	OwnedTeams  *int64 `json:"ownedTeams"`
	TeamMembers *int64 `json:"teamMembers"`
	TeamInvites *int64 `json:"teamInvites"`
}

func (d *Deps) list(w http.ResponseWriter, r *http.Request, p *auth.Principal) error {
	out := ListResponse{Teams: []TeamJSON{}, Owner: spaces.Usage{QuotaBytes: spaces.OwnerQuota(d.Config)},
		Limits: Limits{OwnedTeams: limitOrNull(d.Config.MaxOwnedTeams), TeamMembers: limitOrNull(d.Config.MaxTeamMembers),
			TeamInvites: limitOrNull(d.Config.MaxTeamInvites)}}
	err := d.Store.Read(r.Context(), func(tx store.ReadTx) error {
		rows, err := tx.UserTeams(p.User.ID)
		if err != nil {
			return err
		}
		for i := range rows {
			tj, err := d.teamJSON(tx, &rows[i].Team, rows[i].Role, rows[i].MemberCount)
			if err != nil {
				return err
			}
			out.Teams = append(out.Teams, *tj)
		}
		if out.OwnedCount, err = tx.OwnedTeamCount(p.User.ID); err != nil {
			return err
		}
		out.Owner.BytesUsed, err = tx.OwnerUsage(p.User.ID)
		return err
	})
	if err != nil {
		return err
	}
	return httpx.WriteJSON(w, http.StatusOK, out, nil)
}

// create is POST /v1/teams {name}: the team, the caller's owner membership and the empty team
// space, in one transaction. At most REQHARBOR_MAX_OWNED_TEAMS non-personal teams per billing
// owner (409 limit_teams; the hidden personal team is not counted).
func (d *Deps) create(w http.ResponseWriter, r *http.Request, p *auth.Principal) error {
	body, err := readBody(w, r)
	if err != nil {
		return err
	}
	name, err := teamName(body)
	if err != nil {
		return err
	}
	now := d.Clock.Now()
	t := store.Team{ID: ids.ULID(now), Name: name, OwnerUserID: p.User.ID, CreatedAt: clock.Millis(now), WorkspaceID: ids.ULID(now)}
	var out *TeamJSON
	err = d.Store.Write(r.Context(), func(tx store.WriteTx) error {
		if max := d.Config.MaxOwnedTeams; max > 0 {
			n, err := tx.OwnedTeamCount(p.User.ID)
			if err != nil {
				return err
			}
			if n >= max {
				return limitTeams("You can own up to %d teams on this server", max)
			}
		}
		if err := tx.InsertTeam(t, t.CreatedAt); err != nil {
			return err
		}
		out, err = d.teamJSON(tx, &t, roles.Owner, 1)
		return err
	})
	if err != nil {
		return err
	}
	return httpx.WriteJSON(w, http.StatusCreated, out, nil)
}

func (d *Deps) get(w http.ResponseWriter, r *http.Request, c *Ctx) error {
	var out *TeamJSON
	err := d.Store.Read(r.Context(), func(tx store.ReadTx) error {
		var err error
		out, err = d.teamJSON(tx, c.Team, c.Me.Role, -1)
		return err
	})
	if err != nil {
		return err
	}
	return httpx.WriteJSON(w, http.StatusOK, out, nil)
}

// rename is PATCH /v1/teams/{id} {name}: admin or above. The team space takes the same name.
func (d *Deps) rename(w http.ResponseWriter, r *http.Request, c *Ctx) error {
	const msg = "Only admins and owners can rename the team"
	if err := requireRole(c.Me.Role, roles.Admin, msg); err != nil {
		return err
	}
	body, err := readBody(w, r)
	if err != nil {
		return err
	}
	name, err := teamName(body)
	if err != nil {
		return err
	}
	var out *TeamJSON
	err = d.write(r, c, func(tx store.WriteTx, team *store.Team, me *store.Member) error {
		if err := requireRole(me.Role, roles.Admin, msg); err != nil {
			return err
		}
		if err := tx.RenameTeam(team.ID, name); err != nil {
			return err
		}
		team.Name = name
		out, err = d.teamJSON(tx, team, me.Role, -1)
		return err
	})
	if err != nil {
		return err
	}
	return httpx.WriteJSON(w, http.StatusOK, out, nil)
}

// deleteTeam is DELETE /v1/teams/{id}: the billing owner only. Every member (the caller too) gets
// a `deleted` departure; the team's memberships, invites, space, entities and shares cascade, so
// its share pages answer 404. 204.
func (d *Deps) deleteTeam(w http.ResponseWriter, r *http.Request, c *Ctx) error {
	const msg = "Only the team's owner can delete it"
	if c.Team.OwnerUserID != c.uid() {
		return roles.Forbidden(msg)
	}
	now := d.now()
	err := d.write(r, c, func(tx store.WriteTx, team *store.Team, me *store.Member) error {
		if team.OwnerUserID != me.UserID {
			return roles.Forbidden(msg)
		}
		members, err := tx.TeamMembers(team.ID)
		if err != nil {
			return err
		}
		by := me.UserID
		for _, m := range members {
			if err := tx.InsertDeparture(store.Departure{TeamID: team.ID, UserID: m.UserID, SpaceID: team.WorkspaceID,
				TeamName: team.Name, Reason: store.DepartureDeleted, At: now, ByUserID: &by}); err != nil {
				return err
			}
		}
		return tx.DeleteTeam(team.ID)
	})
	if err != nil {
		return err
	}
	w.WriteHeader(http.StatusNoContent)
	return nil
}

// ---------------------------------------------------------------- members

func (d *Deps) members(w http.ResponseWriter, r *http.Request, c *Ctx) error {
	out := []MemberJSON{}
	err := d.Store.Read(r.Context(), func(tx store.ReadTx) error {
		rows, err := tx.TeamMembers(c.Team.ID)
		if err != nil {
			return err
		}
		for i := range rows {
			out = append(out, memberJSON(&rows[i], c.Team))
		}
		return nil
	})
	if err != nil {
		return err
	}
	return httpx.WriteJSON(w, http.StatusOK, struct {
		Members []MemberJSON `json:"members"`
	}{out}, nil)
}

// target loads the member uid of team: 404 member_not_found when it is not one.
func target(tx store.ReadTx, team *store.Team, uid string) (*store.Member, error) {
	m, err := tx.TeamMember(team.ID, uid)
	if errors.Is(err, store.ErrNotFound) {
		return nil, memberNotFound()
	}
	return m, err
}

// MayChangeRole is the role-change rule (04 §5.4, AC-TEAM-3): owners change anyone to any role;
// admins change only editors and viewers, and grant at most admin; anyone may lower their own role.
func MayChangeRole(actor, actorID, targetRole, targetID, newRole string) bool {
	switch {
	case actor == roles.Owner:
		return true
	case actor == roles.Admin && roles.Rank(targetRole) < roles.Rank(roles.Admin) && roles.Rank(newRole) <= roles.Rank(roles.Admin):
		return true
	case actorID == targetID && roles.Rank(newRole) < roles.Rank(targetRole):
		return true
	}
	return false
}

// MayRemove is the removal rule: owners remove anyone else; admins only editors and viewers.
// Leaving is POST …/leave.
func MayRemove(actor, targetRole string) bool {
	return actor == roles.Owner || (actor == roles.Admin && roles.Rank(targetRole) < roles.Rank(roles.Admin))
}

// changeRole is PATCH /v1/teams/{id}/members/{uid} {role}. Checks, in order: the member (404),
// the rule (403 forbidden_role), an unchanged role (200, nothing written), the last owner (409
// last_owner), the billing owner (409 transfer_required). A demotion below admin revokes the
// invite links the member created in this team, in the same transaction (QA: invite abuse).
func (d *Deps) changeRole(w http.ResponseWriter, r *http.Request, c *Ctx) error {
	body, err := readBody(w, r)
	if err != nil {
		return err
	}
	newRole, ok := body.Get("role").Str()
	if !ok || !roles.Valid(newRole) {
		return httpx.BadRequest("`role` must be one of owner, admin, editor, viewer")
	}
	uid := r.PathValue("uid")
	now := d.now()
	var out MemberJSON
	err = d.write(r, c, func(tx store.WriteTx, team *store.Team, me *store.Member) error {
		t, err := target(tx, team, uid)
		if err != nil {
			return err
		}
		if !MayChangeRole(me.Role, me.UserID, t.Role, t.UserID, newRole) {
			return roles.Forbidden("Your role can't give this member that role")
		}
		if newRole != t.Role {
			if t.Role == roles.Owner {
				owners, err := tx.OwnerCount(team.ID)
				if err != nil {
					return err
				}
				if owners <= 1 {
					return lastOwner()
				}
			}
			if t.UserID == team.OwnerUserID {
				return transferRequired()
			}
			if err := tx.SetMemberRole(team.ID, t.UserID, newRole); err != nil {
				return err
			}
			if roles.Rank(t.Role) >= roles.Rank(roles.Admin) && roles.Rank(newRole) < roles.Rank(roles.Admin) {
				if _, err := tx.RevokeTeamInvitesBy(team.ID, t.UserID, now); err != nil {
					return err
				}
			}
			t.Role = newRole
		}
		out = memberJSON(t, team)
		return nil
	})
	if err != nil {
		return err
	}
	return httpx.WriteJSON(w, http.StatusOK, out, nil)
}

// RemoveResponse is DELETE /v1/teams/{id}/members/{uid}.
type RemoveResponse struct {
	UserID         string `json:"userId"`
	RevokedInvites int64  `json:"revokedInvites"`
}

// removeMember is DELETE /v1/teams/{id}/members/{uid} with an optional body
// {"revokeInvites": bool}. Checks: yourself (403: use leave), the member (404), the rule (403),
// the last owner (409), the billing owner (409 transfer_required). One transaction records the
// `removed` departure (which blocks invite links created before it for this user), deletes the
// membership, revokes every invite link the member created in this team whatever the body says,
// and with revokeInvites every other active link of the team too.
func (d *Deps) removeMember(w http.ResponseWriter, r *http.Request, c *Ctx) error {
	uid := r.PathValue("uid")
	if uid == c.uid() {
		return roles.Forbidden("To leave the team, use leave")
	}
	if err := requireRole(c.Me.Role, roles.Admin, "Only admins and owners can remove members"); err != nil {
		return err
	}
	body, err := readOptionalBody(w, r)
	if err != nil {
		return err
	}
	all := false
	if v := body.Get("revokeInvites"); v != nil && !v.IsNull() {
		if v.Kind != jsonx.Bool {
			return httpx.BadRequest("`revokeInvites` must be a boolean")
		}
		all = v.B
	}
	now := d.now()
	out := RemoveResponse{UserID: uid}
	err = d.write(r, c, func(tx store.WriteTx, team *store.Team, me *store.Member) error {
		t, err := target(tx, team, uid)
		if err != nil {
			return err
		}
		if !MayRemove(me.Role, t.Role) {
			return roles.Forbidden("Your role can't remove this member")
		}
		if t.Role == roles.Owner {
			owners, err := tx.OwnerCount(team.ID)
			if err != nil {
				return err
			}
			if owners <= 1 {
				return lastOwner()
			}
		}
		if t.UserID == team.OwnerUserID {
			return transferRequired()
		}
		by := me.UserID
		if err := tx.InsertDeparture(store.Departure{TeamID: team.ID, UserID: t.UserID, SpaceID: team.WorkspaceID,
			TeamName: team.Name, Reason: store.DepartureRemoved, At: now, ByUserID: &by}); err != nil {
			return err
		}
		if err := tx.DeleteMember(team.ID, t.UserID); err != nil {
			return err
		}
		n, err := tx.RevokeTeamInvitesBy(team.ID, t.UserID, now)
		if err != nil {
			return err
		}
		out.RevokedInvites = n
		if all {
			n, err := tx.RevokeAllTeamInvites(team.ID, now)
			if err != nil {
				return err
			}
			out.RevokedInvites += n
		}
		return nil
	})
	if err != nil {
		return err
	}
	return httpx.WriteJSON(w, http.StatusOK, out, nil)
}

// transfer is POST /v1/teams/{id}/transfer {userId}: the billing owner hands the team to another
// member, who becomes owner and billing owner; the caller becomes admin. Checks: the caller is the
// billing owner (403), not yourself (400), the member (404), the receiver's owned-team count (409
// limit_teams), the receiver's aggregate with REQHARBOR_QUOTA_OWNER_BYTES (413 quota_exceeded,
// naming the shortfall). All in one transaction, so a removal racing it either runs first (404)
// or finds the caller no longer owner (403); the team never loses its last owner.
func (d *Deps) transfer(w http.ResponseWriter, r *http.Request, c *Ctx) error {
	const msg = "Only the team's owner can transfer ownership"
	if c.Team.OwnerUserID != c.uid() {
		return roles.Forbidden(msg)
	}
	body, err := readBody(w, r)
	if err != nil {
		return err
	}
	uid, ok := body.Get("userId").Str()
	if !ok || uid == "" {
		return httpx.BadRequest("`userId` must be a member's user id")
	}
	if uid == c.uid() {
		return httpx.BadRequest("You already own this team")
	}
	var out *TeamJSON
	err = d.write(r, c, func(tx store.WriteTx, team *store.Team, me *store.Member) error {
		if team.OwnerUserID != me.UserID {
			return roles.Forbidden(msg)
		}
		t, err := target(tx, team, uid)
		if err != nil {
			return err
		}
		if max := d.Config.MaxOwnedTeams; max > 0 {
			n, err := tx.OwnedTeamCount(t.UserID)
			if err != nil {
				return err
			}
			if n >= max {
				return httpx.NewError(409, "limit_teams", fmt.Sprintf("%s already owns %d teams, the most this server allows", t.Name, max))
			}
		}
		if q := d.Config.QuotaOwnerBytes; q > 0 {
			theirs, err := tx.OwnerUsage(t.UserID)
			if err != nil {
				return err
			}
			used, err := tx.WorkspaceUsage(team.WorkspaceID)
			if err != nil {
				return err
			}
			if theirs+used > q {
				return quotaShortfall(t.Name, team.Name, theirs+used-q)
			}
		}
		if err := tx.SetMemberRole(team.ID, t.UserID, roles.Owner); err != nil {
			return err
		}
		if err := tx.SetTeamOwner(team.ID, t.UserID); err != nil {
			return err
		}
		if err := tx.SetMemberRole(team.ID, me.UserID, roles.Admin); err != nil {
			return err
		}
		team.OwnerUserID = t.UserID
		out, err = d.teamJSON(tx, team, roles.Admin, -1)
		return err
	})
	if err != nil {
		return err
	}
	return httpx.WriteJSON(w, http.StatusOK, out, nil)
}

// leave is POST /v1/teams/{id}/leave: any member except the last owner (409 last_owner) and the
// billing owner (409 transfer_required). It records a `left` departure, which does not block
// rejoining, and revokes the invite links the member created in this team. 204.
func (d *Deps) leave(w http.ResponseWriter, r *http.Request, c *Ctx) error {
	now := d.now()
	err := d.write(r, c, func(tx store.WriteTx, team *store.Team, me *store.Member) error {
		if me.Role == roles.Owner {
			owners, err := tx.OwnerCount(team.ID)
			if err != nil {
				return err
			}
			if owners <= 1 {
				return lastOwner()
			}
		}
		if team.OwnerUserID == me.UserID {
			return transferRequired()
		}
		by := me.UserID
		if err := tx.InsertDeparture(store.Departure{TeamID: team.ID, UserID: me.UserID, SpaceID: team.WorkspaceID,
			TeamName: team.Name, Reason: store.DepartureLeft, At: now, ByUserID: &by}); err != nil {
			return err
		}
		if err := tx.DeleteMember(team.ID, me.UserID); err != nil {
			return err
		}
		_, err := tx.RevokeTeamInvitesBy(team.ID, me.UserID, now)
		return err
	})
	if err != nil {
		return err
	}
	w.WriteHeader(http.StatusNoContent)
	return nil
}
