// Package legacy serves the Worker-compatible token routes: /v1/workspaces, /v1/workspace,
// /v1/workspace/token/rotate, /v1/sync and /v1/shares* (06-go-server.md §1.2).
package legacy

import (
	"errors"
	"net/http"
	"regexp"

	"github.com/mihajlopopara/reqharbor/server/internal/clientip"
	"github.com/mihajlopopara/reqharbor/server/internal/clock"
	"github.com/mihajlopopara/reqharbor/server/internal/config"
	"github.com/mihajlopopara/reqharbor/server/internal/httpx"
	"github.com/mihajlopopara/reqharbor/server/internal/ids"
	"github.com/mihajlopopara/reqharbor/server/internal/jobs"
	"github.com/mihajlopopara/reqharbor/server/internal/jsx"
	"github.com/mihajlopopara/reqharbor/server/internal/ratelimit"
	"github.com/mihajlopopara/reqharbor/server/internal/shares"
	"github.com/mihajlopopara/reqharbor/server/internal/store"
	"github.com/mihajlopopara/reqharbor/server/internal/syncsvc"
)

// Deps are the collaborators of the legacy routes.
type Deps struct {
	Store         store.Store
	Clock         clock.Clock
	Config        *config.Config
	Breaker       *jobs.Breaker
	ClientIP      *clientip.Resolver
	CreateLimiter *ratelimit.Limiter // workspace creates per IP bucket
	ShareLimiter  *ratelimit.Limiter // share creates per workspace
	Shares        *shares.Service
	// Epoch returns the current sync epoch for pull responses.
	Epoch func() string
}

var (
	bearerRe    = regexp.MustCompile(`(?i)^Bearer\s+(\S+)\s*$`)
	sharePathRe = regexp.MustCompile(`^[A-Za-z0-9]{1,64}$`)
)

func unauthorized(message string) *httpx.APIError {
	return httpx.NewError(401, "unauthorized", message).WithHeader("WWW-Authenticate", `Bearer realm="reqharbor"`)
}

// ServerFull is 507 server_full for a growing write the breaker refuses.
func ServerFull(message string) *httpx.APIError { return httpx.NewError(507, "server_full", message) }

// Register adds the legacy routes to mux.
func Register(mux *http.ServeMux, d *Deps) {
	mux.Handle("/v1/workspaces", httpx.Methods{{Name: "POST", Handler: d.gate(true, d.createWorkspace)}})
	mux.Handle("/v1/workspace", httpx.Methods{{Name: "GET", Handler: d.authed(d.getWorkspace)}})
	mux.Handle("/v1/workspace/token/rotate", httpx.Methods{{Name: "POST", Handler: d.authed(d.rotate)}})
	mux.Handle("/v1/sync", httpx.Methods{
		{Name: "GET", Handler: d.authed(d.pull)},
		{Name: "POST", Handler: d.authed(d.push)},
	})
	mux.Handle("/v1/shares", httpx.Methods{
		{Name: "GET", Handler: d.authed(d.listShares)},
		{Name: "POST", Handler: d.authed(d.createShare)},
	})
	shareByID := httpx.Methods{
		{Name: "PUT", Handler: d.authed(d.updateShare)},
		{Name: "DELETE", Handler: d.authed(d.revokeShare)},
	}
	mux.Handle("/v1/shares/{id}", http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if !sharePathRe.MatchString(r.PathValue("id")) {
			httpx.WriteError(w, httpx.RouteNotFound(r)) // a route 404, before the method check
			return
		}
		shareByID.ServeHTTP(w, r)
	}))
}

// gate applies REQHARBOR_LEGACY_WORKSPACES before auth and before the body.
func (d *Deps) gate(create bool, h httpx.Func) httpx.Func {
	return func(w http.ResponseWriter, r *http.Request) error {
		switch mode := d.Config.LegacyWorkspaces; {
		case mode == config.LegacyOff:
			return httpx.NewError(403, "legacy_disabled", "Token sync is disabled on this server")
		case create && mode != config.LegacyOpen:
			return httpx.NewError(403, "legacy_disabled", "This server does not accept new token-sync workspaces")
		}
		return h(w, r)
	}
}

type authedFunc func(w http.ResponseWriter, r *http.Request, ws *store.Workspace) error

