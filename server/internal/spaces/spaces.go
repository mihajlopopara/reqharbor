// Package spaces serves the signed-in account routes (06-go-server.md §4, S6): /v1/me,
// /v1/me/sessions, /v1/me/spaces, and the account spaces under /v1/spaces/{id} with their sync
// and shares. Authentication is the access token (auth.Service.Authenticate); authorisation is the
// space membership.
package spaces

import (
	"errors"
	"net/http"
	"regexp"
	"sort"

	"github.com/mihajlopopara/reqharbor/server/internal/auth"
	"github.com/mihajlopopara/reqharbor/server/internal/clock"
	"github.com/mihajlopopara/reqharbor/server/internal/config"
	"github.com/mihajlopopara/reqharbor/server/internal/httpx"
	"github.com/mihajlopopara/reqharbor/server/internal/jobs"
	"github.com/mihajlopopara/reqharbor/server/internal/ratelimit"
	"github.com/mihajlopopara/reqharbor/server/internal/shares"
	"github.com/mihajlopopara/reqharbor/server/internal/store"
	"github.com/mihajlopopara/reqharbor/server/internal/syncsvc"
)

// Authenticator resolves an access token; *auth.Service implements it.
type Authenticator interface {
	Authenticate(r *http.Request) (*auth.Principal, error)
}

// Deps are the collaborators of the account routes.
type Deps struct {
	Store   store.Store
	Clock   clock.Clock
	Config  *config.Config
	Auth    Authenticator
	Breaker *jobs.Breaker
	Shares  *shares.Service
	// SyncLimiter is REQHARBOR_RATE_SYNC_PER_MIN per session; ShareLimiter is the share-create
	// limit per space (shared with legacy workspaces, keyed by id).
	SyncLimiter  *ratelimit.Limiter
	ShareLimiter *ratelimit.Limiter
	// Epoch returns the current sync epoch for pull responses.
	Epoch func() string
}

// Space kinds and the M1 role names.
const (
	KindPersonal = "personal"
	KindTeam     = "team"
)

const dayMillis = 24 * 60 * 60 * 1000

var sharePathRe = regexp.MustCompile(`^[A-Za-z0-9]{1,64}$`)

func spaceNotFound() *httpx.APIError {
	return httpx.NewError(404, "space_not_found", "Space not found")
}

func notAMember() *httpx.APIError {
	return httpx.NewError(403, "not_a_member", "You are not a member of this space")
}

func serverFull(message string) *httpx.APIError { return httpx.NewError(507, "server_full", message) }

// Register adds the routes to mux.
func Register(mux *http.ServeMux, d *Deps) {
	mux.Handle("/v1/me", httpx.Methods{{Name: "GET", Handler: d.user(d.me)}})
	mux.Handle("/v1/me/sessions", httpx.Methods{{Name: "GET", Handler: d.user(d.sessions)}})
	mux.Handle("/v1/me/sessions/{id}", httpx.Methods{{Name: "DELETE", Handler: d.user(d.revokeSession)}})
	mux.Handle("/v1/me/spaces", httpx.Methods{{Name: "GET", Handler: d.user(d.mySpaces)}})

	mux.Handle("/v1/spaces/{id}", httpx.Methods{{Name: "GET", Handler: d.space(d.getSpace)}})
	mux.Handle("/v1/spaces/{id}/sync", httpx.Methods{
		{Name: "GET", Handler: d.space(d.limitSync(d.pull))},
		{Name: "POST", Handler: d.space(d.limitSync(d.push))},
	})
	mux.Handle("/v1/spaces/{id}/shares", httpx.Methods{
		{Name: "GET", Handler: d.space(d.listShares)},
		{Name: "POST", Handler: d.space(d.createShare)},
	})
	shareByID := httpx.Methods{
		{Name: "PUT", Handler: d.space(d.updateShare)},
		{Name: "DELETE", Handler: d.space(d.revokeShare)},
	}
	mux.Handle("/v1/spaces/{id}/shares/{sid}", http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if !sharePathRe.MatchString(r.PathValue("sid")) {
			httpx.WriteError(w, httpx.RouteNotFound(r)) // a route 404, as for legacy share ids
			return
		}
		shareByID.ServeHTTP(w, r)
	}))
}

