package teams

import (
	"errors"
	"fmt"
	"net/http"
	"regexp"
	"time"

	"github.com/mihajlopopara/reqharbor/server/internal/auth"
	"github.com/mihajlopopara/reqharbor/server/internal/clientip"
	"github.com/mihajlopopara/reqharbor/server/internal/clock"
	"github.com/mihajlopopara/reqharbor/server/internal/httpx"
	"github.com/mihajlopopara/reqharbor/server/internal/ids"
	"github.com/mihajlopopara/reqharbor/server/internal/jsonx"
	"github.com/mihajlopopara/reqharbor/server/internal/jsx"
	"github.com/mihajlopopara/reqharbor/server/internal/roles"
	"github.com/mihajlopopara/reqharbor/server/internal/store"
)

// Invite limits (AC-INV-1, 04 §5.6).
const (
	// MaxInviteDays is the longest invite lifetime. team_departures are kept 31 days, so a removal
	// always outlives every link created before it.
	MaxInviteDays     = 30
	DefaultInviteDays = 7
	// MaxInviteUses caps a limited link's uses (the app offers 1, 5 and 25, or unlimited).
	MaxInviteUses = 10000
	// InvitesPerDay is the per-team invite-create limit (30 per day).
	InvitesPerDay = 30
	// TicketTTL is how long a signup ticket lasts.
	TicketTTL = 10 * time.Minute
)

// inviteTokenRe matches an invite token (in the /i# fragment): rhi_ + base64url of 32 random
// bytes. Signup tickets are rht_ tokens (auth.TicketRe).
var inviteTokenRe = regexp.MustCompile(`^rhi_[A-Za-z0-9_-]{43}$`)

func inviteInvalid() *httpx.APIError {
	return httpx.NewError(410, "invite_invalid", "This invite link is no longer valid")
}

func inviteNotFound() *httpx.APIError {
	return httpx.NewError(404, "invite_not_found", "Invite not found")
}

func limitTeams(format string, max int64) *httpx.APIError {
	return httpx.NewError(409, "limit_teams", fmt.Sprintf(format, max))
}

func quotaShortfall(receiver, team string, short int64) *httpx.APIError {
	return httpx.TooLarge("quota_exceeded", fmt.Sprintf("%s doesn't have room for %s: they need %d more bytes", receiver, team, short))
}

// Person names a user; Name is null when the account no longer exists.
type Person struct {
	UserID string  `json:"userId"`
	Name   *string `json:"name"`
}

// InviteJSON is one active invite link. MaxUses is null for an unlimited link.
type InviteJSON struct {
	InviteID  string  `json:"inviteId"`
	Role      string  `json:"role"`
	CreatedAt int64   `json:"createdAt"`
	ExpiresAt int64   `json:"expiresAt"`
	MaxUses   *int64  `json:"maxUses"`
	Uses      int64   `json:"uses"`
	CreatedBy *Person `json:"createdBy"`
}

func inviteJSON(i *store.TeamInvite) InviteJSON {
	out := InviteJSON{InviteID: i.ID, Role: i.Role, CreatedAt: i.CreatedAt, ExpiresAt: i.ExpiresAt, MaxUses: i.MaxUses, Uses: i.Uses}
	if i.CreatedBy != nil {
		out.CreatedBy = &Person{UserID: *i.CreatedBy, Name: i.CreatedByName}
	}
	return out
}

// InviteURL is the link the app shows: PUBLIC_URL/i#token. The token stays in the fragment, so it
// never reaches a server log (AC-INV-2).
func (d *Deps) InviteURL(token string) string { return d.Config.PublicURL + "/i#" + token }

const adminOnlyInvites = "Only admins and owners can manage invite links"

func (d *Deps) listInvites(w http.ResponseWriter, r *http.Request, c *Ctx) error {
	if err := requireRole(c.Me.Role, roles.Admin, adminOnlyInvites); err != nil {
		return err
	}
	out := []InviteJSON{}
	err := d.Store.Read(r.Context(), func(tx store.ReadTx) error {
		rows, err := tx.ActiveTeamInvites(c.Team.ID, d.now())
		if err != nil {
			return err
		}
		for i := range rows {
			out = append(out, inviteJSON(&rows[i]))
		}
		return nil
	})
	if err != nil {
		return err
	}
	return httpx.WriteJSON(w, http.StatusOK, struct {
		Invites []InviteJSON `json:"invites"`
	}{out}, nil)
}