func (d *Deps) authed(h authedFunc) httpx.Func {
	return d.gate(false, func(w http.ResponseWriter, r *http.Request) error {
		ws, err := d.authenticate(r)
		if err != nil {
			return err
		}
		return h(w, r, ws)
	})
}

// authenticate resolves `Authorization: Bearer rh_…` to its workspace or returns 401.
func (d *Deps) authenticate(r *http.Request) (*store.Workspace, error) {
	m := bearerRe.FindStringSubmatch(r.Header.Get("Authorization"))
	if m == nil || !ids.LegacyTokenRe.MatchString(m[1]) {
		return nil, unauthorized("Missing or invalid workspace token")
	}
	hash := ids.SHA256Hex(m[1])
	var ws *store.Workspace
	err := d.Store.Read(r.Context(), func(tx store.ReadTx) error {
		var err error
		ws, err = tx.WorkspaceByTokenHash(hash)
		return err
	})
	if errors.Is(err, store.ErrNotFound) {
		return nil, unauthorized("Missing or invalid workspace token")
	}
	if err != nil {
		return nil, err
	}
	if !ids.Equal(ws.TokenHash, hash) {
		return nil, unauthorized("Missing or invalid workspace token")
	}
	return ws, nil
}

// growth is the breaker check for growing legacy writes.
func (d *Deps) growth() error {
	if d.Breaker == nil {
		return nil
	}
	if d.Breaker.Open() {
		return ServerFull("This server is out of storage space; try again later")
	}
	if d.Breaker.LegacyFull() {
		return ServerFull("This server has reached its storage limit for token-sync workspaces")
	}
	return nil
}

func (d *Deps) limits() syncsvc.Limits {
	return syncsvc.Limits{QuotaBytes: d.Config.QuotaLegacyBytes, Growth: d.growth}
}

func (d *Deps) createWorkspace(w http.ResponseWriter, r *http.Request) error {
	body, err := httpx.ReadJSONObject(w, r, httpx.MaxBodyBytes)
	if err != nil {
		return err
	}
	raw, ok := body.Get("name").Str()
	name := jsx.Trim(raw)
	if !ok || jsx.UTF16Len(name) == 0 || jsx.UTF16Len(name) > 200 {
		return httpx.BadRequest("`name` must be a non-empty string of at most 200 characters")
	}
	bucket := clientip.Bucket(d.ClientIP.ClientIP(r))
	if ok, retry := d.CreateLimiter.Allow("ws_create:" + bucket); !ok {
		return httpx.RateLimited("Too many workspaces created from this IP; try again later", int64((retry+999_999_999)/1_000_000_000))
	}
	if err := d.growth(); err != nil {
		return err
	}
	now := d.Clock.Now()
	id := ids.ULID(now)
	token := ids.Token("rh_")
	err = d.Store.Write(r.Context(), func(tx store.WriteTx) error {
		return tx.InsertWorkspace(store.Workspace{ID: id, Name: name, TokenHash: ids.SHA256Hex(token), CreatedAt: clock.Millis(now)})
	})
	if err != nil {
		return err
	}
	// The token is returned exactly once; only its hash is stored.
	return httpx.WriteJSON(w, http.StatusCreated, struct {
		WorkspaceID string `json:"workspaceId"`
		Token       string `json:"token"`
	}{id, token}, nil)
}

func (d *Deps) getWorkspace(w http.ResponseWriter, r *http.Request, ws *store.Workspace) error {
	var count, used int64
	err := d.Store.Read(r.Context(), func(tx store.ReadTx) error {
		var err error
		if count, err = tx.LiveEntityCount(ws.ID); err != nil {
			return err
		}
		used, err = tx.WorkspaceUsage(ws.ID)
		return err
	})
	if err != nil {
		return err
	}
	return httpx.WriteJSON(w, http.StatusOK, struct {
		WorkspaceID string `json:"workspaceId"`
		Name        string `json:"name"`
		CreatedAt   int64  `json:"createdAt"`
		EntityCount int64  `json:"entityCount"`
		BytesUsed   int64  `json:"bytesUsed"`
		QuotaBytes  int64  `json:"quotaBytes"`
	}{ws.ID, ws.Name, ws.CreatedAt, count, used, d.Config.QuotaLegacyBytes}, nil)
}