type userFunc func(w http.ResponseWriter, r *http.Request, p *auth.Principal) error

// user authenticates the access token.
func (d *Deps) user(h userFunc) httpx.Func {
	return func(w http.ResponseWriter, r *http.Request) error {
		p, err := d.Auth.Authenticate(r)
		if err != nil {
			return err
		}
		return h(w, r, p)
	}
}

// Access is an authorised request to one space.
type Access struct {
	Principal *auth.Principal
	Space     *store.SpaceAccess
}

func (a *Access) id() string { return a.Space.Workspace.ID }

type spaceFunc func(w http.ResponseWriter, r *http.Request, a *Access) error

// space applies, in order: the REQHARBOR_MIN_CLIENT_VERSION gate (426), the access token (401),
// then space authorisation (§4): an unknown id or a legacy workspace is 404 space_not_found, no
// membership is 403 not_a_member. Only then does the handler read a body.
func (d *Deps) space(h spaceFunc) httpx.Func {
	return func(w http.ResponseWriter, r *http.Request) error {
		if err := httpx.CheckClientVersion(r, d.Config.MinClientVersion); err != nil {
			return err
		}
		p, err := d.Auth.Authenticate(r)
		if err != nil {
			return err
		}
		var acc *store.SpaceAccess
		err = d.Store.Read(r.Context(), func(tx store.ReadTx) error {
			var err error
			acc, err = tx.SpaceAccess(r.PathValue("id"), p.User.ID)
			return err
		})
		switch {
		case errors.Is(err, store.ErrNotFound):
			return spaceNotFound()
		case err != nil:
			return err
		case acc.TeamID == nil:
			return spaceNotFound() // legacy workspaces are never addressable through /v1/spaces
		case acc.Role == nil:
			return notAMember()
		}
		return h(w, r, &Access{Principal: p, Space: acc})
	}
}

// limitSync is REQHARBOR_RATE_SYNC_PER_MIN per session.
func (d *Deps) limitSync(h spaceFunc) spaceFunc {
	return func(w http.ResponseWriter, r *http.Request, a *Access) error {
		if d.SyncLimiter != nil {
			if ok, retry := d.SyncLimiter.Allow("sync:" + a.Principal.Session.ID); !ok {
				return httpx.RateLimited("Too many sync requests from this device; try again shortly", int64((retry+999_999_999)/1_000_000_000))
			}
		}
		return h(w, r, a)
	}
}

// kind is the space kind of a team.
func kind(personal bool) string {
	if personal {
		return KindPersonal
	}
	return KindTeam
}

// quota is the configured quota of a space kind; nil means unlimited (a 0 setting).
func (d *Deps) quota(personal bool) *int64 {
	q := d.Config.QuotaTeamBytes
	if personal {
		q = d.Config.QuotaPersonalBytes
	}
	if q <= 0 {
		return nil
	}
	return &q
}

func (d *Deps) limits(acc *store.SpaceAccess) syncsvc.Limits {
	var q int64
	if p := d.quota(acc.Personal); p != nil {
		q = *p
	}
	return syncsvc.Limits{QuotaBytes: q, Growth: d.growth(acc)}
}

// growth is the breaker check of a growing write to a space: the server-wide breaker, then the
// open-signup total for personal spaces of open-signup accounts.
func (d *Deps) growth(acc *store.SpaceAccess) func() error {
	return func() error {
		if d.Breaker == nil {
			return nil
		}
		if d.Breaker.Open() {
			return serverFull("This server is out of storage space; try again later")
		}
		if acc.Personal && acc.OwnerSignupVia == "open" && d.Breaker.OpenSignupFull() {
			return serverFull("This server has reached its storage limit for self-registered accounts; ask its admin")
		}
		return nil
	}
}

