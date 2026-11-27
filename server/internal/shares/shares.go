// Package shares implements share links independent of how the caller authenticated: create,
// list, update and revoke for a workspace, and the public /s/{id} views (port of
// worker/src/shares.ts and snapshot.ts prepareSnapshot).
package shares

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"regexp"

	"github.com/mihajlopopara/reqharbor/server/internal/clock"
	"github.com/mihajlopopara/reqharbor/server/internal/httpx"
	"github.com/mihajlopopara/reqharbor/server/internal/ids"
	"github.com/mihajlopopara/reqharbor/server/internal/jsonx"
	"github.com/mihajlopopara/reqharbor/server/internal/jsx"
	"github.com/mihajlopopara/reqharbor/server/internal/store"
	"github.com/mihajlopopara/reqharbor/server/internal/syncsvc"
)

// SnapshotBytes is the 1 MiB snapshot limit (PRODUCT §8).
const SnapshotBytes = 1024 * 1024

// Kinds are the shareable root entities.
var Kinds = []string{"collection", "request"}

var entityIDRe = regexp.MustCompile(`^[A-Za-z0-9_.:-]{1,128}$`)

// Service holds what share operations need.
type Service struct {
	Store     store.Store
	Clock     clock.Clock
	PublicURL string
}

// URL is the public share URL.
func (s *Service) URL(id string) string { return s.PublicURL + "/s/" + id }

// Scope is the storage scope a share is written in.
type Scope struct {
	WorkspaceID string
	Limits      syncsvc.Limits
}

func isKind(k string) bool { return k == "collection" || k == "request" }

// PrepareSnapshot validates a client snapshot for kind, removes secret variables from an
// attached environment and returns the canonical JSON to store.
func PrepareSnapshot(kind string, raw *jsonx.Value) ([]byte, error) {
	if !raw.IsObject() {
		return nil, httpx.BadRequest("`snapshot` must be an object")
	}
	v := raw.Get("schemaVersion")
	n, safe := int64(0), false
	if v != nil && v.Kind == jsonx.Number {
		n, safe = jsx.SafeInteger(v.S)
	}
	if !safe || n < 1 {
		return nil, httpx.BadRequest("`snapshot.schemaVersion` must be a positive integer")
	}
	root := raw.Get(kind)
	if !root.IsObject() {
		return nil, httpx.BadRequest(fmt.Sprintf("`snapshot.%s` must be an object for a %s share", kind, kind))
	}
	if kind == "collection" {
		for _, k := range []string{"folders", "requests"} {
			if f := root.Get(k); f != nil && !f.IsArray() {
				return nil, httpx.BadRequest(fmt.Sprintf("`snapshot.collection.%s` must be an array", k))
			}
		}
	}
	env := raw.Get("environment")
	if env != nil && !env.IsNull() && !env.IsObject() {
		return nil, httpx.BadRequest("`snapshot.environment` must be an object or null")
	}
	clean := raw
	if env.IsObject() {
		clean = raw.ShallowCopy()
		clean.Set("environment", RemoveSecretVariables(env))
	}
	text, err := jsonx.Canonical(clean)
	if err != nil {
		if errors.Is(err, jsonx.ErrNumberRange) || errors.Is(err, jsonx.ErrLoneSurrogate) || errors.Is(err, jsonx.ErrTooDeep) {
			return nil, httpx.BadRequest("Snapshot contains a value ReqHarbor cannot read")
		}
		return nil, err
	}
	if len(text) > SnapshotBytes {
		return nil, httpx.TooLarge("snapshot_too_large", fmt.Sprintf("Snapshot exceeds %d bytes", SnapshotBytes))
	}
	return text, nil
}