// CreatedInvite is the POST /v1/teams/{id}/invites response: the invite plus its token and link,
// shown only this once.
type CreatedInvite struct {
	InviteJSON
	Token string `json:"token"`
	URL   string `json:"url"`
}

// inviteParams reads {role?, maxUses?, expiresInDays?}: role editor, 1 use and 7 days by default;
// maxUses null is unlimited.
func inviteParams(body *jsonx.Value) (role string, maxUses *int64, days int64, err error) {
	role = roles.Editor
	if v := body.Get("role"); v != nil {
		s, ok := v.Str()
		if !ok || (s != roles.Admin && s != roles.Editor && s != roles.Viewer) {
			return "", nil, 0, httpx.BadRequest("`role` must be admin, editor or viewer")
		}
		role = s
	}
	one := int64(1)
	maxUses = &one
	if v := body.Get("maxUses"); v != nil {
		switch {
		case v.IsNull():
			maxUses = nil
		case v.Kind == jsonx.Number:
			n, ok := jsx.SafeInteger(v.S)
			if !ok || n < 1 || n > MaxInviteUses {
				return "", nil, 0, httpx.BadRequest(fmt.Sprintf("`maxUses` must be an integer from 1 to %d, or null for unlimited", MaxInviteUses))
			}
			maxUses = &n
		default:
			return "", nil, 0, httpx.BadRequest(fmt.Sprintf("`maxUses` must be an integer from 1 to %d, or null for unlimited", MaxInviteUses))
		}
	}
	days = DefaultInviteDays
	if v := body.Get("expiresInDays"); v != nil {
		n, ok := int64(0), false
		if v.Kind == jsonx.Number {
			n, ok = jsx.SafeInteger(v.S)
		}
		if !ok || n < 1 || n > MaxInviteDays {
			return "", nil, 0, httpx.BadRequest(fmt.Sprintf("`expiresInDays` must be an integer from 1 to %d", MaxInviteDays))
		}
		days = n
	}
	return role, maxUses, days, nil
}

// createInvite is POST /v1/teams/{id}/invites {role, maxUses, expiresInDays}: admin or above.
// Admin links only by an owner (403) and only single-use (400; the 0005 CHECK is the backstop).
// At most REQHARBOR_MAX_TEAM_INVITES active links (409 limit_invites) and 30 creates per team per
// day (429 rate_limited).
func (d *Deps) createInvite(w http.ResponseWriter, r *http.Request, c *Ctx) error {
	if err := requireRole(c.Me.Role, roles.Admin, adminOnlyInvites); err != nil {
		return err
	}
	body, err := readBody(w, r)
	if err != nil {
		return err
	}
	role, maxUses, days, err := inviteParams(body)
	if err != nil {
		return err
	}
	if role == roles.Admin && (maxUses == nil || *maxUses != 1) {
		return httpx.BadRequest("Admin links are single-use")
	}
	now := d.Clock.Now()
	ms := clock.Millis(now)
	token := ids.Token("rhi_")
	by := c.uid()
	inv := store.TeamInvite{ID: ids.ULID(now), TeamID: c.Team.ID, TokenHash: ids.SHA256Hex(token), Role: role, CreatedBy: &by,
		CreatedAt: ms, ExpiresAt: ms + days*24*int64(time.Hour/time.Millisecond), MaxUses: maxUses}
	err = d.write(r, c, func(tx store.WriteTx, team *store.Team, me *store.Member) error {
		if err := requireRole(me.Role, roles.Admin, adminOnlyInvites); err != nil {
			return err
		}
		if role == roles.Admin && me.Role != roles.Owner {
			return roles.Forbidden("Only owners can create admin invite links")
		}
		if max := d.Config.MaxTeamInvites; max > 0 {
			active, err := tx.ActiveTeamInvites(team.ID, ms)
			if err != nil {
				return err
			}
			if int64(len(active)) >= max {
				return httpx.NewError(409, "limit_invites", fmt.Sprintf("%s already has %d active invite links. Revoke some first.", team.Name, max))
			}
		}
		n, err := tx.InvitesCreatedSince(team.ID, ms-24*int64(time.Hour/time.Millisecond))
		if err != nil {
			return err
		}
		if n >= InvitesPerDay {
			return httpx.RateLimited("Too many invite links created for this team today; try again tomorrow", 3600)
		}
		name := me.Name
		inv.CreatedByName = &name
		return tx.InsertTeamInvite(inv)
	})
	if err != nil {
		return err
	}
	return httpx.WriteJSON(w, http.StatusCreated, CreatedInvite{InviteJSON: inviteJSON(&inv), Token: token, URL: d.InviteURL(token)}, nil)
}