// ---------------------------------------------------------------- /v1/me

// MeResponse is GET /v1/me.
type MeResponse struct {
	User      auth.UserJSON `json:"user"`
	Method    string        `json:"method"`
	SessionID string        `json:"sessionId"`
}

func (d *Deps) me(w http.ResponseWriter, r *http.Request, p *auth.Principal) error {
	method := ""
	err := d.Store.Read(r.Context(), func(tx store.ReadTx) error {
		_, err := tx.PasswordPHC(p.User.ID)
		if err == nil {
			method = "password"
			return nil
		}
		if !errors.Is(err, store.ErrNotFound) {
			return err
		}
		id, err := tx.IdentityOf(p.User.ID)
		if err != nil {
			return err
		}
		method = id.Provider
		return nil
	})
	if err != nil {
		return err
	}
	return httpx.WriteJSON(w, http.StatusOK, MeResponse{User: auth.UserView(p.User), Method: method, SessionID: p.Session.ID}, nil)
}

// SessionJSON is one row of GET /v1/me/sessions. LastUsedDay is Unix ms at 00:00 UTC of the day
// the session was last used.
type SessionJSON struct {
	ID          string `json:"id"`
	DeviceName  string `json:"deviceName"`
	CreatedAt   int64  `json:"createdAt"`
	LastUsedDay int64  `json:"lastUsedDay"`
	Current     bool   `json:"current"`
}

// sessions lists the user's live sessions (not revoked, refresh not expired), newest first.
func (d *Deps) sessions(w http.ResponseWriter, r *http.Request, p *auth.Principal) error {
	var rows []store.Session
	err := d.Store.Read(r.Context(), func(tx store.ReadTx) error {
		var err error
		rows, err = tx.UserSessions(p.User.ID)
		return err
	})
	if err != nil {
		return err
	}
	now := clock.Millis(d.Clock.Now())
	out := []SessionJSON{}
	for _, s := range rows {
		if s.RevokedAt != nil || s.ExpiresAt <= now || s.IdleExpiresAt <= now {
			continue
		}
		out = append(out, SessionJSON{ID: s.ID, DeviceName: s.DeviceName, CreatedAt: s.CreatedAt,
			LastUsedDay: s.LastUsedDay * dayMillis, Current: s.ID == p.Session.ID})
	}
	sort.SliceStable(out, func(i, j int) bool {
		if out[i].CreatedAt != out[j].CreatedAt {
			return out[i].CreatedAt > out[j].CreatedAt
		}
		return out[i].ID > out[j].ID
	})
	return httpx.WriteJSON(w, http.StatusOK, struct {
		Sessions []SessionJSON `json:"sessions"`
	}{out}, nil)
}

// revokeSession is DELETE /v1/me/sessions/{id}: WHERE id=? AND user_id=?me. Another user's id, an
// unknown id or an already revoked session is 404. Revoking the current session is allowed (it is
// logout). 204 on success.
func (d *Deps) revokeSession(w http.ResponseWriter, r *http.Request, p *auth.Principal) error {
	var ok bool
	err := d.Store.Write(r.Context(), func(tx store.WriteTx) error {
		var err error
		ok, err = tx.RevokeUserSession(r.PathValue("id"), p.User.ID, clock.Millis(d.Clock.Now()))
		return err
	})
	if err != nil {
		return err
	}
	if !ok {
		return httpx.NotFound("Session not found")
	}
	w.WriteHeader(http.StatusNoContent)
	return nil
}

// MySpace is one entry of GET /v1/me/spaces. TeamID and TeamName are null for the personal
// space; QuotaBytes is null when unlimited.
type MySpace struct {
	SpaceID    string  `json:"spaceId"`
	Kind       string  `json:"kind"`
	TeamID     *string `json:"teamId"`
	TeamName   *string `json:"teamName"`
	Role       string  `json:"role"`
	BytesUsed  int64   `json:"bytesUsed"`
	QuotaBytes *int64  `json:"quotaBytes"`
}