// RemoveSecretVariables drops every variable with secret === true (AC-SHARE-2).
func RemoveSecretVariables(env *jsonx.Value) *jsonx.Value {
	vars := env.Get("variables")
	if !vars.IsArray() {
		return env
	}
	out := env.ShallowCopy()
	kept := &jsonx.Value{Kind: jsonx.Array, Arr: []*jsonx.Value{}}
	for _, v := range vars.Arr {
		if !(v.IsObject() && v.Get("secret").IsTrue()) {
			kept.Arr = append(kept.Arr, v)
		}
	}
	out.Set("variables", kept)
	return out
}

// Created is the create and update response.
type Created struct {
	ShareID   string `json:"shareId"`
	URL       string `json:"url"`
	CreatedAt int64  `json:"createdAt"`
	UpdatedAt int64  `json:"updatedAt"`
}

// Prepared is a validated share create body. CreatedBy and Options are set for account spaces
// only (ParseOptions); legacy shares have neither.
type Prepared struct {
	Kind, EntityID string
	Snapshot       []byte
	CreatedBy      *string
	Options        []byte
}

// ParseOptions validates the `options` member of an account-space share create or update body
// (AC-TSHARE-3, 0004 shares.options) and returns its canonical JSON
// {"includeCredentials":bool,"environmentId":string|null}, or nil when the member is absent or
// null (a create then stores no options and an update keeps the stored ones). A missing
// includeCredentials is false; unknown keys are dropped.
func ParseOptions(body *jsonx.Value) ([]byte, error) {
	raw := body.Get("options")
	if raw == nil || raw.IsNull() {
		return nil, nil
	}
	if !raw.IsObject() {
		return nil, httpx.BadRequest("`options` must be an object or null")
	}
	include := false
	if v := raw.Get("includeCredentials"); v != nil && !v.IsNull() {
		if v.Kind != jsonx.Bool {
			return nil, httpx.BadRequest("`options.includeCredentials` must be a boolean")
		}
		include = v.B
	}
	var envID *string
	if v := raw.Get("environmentId"); v != nil && !v.IsNull() {
		id, ok := v.Str()
		if !ok || !entityIDRe.MatchString(id) {
			return nil, httpx.BadRequest("`options.environmentId` must be a 1-128 character id or null")
		}
		envID = &id
	}
	return httpx.Marshal(ShareOptions{IncludeCredentials: include, EnvironmentID: envID})
}

// ShareOptions is the stored shape of shares.options.
type ShareOptions struct {
	IncludeCredentials bool    `json:"includeCredentials"`
	EnvironmentID      *string `json:"environmentId"`
}

// Prepare validates a create body in the Worker's order: kind, entityId, then the snapshot.
func (s *Service) Prepare(body *jsonx.Value) (*Prepared, error) {
	kind, ok := body.Get("kind").Str()
	if !ok || !isKind(kind) {
		return nil, httpx.BadRequest("`kind` must be \"collection\" or \"request\"")
	}
	entityID, ok := body.Get("entityId").Str()
	if !ok || !entityIDRe.MatchString(entityID) {
		return nil, httpx.BadRequest("`entityId` must be a 1-128 character id")
	}
	snap, err := PrepareSnapshot(kind, body.Get("snapshot"))
	if err != nil {
		return nil, err
	}
	return &Prepared{Kind: kind, EntityID: entityID, Snapshot: snap}, nil
}

// Insert stores a prepared share after the quota and breaker checks.
func (s *Service) Insert(ctx context.Context, sc Scope, p *Prepared) (*Created, error) {
	size := int64(len(p.Snapshot))
	id := ids.ShareID()
	now := clock.Millis(s.Clock.Now())
	err := s.Store.Write(ctx, func(tx store.WriteTx) error {
		if err := checkGrowth(tx, sc, size); err != nil {
			return err
		}
		return tx.InsertShare(store.Share{
			ID: id, WorkspaceID: sc.WorkspaceID, Kind: p.Kind, EntityID: p.EntityID,
			Snapshot: p.Snapshot, Size: size, CreatedAt: now, UpdatedAt: now,
			CreatedBy: p.CreatedBy, Options: p.Options,
		})
	})
	if err != nil {
		return nil, err
	}
	return &Created{ShareID: id, URL: s.URL(id), CreatedAt: now, UpdatedAt: now}, nil
}