// RevokedInvite is the DELETE /v1/teams/{id}/invites/{iid} response.
type RevokedInvite struct {
	InviteID  string `json:"inviteId"`
	RevokedAt int64  `json:"revokedAt"`
}

// revokeInvite is DELETE /v1/teams/{id}/invites/{iid}: admin or above; WHERE id AND team_id, so
// another team's invite id is 404 invite_not_found. Idempotent; never reads a body.
func (d *Deps) revokeInvite(w http.ResponseWriter, r *http.Request, c *Ctx) error {
	if err := requireRole(c.Me.Role, roles.Admin, adminOnlyInvites); err != nil {
		return err
	}
	iid := r.PathValue("iid")
	now := d.now()
	out := RevokedInvite{InviteID: iid, RevokedAt: now}
	err := d.write(r, c, func(tx store.WriteTx, team *store.Team, me *store.Member) error {
		if err := requireRole(me.Role, roles.Admin, adminOnlyInvites); err != nil {
			return err
		}
		inv, err := tx.TeamInvite(iid)
		if errors.Is(err, store.ErrNotFound) || (err == nil && inv.TeamID != team.ID) {
			return inviteNotFound()
		}
		if err != nil {
			return err
		}
		if inv.RevokedAt != nil {
			out.RevokedAt = *inv.RevokedAt
			return nil
		}
		_, err = tx.RevokeTeamInvite(iid, team.ID, now)
		return err
	})
	if err != nil {
		return err
	}
	return httpx.WriteJSON(w, http.StatusOK, out, nil)
}

// ---------------------------------------------------------------- /v1/invites/*

// limitIP is the per-IP limit of the unauthenticated invite routes.
func (d *Deps) limitIP(r *http.Request) error {
	if d.InviteLimiter == nil {
		return nil
	}
	if ok, retry := d.InviteLimiter.Allow("invite:" + clientip.Bucket(d.ClientIP.ClientIP(r))); !ok {
		return httpx.RateLimited("Too many invite requests; try again shortly", int64((retry+time.Second-1)/time.Second))
	}
	return nil
}

// tokenOf reads {token} and finds a usable invite of a team; anything else is the one generic 410
// invite_invalid (AC-INV-5).
func (d *Deps) tokenOf(tx store.ReadTx, body *jsonx.Value) (*store.TeamInvite, *store.Team, error) {
	token, ok := body.Get("token").Str()
	if !ok || !inviteTokenRe.MatchString(token) {
		return nil, nil, inviteInvalid()
	}
	inv, err := tx.TeamInviteByHash(ids.SHA256Hex(token))
	if errors.Is(err, store.ErrNotFound) {
		return nil, nil, inviteInvalid()
	}
	if err != nil {
		return nil, nil, err
	}
	t, err := tx.Team(inv.TeamID)
	if errors.Is(err, store.ErrNotFound) || (err == nil && t.Personal) {
		return nil, nil, inviteInvalid()
	}
	if err != nil {
		return nil, nil, err
	}
	return inv, t, nil
}

// Preview is POST /v1/invites/preview.
type Preview struct {
	TeamName    string  `json:"teamName"`
	InviterName *string `json:"inviterName"`
	Role        string  `json:"role"`
	ExpiresAt   int64   `json:"expiresAt"`
}

// preview is POST /v1/invites/preview {token}: no auth (the hosted page and the app before
// sign-in). It tells nothing beyond the team name, the inviter, the role and the expiry.
func (d *Deps) preview(w http.ResponseWriter, r *http.Request) error {
	if err := d.limitIP(r); err != nil {
		return err
	}
	body, err := readBody(w, r)
	if err != nil {
		return err
	}
	var out Preview
	err = d.Store.Read(r.Context(), func(tx store.ReadTx) error {
		inv, t, err := d.tokenOf(tx, body)
		if err != nil {
			return err
		}
		if !inv.Usable(d.now()) {
			return inviteInvalid()
		}
		out = Preview{TeamName: t.Name, InviterName: inv.CreatedByName, Role: inv.Role, ExpiresAt: inv.ExpiresAt}
		return nil
	})
	if err != nil {
		return err
	}
	return httpx.WriteJSON(w, http.StatusOK, out, nil)
}