// Departed is a space the user left or lost (M2 fills it; always empty in M1).
type Departed struct {
	SpaceID  string  `json:"spaceId"`
	TeamID   *string `json:"teamId"`
	TeamName *string `json:"teamName"`
	Reason   string  `json:"reason"`
	At       int64   `json:"at"`
}

// MySpaces is GET /v1/me/spaces: the authoritative membership list (04 §6.7).
type MySpaces struct {
	Spaces   []MySpace  `json:"spaces"`
	Departed []Departed `json:"departed"`
}

func (d *Deps) mySpaces(w http.ResponseWriter, r *http.Request, p *auth.Principal) error {
	out := MySpaces{Spaces: []MySpace{}, Departed: []Departed{}}
	err := d.Store.Read(r.Context(), func(tx store.ReadTx) error {
		rows, err := tx.UserSpaces(p.User.ID)
		if err != nil {
			return err
		}
		for _, s := range rows {
			used, err := tx.WorkspaceUsage(s.WorkspaceID)
			if err != nil {
				return err
			}
			m := MySpace{SpaceID: s.WorkspaceID, Kind: kind(s.Personal), Role: s.Role, BytesUsed: used, QuotaBytes: d.quota(s.Personal)}
			if !s.Personal {
				teamID, teamName := s.TeamID, s.TeamName
				m.TeamID, m.TeamName = &teamID, &teamName
			}
			out.Spaces = append(out.Spaces, m)
		}
		return nil
	})
	if err != nil {
		return err
	}
	return httpx.WriteJSON(w, http.StatusOK, out, nil)
}

// ---------------------------------------------------------------- /v1/spaces/{id}

// SpaceJSON is GET /v1/spaces/{id}.
type SpaceJSON struct {
	SpaceID     string `json:"spaceId"`
	Kind        string `json:"kind"`
	Name        string `json:"name"`
	CreatedAt   int64  `json:"createdAt"`
	EntityCount int64  `json:"entityCount"`
	BytesUsed   int64  `json:"bytesUsed"`
	QuotaBytes  *int64 `json:"quotaBytes"`
	Role        string `json:"role"`
}

func (d *Deps) getSpace(w http.ResponseWriter, r *http.Request, a *Access) error {
	var count, used int64
	err := d.Store.Read(r.Context(), func(tx store.ReadTx) error {
		var err error
		if count, err = tx.LiveEntityCount(a.id()); err != nil {
			return err
		}
		used, err = tx.WorkspaceUsage(a.id())
		return err
	})
	if err != nil {
		return err
	}
	ws := a.Space.Workspace
	return httpx.WriteJSON(w, http.StatusOK, SpaceJSON{SpaceID: ws.ID, Kind: kind(a.Space.Personal), Name: ws.Name,
		CreatedAt: ws.CreatedAt, EntityCount: count, BytesUsed: used, QuotaBytes: d.quota(a.Space.Personal), Role: *a.Space.Role}, nil)
}

func (d *Deps) pull(w http.ResponseWriter, r *http.Request, a *Access) error {
	since, limit, err := syncsvc.ParsePullParams(httpx.QueryGet(r, "since"), httpx.QueryGet(r, "limit"))
	if err != nil {
		return err
	}
	page, err := syncsvc.Pull(r.Context(), d.Store, a.id(), since, limit, d.Epoch(), true)
	if err != nil {
		return err
	}
	return httpx.WriteJSON(w, http.StatusOK, page, nil)
}

// push is the legacy push plus the account-space rules: an unknown type is a per-change
// unsupported_type, an updatedAt more than 5 minutes ahead of the server is a per-change
// clock_skew, applied changes record the author, and the response carries serverTime.
// (M2 adds role enforcement here; a personal space has only its owner.)
func (d *Deps) push(w http.ResponseWriter, r *http.Request, a *Access) error {
	body, err := httpx.ReadJSONObject(w, r, httpx.MaxBodyBytes)
	if err != nil {
		return err
	}
	now := clock.Millis(d.Clock.Now())
	changes, err := syncsvc.ParsePush(body, syncsvc.ParseOptions{UnknownTypePerChange: true, MaxUpdatedAt: now + syncsvc.MaxClockAheadMillis})
	if err != nil {
		return err
	}
	resp, err := syncsvc.Push(r.Context(), d.Store, a.id(), changes, d.limits(a.Space),
		&syncsvc.Actor{UserID: a.Principal.User.ID, Now: now})
	if err != nil {
		return err
	}
	return httpx.WriteJSON(w, http.StatusOK, resp, nil)
}