// Create validates the body and stores a new share (order: kind, entityId, snapshot, quota,
// breaker, insert).
func (s *Service) Create(ctx context.Context, sc Scope, body *jsonx.Value) (*Created, error) {
	p, err := s.Prepare(body)
	if err != nil {
		return nil, err
	}
	return s.Insert(ctx, sc, p)
}

func checkGrowth(tx store.WriteTx, sc Scope, delta int64) error {
	if delta <= 0 {
		return nil
	}
	used, err := tx.WorkspaceUsage(sc.WorkspaceID)
	if err != nil {
		return err
	}
	if q := sc.Limits.QuotaBytes; q > 0 && used+delta > q {
		return syncsvc.QuotaExceeded(q, used, delta)
	}
	if sc.Limits.Growth != nil {
		return sc.Limits.Growth()
	}
	return nil
}

// ListItem is one entry of GET /v1/shares.
type ListItem struct {
	ShareID   string  `json:"shareId"`
	URL       string  `json:"url"`
	Kind      string  `json:"kind"`
	EntityID  string  `json:"entityId"`
	Name      *string `json:"name"`
	CreatedAt int64   `json:"createdAt"`
	UpdatedAt int64   `json:"updatedAt"`
}

// List returns the active shares of a workspace, newest first.
func (s *Service) List(ctx context.Context, wsID string) ([]ListItem, error) {
	out := []ListItem{}
	err := s.Store.Read(ctx, func(tx store.ReadTx) error {
		rows, err := tx.ActiveShares(wsID)
		if err != nil {
			return err
		}
		for _, r := range rows {
			out = append(out, ListItem{ShareID: r.ID, URL: s.URL(r.ID), Kind: r.Kind, EntityID: r.EntityID, Name: r.Name, CreatedAt: r.CreatedAt, UpdatedAt: r.UpdatedAt})
		}
		return nil
	})
	return out, err
}

// Author names who created a share in an account space. Name is the user's current name, or null
// when the account no longer exists (shown as "Former member").
type Author struct {
	UserID string  `json:"userId"`
	Name   *string `json:"name"`
}

// SpaceListItem is one entry of GET /v1/spaces/{id}/shares: the legacy shape plus createdBy and
// options.
type SpaceListItem struct {
	ListItem
	CreatedBy *Author         `json:"createdBy"`
	Options   json.RawMessage `json:"options"`
}

// RawOrNull returns stored JSON for embedding, or JSON null.
func RawOrNull(b []byte) json.RawMessage {
	if b == nil {
		return json.RawMessage("null")
	}
	return json.RawMessage(b)
}

// ListSpace returns the active shares of an account space, newest first, with their authors and
// options.
func (s *Service) ListSpace(ctx context.Context, wsID string) ([]SpaceListItem, error) {
	out := []SpaceListItem{}
	err := s.Store.Read(ctx, func(tx store.ReadTx) error {
		rows, err := tx.ActiveShares(wsID)
		if err != nil {
			return err
		}
		for _, r := range rows {
			it := SpaceListItem{
				ListItem: ListItem{ShareID: r.ID, URL: s.URL(r.ID), Kind: r.Kind, EntityID: r.EntityID, Name: r.Name, CreatedAt: r.CreatedAt, UpdatedAt: r.UpdatedAt},
				Options:  RawOrNull(r.Options),
			}
			if r.CreatedBy != nil {
				it.CreatedBy = &Author{UserID: *r.CreatedBy, Name: r.CreatedByName}
			}
			out = append(out, it)
		}
		return nil
	})
	return out, err
}

// AuthorOf loads the author of a share created by userID (nil for a legacy share).
func (s *Service) AuthorOf(ctx context.Context, userID *string) (*Author, error) {
	if userID == nil {
		return nil, nil
	}
	a := &Author{UserID: *userID}
	err := s.Store.Read(ctx, func(tx store.ReadTx) error {
		u, err := tx.UserByID(*userID)
		if errors.Is(err, store.ErrNotFound) {
			return nil
		}
		if err != nil {
			return err
		}
		a.Name = &u.Name
		return nil
	})
	return a, err
}