// Accepted is POST /v1/invites/accept.
type Accepted struct {
	AlreadyMember bool   `json:"alreadyMember"`
	TeamID        string `json:"teamId"`
	TeamName      string `json:"teamName"`
	SpaceID       string `json:"spaceId"`
	Role          string `json:"role"`
}

// accept is POST /v1/invites/accept {token}. A member of the invite's team gets 200
// {alreadyMember:true} with the unchanged role. Otherwise, in one transaction: the link must be
// usable, the caller must not have been removed from the team at or after the link's creation, and
// the team must have room (409 limit_members); then the membership and one use. Any invalid link is
// 410 invite_invalid. Two accepts of a single-use link are serialised: exactly one wins.
func (d *Deps) accept(w http.ResponseWriter, r *http.Request, p *auth.Principal) error {
	body, err := readBody(w, r)
	if err != nil {
		return err
	}
	now := d.now()
	var out Accepted
	err = d.Store.Write(r.Context(), func(tx store.WriteTx) error {
		inv, t, err := d.tokenOf(tx, body)
		if err != nil {
			return err
		}
		out = Accepted{TeamID: t.ID, TeamName: t.Name, SpaceID: t.WorkspaceID, Role: inv.Role}
		if m, err := tx.TeamMember(t.ID, p.User.ID); err == nil {
			out.AlreadyMember, out.Role = true, m.Role
			return nil
		} else if !errors.Is(err, store.ErrNotFound) {
			return err
		}
		if !inv.Usable(now) {
			return inviteInvalid()
		}
		removed, err := tx.RemovedSince(t.ID, p.User.ID, inv.CreatedAt)
		if err != nil {
			return err
		}
		if removed {
			return inviteInvalid()
		}
		n, err := tx.MemberCount(t.ID)
		if err != nil {
			return err
		}
		if max := d.Config.MaxTeamMembers; max > 0 && n >= max {
			return httpx.NewError(409, "limit_members", fmt.Sprintf("%s has reached %d members", t.Name, max))
		}
		if err := tx.InsertMember(t.ID, p.User.ID, inv.Role, now); err != nil {
			return err
		}
		return tx.UseTeamInvite(inv.ID)
	})
	if err != nil {
		return err
	}
	return httpx.WriteJSON(w, http.StatusOK, out, nil)
}

// Ticket is POST /v1/invites/ticket.
type Ticket struct {
	Ticket    string `json:"ticket"`
	ExpiresAt int64  `json:"expiresAt"`
}

// ticket is POST /v1/invites/ticket {token}: no auth. For a usable link it returns a 10-minute,
// single-use signup ticket (rht_…) that the app adds to the authorize URL as &ticket=, so an
// invitee without an account can sign up while REQHARBOR_SIGNUP=invite; the invite token itself
// never appears in a URL (PRIV-4). The ticket only opens signup; joining is still
// POST /v1/invites/accept after sign-in.
func (d *Deps) ticket(w http.ResponseWriter, r *http.Request) error {
	if err := d.limitIP(r); err != nil {
		return err
	}
	body, err := readBody(w, r)
	if err != nil {
		return err
	}
	now := d.Clock.Now()
	tk := ids.Token("rht_")
	out := Ticket{Ticket: tk, ExpiresAt: clock.Millis(now.Add(TicketTTL))}
	err = d.Store.Write(r.Context(), func(tx store.WriteTx) error {
		inv, _, err := d.tokenOf(tx, body)
		if err != nil {
			return err
		}
		if !inv.Usable(clock.Millis(now)) {
			return inviteInvalid()
		}
		// A limited link opens at most as many signups as it has uses left (QA M2 pass 1).
		if inv.MaxUses != nil {
			used, err := tx.UsedSignupTickets(inv.ID)
			if err != nil {
				return err
			}
			if inv.Uses+used >= *inv.MaxUses {
				return inviteInvalid()
			}
		}
		return tx.InsertSignupTicket(store.SignupTicket{TicketHash: ids.SHA256Hex(tk), InviteID: inv.ID, ExpiresAt: out.ExpiresAt})
	})
	if err != nil {
		return err
	}
	return httpx.WriteJSON(w, http.StatusOK, out, nil)
}