// rotate is the compare-and-swap rotation (contract C4).
func (d *Deps) rotate(w http.ResponseWriter, r *http.Request, ws *store.Workspace) error {
	token := ids.Token("rh_")
	var swapped bool
	err := d.Store.Write(r.Context(), func(tx store.WriteTx) error {
		var err error
		swapped, err = tx.RotateToken(ws.ID, ws.TokenHash, ids.SHA256Hex(token))
		return err
	})
	if err != nil {
		return err
	}
	if !swapped {
		return unauthorized("Token was already rotated")
	}
	return httpx.WriteJSON(w, http.StatusOK, struct {
		Token string `json:"token"`
	}{token}, nil)
}

func (d *Deps) pull(w http.ResponseWriter, r *http.Request, ws *store.Workspace) error {
	since, limit, err := syncsvc.ParsePullParams(httpx.QueryGet(r, "since"), httpx.QueryGet(r, "limit"))
	if err != nil {
		return err
	}
	page, err := syncsvc.Pull(r.Context(), d.Store, ws.ID, since, limit, d.Epoch(), false)
	if err != nil {
		return err
	}
	return httpx.WriteJSON(w, http.StatusOK, page, nil)
}

func (d *Deps) push(w http.ResponseWriter, r *http.Request, ws *store.Workspace) error {
	body, err := httpx.ReadJSONObject(w, r, httpx.MaxBodyBytes)
	if err != nil {
		return err
	}
	changes, err := syncsvc.ParsePush(body, syncsvc.ParseOptions{})
	if err != nil {
		return err
	}
	resp, err := syncsvc.Push(r.Context(), d.Store, ws.ID, changes, d.limits(), nil)
	if err != nil {
		return err
	}
	return httpx.WriteJSON(w, http.StatusOK, resp, nil)
}

func (d *Deps) scope(ws *store.Workspace) shares.Scope {
	return shares.Scope{WorkspaceID: ws.ID, Limits: d.limits()}
}

func (d *Deps) createShare(w http.ResponseWriter, r *http.Request, ws *store.Workspace) error {
	body, err := httpx.ReadJSONObject(w, r, httpx.MaxBodyBytes)
	if err != nil {
		return err
	}
	// Validate first (kind, entityId, snapshot), so a malformed request never uses up the
	// workspace's share-create budget; only then charge the limiter.
	prepared, err := d.Shares.Prepare(body)
	if err != nil {
		return err
	}
	if d.ShareLimiter != nil {
		if ok, retry := d.ShareLimiter.Allow("share_create:" + ws.ID); !ok {
			return httpx.RateLimited("Too many shares created in this workspace; try again later", int64((retry+999_999_999)/1_000_000_000))
		}
	}
	created, err := d.Shares.Insert(r.Context(), d.scope(ws), prepared)
	if err != nil {
		return err
	}
	return httpx.WriteJSON(w, http.StatusCreated, created, nil)
}

func (d *Deps) listShares(w http.ResponseWriter, r *http.Request, ws *store.Workspace) error {
	list, err := d.Shares.List(r.Context(), ws.ID)
	if err != nil {
		return err
	}
	return httpx.WriteJSON(w, http.StatusOK, struct {
		Shares []shares.ListItem `json:"shares"`
	}{list}, nil)
}

func (d *Deps) updateShare(w http.ResponseWriter, r *http.Request, ws *store.Workspace) error {
	// Order (§1.2): id and ownership (404), revoked (410), only then the body.
	sh, err := d.Shares.CheckUpdatable(r.Context(), ws.ID, r.PathValue("id"))
	if err != nil {
		return err
	}
	body, err := httpx.ReadJSONObject(w, r, httpx.MaxBodyBytes)
	if err != nil {
		return err
	}
	updated, err := d.Shares.Update(r.Context(), d.scope(ws), sh, body, nil)
	if err != nil {
		return err
	}
	return httpx.WriteJSON(w, http.StatusOK, updated, nil)
}

func (d *Deps) revokeShare(w http.ResponseWriter, r *http.Request, ws *store.Workspace) error {
	revoked, err := d.Shares.Revoke(r.Context(), d.scope(ws), r.PathValue("id"))
	if err != nil {
		return err
	}
	return httpx.WriteJSON(w, http.StatusOK, revoked, nil)
}