// Owned loads a share of this workspace. Malformed ids, unknown ids and other workspaces'
// shares are indistinguishable 404s.
func (s *Service) Owned(ctx context.Context, wsID, id string) (*store.Share, error) {
	if !ids.ShareIDRe.MatchString(id) {
		return nil, httpx.NotFound("Share not found")
	}
	var sh *store.Share
	err := s.Store.Read(ctx, func(tx store.ReadTx) error {
		var err error
		sh, err = tx.Share(id)
		return err
	})
	if errors.Is(err, store.ErrNotFound) || (err == nil && sh.WorkspaceID != wsID) {
		return nil, httpx.NotFound("Share not found")
	}
	return sh, err
}

// CheckUpdatable runs the checks an update makes before reading its body: ownership (404),
// then revoked (410).
func (s *Service) CheckUpdatable(ctx context.Context, wsID, id string) (*store.Share, error) {
	sh, err := s.Owned(ctx, wsID, id)
	if err != nil {
		return nil, err
	}
	if sh.RevokedAt != nil {
		return nil, httpx.Gone()
	}
	return sh, nil
}

// Update replaces the snapshot behind the same URL. sh comes from CheckUpdatable. options, when
// not nil, replaces the stored options (account spaces); nil keeps them.
func (s *Service) Update(ctx context.Context, sc Scope, sh *store.Share, body *jsonx.Value, options []byte) (*Created, error) {
	snap, err := PrepareSnapshot(sh.Kind, body.Get("snapshot"))
	if err != nil {
		return nil, err
	}
	size := int64(len(snap))
	now := clock.Millis(s.Clock.Now())
	if sh.UpdatedAt > now {
		now = sh.UpdatedAt
	}
	err = s.Store.Write(ctx, func(tx store.WriteTx) error {
		cur, err := tx.Share(sh.ID)
		if err != nil {
			return err
		}
		if cur.RevokedAt != nil {
			return httpx.Gone()
		}
		if err := checkGrowth(tx, sc, size-cur.Size); err != nil {
			return err
		}
		if cur.UpdatedAt > now {
			now = cur.UpdatedAt
		}
		ok, err := tx.UpdateShareSnapshot(sh.ID, sc.WorkspaceID, snap, options, size, now)
		if err != nil {
			return err
		}
		if !ok {
			return httpx.Gone() // revoked concurrently
		}
		return nil
	})
	if err != nil {
		return nil, err
	}
	return &Created{ShareID: sh.ID, URL: s.URL(sh.ID), CreatedAt: sh.CreatedAt, UpdatedAt: now}, nil
}

// Revoked is the revoke response.
type Revoked struct {
	ShareID   string `json:"shareId"`
	RevokedAt int64  `json:"revokedAt"`
}

// Revoke revokes a share (idempotent) and discards its snapshot. It never reads a body.
func (s *Service) Revoke(ctx context.Context, wsID, id string) (*Revoked, error) {
	sh, err := s.Owned(ctx, wsID, id)
	if err != nil {
		return nil, err
	}
	if sh.RevokedAt != nil {
		return &Revoked{ShareID: id, RevokedAt: *sh.RevokedAt}, nil
	}
	revokedAt := clock.Millis(s.Clock.Now())
	err = s.Store.Write(ctx, func(tx store.WriteTx) error {
		cur, err := tx.Share(id)
		if err != nil {
			return err
		}
		if cur.RevokedAt != nil {
			revokedAt = *cur.RevokedAt
			return nil
		}
		return tx.RevokeShare(id, revokedAt)
	})
	if err != nil {
		return nil, err
	}
	return &Revoked{ShareID: id, RevokedAt: revokedAt}, nil
}