// ---------------------------------------------------------------- shares

func (d *Deps) scope(a *Access) shares.Scope {
	return shares.Scope{WorkspaceID: a.id(), Limits: d.limits(a.Space)}
}

func (d *Deps) listShares(w http.ResponseWriter, r *http.Request, a *Access) error {
	list, err := d.Shares.ListSpace(r.Context(), a.id())
	if err != nil {
		return err
	}
	return httpx.WriteJSON(w, http.StatusOK, struct {
		Shares []shares.SpaceListItem `json:"shares"`
	}{list}, nil)
}

func (d *Deps) createShare(w http.ResponseWriter, r *http.Request, a *Access) error {
	body, err := httpx.ReadJSONObject(w, r, httpx.MaxBodyBytes)
	if err != nil {
		return err
	}
	// Validate everything first (kind, entityId, snapshot, options), so a malformed request never
	// uses up the space's share-create budget.
	prepared, err := d.Shares.Prepare(body)
	if err != nil {
		return err
	}
	if prepared.Options, err = shares.ParseOptions(body); err != nil {
		return err
	}
	uid := a.Principal.User.ID
	prepared.CreatedBy = &uid
	if d.ShareLimiter != nil {
		if ok, retry := d.ShareLimiter.Allow("share_create:" + a.id()); !ok {
			return httpx.RateLimited("Too many shares created in this space; try again later", int64((retry+999_999_999)/1_000_000_000))
		}
	}
	created, err := d.Shares.Insert(r.Context(), d.scope(a), prepared)
	if err != nil {
		return err
	}
	name := a.Principal.User.Name
	return writeShare(w, http.StatusCreated, created, &shares.Author{UserID: uid, Name: &name}, prepared.Options)
}

// writeShare writes the create and update response of a space share: the legacy shape plus
// createdBy and options.
func writeShare(w http.ResponseWriter, status int, c *shares.Created, by *shares.Author, options []byte) error {
	return httpx.WriteJSON(w, status, struct {
		shares.Created
		CreatedBy *shares.Author `json:"createdBy"`
		Options   any            `json:"options"`
	}{*c, by, shares.RawOrNull(options)}, nil)
}

// updateShare checks, in order: the share id and that it belongs to this space (404), revoked
// (410), and only then the body; `options` absent or null keeps the stored options.
func (d *Deps) updateShare(w http.ResponseWriter, r *http.Request, a *Access) error {
	sh, err := d.Shares.CheckUpdatable(r.Context(), a.id(), r.PathValue("sid"))
	if err != nil {
		return err
	}
	body, err := httpx.ReadJSONObject(w, r, httpx.MaxBodyBytes)
	if err != nil {
		return err
	}
	options, err := shares.ParseOptions(body)
	if err != nil {
		return err
	}
	updated, err := d.Shares.Update(r.Context(), d.scope(a), sh, body, options)
	if err != nil {
		return err
	}
	if options == nil {
		options = sh.Options
	}
	by, err := d.Shares.AuthorOf(r.Context(), sh.CreatedBy)
	if err != nil {
		return err
	}
	return writeShare(w, http.StatusOK, updated, by, options)
}

// revokeShare never reads a body.
func (d *Deps) revokeShare(w http.ResponseWriter, r *http.Request, a *Access) error {
	revoked, err := d.Shares.Revoke(r.Context(), a.id(), r.PathValue("sid"))
	if err != nil {
		return err
	}
	return httpx.WriteJSON(w, http.StatusOK, revoked, nil)
}
