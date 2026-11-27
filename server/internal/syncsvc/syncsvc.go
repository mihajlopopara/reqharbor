// Package syncsvc validates pushes and applies them with last-write-wins inside one serialised
// write transaction; it also serves pull pages (06-go-server.md §1.2 "Sync"). It is shared by
// the legacy token routes and, from S6, by space sync.
package syncsvc

import (
	"context"
	"errors"
	"fmt"
	"regexp"
	"strconv"
	"strings"

	"github.com/mihajlopopara/reqharbor/server/internal/httpx"
	"github.com/mihajlopopara/reqharbor/server/internal/jsonx"
	"github.com/mihajlopopara/reqharbor/server/internal/jsx"
	"github.com/mihajlopopara/reqharbor/server/internal/store"
)

// Protocol constants the client hard-codes (PRODUCT §8).
const (
	EntityDataBytes  = 512 * 1024
	ChangesPerPush   = 500
	PullLimitDefault = 500
	PullLimitMax     = 500
)

// Types is the allowlist of synced entity types (AC-SYNC-3: history and settings never sync).
var Types = []string{"project", "collection", "folder", "request", "environment"}

// Rejection reasons reported per change.
const (
	ReasonEntityTooLarge   = "entity_too_large"
	ReasonInvalidJSONValue = "invalid_json_value"
	ReasonUnsupportedType  = "unsupported_type"
	ReasonClockSkew        = "clock_skew"
)

// MaxClockAheadMillis is how far a change's updatedAt may be ahead of the server's clock in an
// account space before it is rejected with clock_skew (AC-TSYNC-5, 04 §6.2): 5 minutes.
const MaxClockAheadMillis = 5 * 60 * 1000

var (
	idRe       = regexp.MustCompile(`^[A-Za-z0-9_.:-]{1,128}$`)
	deviceIDRe = regexp.MustCompile(`^[A-Za-z0-9_.:-]{0,128}$`)
)

func knownType(t string) bool {
	for _, k := range Types {
		if k == t {
			return true
		}
	}
	return false
}

// Change is one validated pushed change.
type Change struct {
	Type      string
	ID        string
	UpdatedAt int64
	Deleted   bool
	DeviceID  string
	Data      []byte // canonical JSON; nil for tombstones and rejected changes
	Size      int64
	// Rejected is a per-change rejection reason; such changes take no rev and are not stored.
	Rejected string
}

// ParseOptions vary validation between legacy and space sync.
type ParseOptions struct {
	// UnknownTypePerChange turns an unknown type into a per-change unsupported_type rejection
	// (space sync) instead of a whole-request 400 (legacy sync, byte-compatible).
	UnknownTypePerChange bool
	// MaxUpdatedAt, when positive, rejects a change whose updatedAt is greater with a per-change
	// clock_skew (account spaces: server time + MaxClockAheadMillis).
	MaxUpdatedAt int64
}

// ParsePush validates a push body in the Worker's order. The first invalid change ends the
// request with 400; oversized or unreadable data is a per-change rejection.
func ParsePush(body *jsonx.Value, opts ParseOptions) ([]Change, error) {
	raw := body.Get("changes")
	if !raw.IsArray() {
		return nil, httpx.BadRequest("`changes` must be an array")
	}
	if len(raw.Arr) > ChangesPerPush {
		return nil, httpx.TooLarge("too_many_changes", fmt.Sprintf("At most %d changes per push", ChangesPerPush))
	}
	out := make([]Change, 0, len(raw.Arr))
	for i, item := range raw.Arr {
		c, err := validateChange(item, i, opts)
		if err != nil {
			return nil, err
		}
		out = append(out, c)
	}
	seen := make(map[string]struct{}, len(out))
	for _, c := range out {
		k := c.Type + "\x00" + c.ID
		if _, dup := seen[k]; dup {
			return nil, httpx.BadRequestCode("duplicate_change", fmt.Sprintf("Duplicate change for %s %s in one push", c.Type, c.ID))
		}
		seen[k] = struct{}{}
	}
	return out, nil
}

func validateChange(raw *jsonx.Value, i int, opts ParseOptions) (Change, error) {
	at := "changes[" + strconv.Itoa(i) + "]"
	if !raw.IsObject() {
		return Change{}, httpx.BadRequest(at + " must be an object")
	}
	var c Change
	typ, isStr := raw.Get("type").Str()
	if !isStr || !knownType(typ) {
		if !opts.UnknownTypePerChange || !isStr {
			return Change{}, httpx.BadRequestCode("unsupported_type",
				at+".type must be one of "+strings.Join(Types, ", ")+" (history and settings are never synced)")
		}
		c.Rejected = ReasonUnsupportedType
	}
	c.Type = typ
	id, ok := raw.Get("id").Str()
	if !ok || !idRe.MatchString(id) {
		return Change{}, httpx.BadRequest(at + ".id must be a 1-128 character id")
	}
	c.ID = id
	ua := raw.Get("updatedAt")
	if ua == nil || ua.Kind != jsonx.Number {
		return Change{}, httpx.BadRequest(at + ".updatedAt must be a non-negative integer (epoch ms)")
	}
	n, safe := jsx.SafeInteger(ua.S)
	if !safe || n < 0 {
		return Change{}, httpx.BadRequest(at + ".updatedAt must be a non-negative integer (epoch ms)")
	}
	c.UpdatedAt = n
	del := raw.Get("deleted")
	if del == nil || del.Kind != jsonx.Bool {
		return Change{}, httpx.BadRequest(at + ".deleted must be a boolean")
	}
	c.Deleted = del.B
	if dev := raw.Get("deviceId"); dev != nil && !dev.IsNull() {
		s, ok := dev.Str()
		if !ok || !deviceIDRe.MatchString(s) {
			return Change{}, httpx.BadRequest(at + ".deviceId must be a string of at most 128 id characters")
		}
		c.DeviceID = s
	}
	data := raw.Get("data")
	if c.Deleted {
		if data != nil && !data.IsNull() {
			return Change{}, httpx.BadRequest(at + ".data must be null for a tombstone")
		}
	} else if !data.IsObject() {
		return Change{}, httpx.BadRequest(at + ".data must be an object unless deleted is true")
	}
	// Every 400 check above applies to every change; only then the per-change rejections, in
	// this order: unsupported_type, clock_skew, then the data checks (invalid_json_value,
	// entity_too_large).
	if c.Rejected != "" {
		return c, nil
	}
	if opts.MaxUpdatedAt > 0 && c.UpdatedAt > opts.MaxUpdatedAt {
		c.Rejected = ReasonClockSkew
		return c, nil
	}
	if c.Deleted {
		return c, nil
	}
	if c.Type == "environment" {
		data = StripSecretValuesForSync(data)
	}
	canon, err := jsonx.Canonical(data)
	if err != nil {
		if errors.Is(err, jsonx.ErrNumberRange) || errors.Is(err, jsonx.ErrLoneSurrogate) || errors.Is(err, jsonx.ErrTooDeep) {
			c.Rejected = ReasonInvalidJSONValue
			return c, nil
		}
		return Change{}, err
	}
	if len(canon) > EntityDataBytes {
		c.Rejected = ReasonEntityTooLarge
		return c, nil
	}
	c.Data = canon
	c.Size = int64(len(canon))
	return c, nil
}

// StripSecretValuesForSync blanks the value of every secret variable of an environment
// (AC-ENV-7): `{...v, value: ""}` for each variable with secret === true. The client keeps its
// local secret values on pull.
func StripSecretValuesForSync(env *jsonx.Value) *jsonx.Value {
	vars := env.Get("variables")
	if !vars.IsArray() {
		return env
	}
	out := env.ShallowCopy()
	nv := &jsonx.Value{Kind: jsonx.Array, Arr: make([]*jsonx.Value, len(vars.Arr))}
	for i, v := range vars.Arr {
		if v.IsObject() && v.Get("secret").IsTrue() {
			cp := v.ShallowCopy()
			cp.Set("value", jsonx.NewString(""))
			nv.Arr[i] = cp
		} else {
			nv.Arr[i] = v
		}
	}
	out.Set("variables", nv)
	return out
}

// Wins is last-write-wins, contract C3: the incoming change wins when its updatedAt is greater,
// or equal with a deviceId that is byte-wise greater than or equal to the stored one.
func Wins(inUpdatedAt int64, inDevice string, storedUpdatedAt int64, storedDevice string) bool {
	return inUpdatedAt > storedUpdatedAt || (inUpdatedAt == storedUpdatedAt && inDevice >= storedDevice)
}

// Result is one per-change push result.
type Result struct {
	Type   string `json:"type"`
	ID     string `json:"id"`
	Status string `json:"status"`
	Rev    int64  `json:"rev,omitempty"`
	Reason string `json:"reason,omitempty"`
}

// PushResponse is the push response body. ServerTime is set on account spaces only, so legacy
// responses stay byte-identical.
type PushResponse struct {
	Results    []Result `json:"results"`
	Cursor     int64    `json:"cursor"`
	ServerTime *int64   `json:"serverTime,omitempty"`
}

// Actor is the signed-in user pushing to an account space. Legacy pushes have none.
type Actor struct {
	UserID string
	// Now is the server time (Unix ms) reported as serverTime.
	Now int64
}

// Limits are the per-push storage rules.
type Limits struct {
	// QuotaBytes caps the space's usage; 0 means unlimited.
	QuotaBytes int64
	// Growth, when non-nil, is called for a growing push after the quota check; an error (507
	// server_full from the breaker) refuses the push.
	Growth func() error
}

// QuotaExceeded is the Worker's 413 quota_exceeded.
func QuotaExceeded(quota, used, delta int64) *httpx.APIError {
	return httpx.TooLarge("quota_exceeded",
		fmt.Sprintf("Workspace storage quota of %d bytes exceeded (%d used, this write adds %d)", quota, used, delta))
}

// Push applies validated changes to workspace wsID in one write transaction. With an actor (an
// account space) every applied change records actor.UserID as updated_by, and the response
// carries serverTime; a stale change keeps the stored author.
func Push(ctx context.Context, st store.Store, wsID string, changes []Change, lim Limits, actor *Actor) (*PushResponse, error) {
	resp := &PushResponse{Results: make([]Result, len(changes))}
	updatedBy := ""
	if actor != nil {
		updatedBy = actor.UserID
		now := actor.Now
		resp.ServerTime = &now
	}
	err := st.Write(ctx, func(tx store.WriteTx) error {
		if err := tx.LockWorkspace(wsID); err != nil {
			return err
		}
		ws, err := tx.Workspace(wsID) // re-read the head inside the transaction
		if err != nil {
			return err
		}
		head := ws.Rev
		metas := make([]*store.EntityMeta, len(changes))
		var delta int64
		n := 0
		for i, c := range changes {
			if c.Rejected != "" {
				continue
			}
			m, err := tx.EntityMeta(wsID, c.Type, c.ID)
			if err != nil && !errors.Is(err, store.ErrNotFound) {
				return err
			}
			metas[i] = m
			prev := int64(0)
			if m != nil {
				prev = m.Size
			}
			delta += c.Size - prev
			n++
		}
		if n > 0 && delta > 0 {
			used, err := tx.WorkspaceUsage(wsID)
			if err != nil {
				return err
			}
			if lim.QuotaBytes > 0 && used+delta > lim.QuotaBytes {
				return QuotaExceeded(lim.QuotaBytes, used, delta)
			}
			if lim.Growth != nil {
				if err := lim.Growth(); err != nil {
					return err
				}
			}
		}
		k := int64(0)
		for i, c := range changes {
			if c.Rejected != "" {
				resp.Results[i] = Result{Type: c.Type, ID: c.ID, Status: "rejected", Reason: c.Rejected}
				continue
			}
			k++
			rev := head + k
			m := metas[i]
			if m == nil || Wins(c.UpdatedAt, c.DeviceID, m.UpdatedAt, m.DeviceID) {
				if err := tx.UpsertEntity(wsID, store.Entity{
					Type: c.Type, ID: c.ID, Rev: rev, UpdatedAt: c.UpdatedAt, Deleted: c.Deleted,
					DeviceID: c.DeviceID, Data: c.Data, Size: c.Size, UpdatedBy: updatedBy,
				}); err != nil {
					return err
				}
				resp.Results[i] = Result{Type: c.Type, ID: c.ID, Status: "applied", Rev: rev}
			} else {
				// A losing change still moves the stored row to a fresh rev, so the winner is
				// re-delivered to a client whose cursor is already past its old rev.
				if err := tx.BumpEntityRev(wsID, c.Type, c.ID, rev); err != nil {
					return err
				}
				resp.Results[i] = Result{Type: c.Type, ID: c.ID, Status: "stale", Rev: rev}
			}
		}
		if k > 0 {
			if err := tx.SetWorkspaceRev(wsID, head+k); err != nil {
				return err
			}
		}
		resp.Cursor = head + k
		return nil
	})
	if err != nil {
		return nil, err
	}
	return resp, nil
}

// PulledChange is one change of a pull page; Data is the stored canonical JSON.
type PulledChange struct {
	Type      string    `json:"type"`
	ID        string    `json:"id"`
	UpdatedAt int64     `json:"updatedAt"`
	Deleted   bool      `json:"deleted"`
	DeviceID  string    `json:"deviceId"`
	Data      rawOrNull `json:"data"`
	Rev       int64     `json:"rev"`
	// UpdatedBy is set on account-space pulls when the version has an author; legacy pulls never
	// carry it.
	UpdatedBy *UpdatedBy `json:"updatedBy,omitempty"`
}

// UpdatedBy names the author of a pulled version. Name is the user's current name, or null when
// the account no longer exists.
type UpdatedBy struct {
	UserID string  `json:"userId"`
	Name   *string `json:"name"`
}

// rawOrNull embeds stored JSON verbatim, or null.
type rawOrNull []byte

// MarshalJSON implements json.Marshaler.
func (r rawOrNull) MarshalJSON() ([]byte, error) {
	if r == nil {
		return []byte("null"), nil
	}
	return r, nil
}

// PullPage is the pull response body.
type PullPage struct {
	Changes   []PulledChange `json:"changes"`
	Cursor    int64          `json:"cursor"`
	HasMore   bool           `json:"hasMore"`
	SyncEpoch string         `json:"syncEpoch"`
}

// ParsePullParams validates `since` and `limit` like the Worker (digits only, a safe integer,
// at least the minimum; limit is clamped to 500).
func ParsePullParams(since, limit string) (int64, int, error) {
	s, err := intParam(since, "since", 0, 0)
	if err != nil {
		return 0, 0, err
	}
	l, err := intParam(limit, "limit", PullLimitDefault, 1)
	if err != nil {
		return 0, 0, err
	}
	if l > PullLimitMax {
		l = PullLimitMax
	}
	return s, int(l), nil
}

var digitsRe = regexp.MustCompile(`^\d+$`)

func intParam(raw, name string, def, min int64) (int64, error) {
	if raw == "" {
		return def, nil
	}
	if !digitsRe.MatchString(raw) {
		return 0, httpx.BadRequest("`" + name + "` must be a non-negative integer")
	}
	n, err := strconv.ParseInt(raw, 10, 64)
	if err != nil || n > jsx.MaxSafeInteger || n < min {
		return 0, httpx.BadRequest("`" + name + "` must be an integer >= " + strconv.FormatInt(min, 10))
	}
	return n, nil
}

// Pull reads one page and the head in one read transaction. withActors adds updatedBy (account
// spaces).
func Pull(ctx context.Context, st store.Store, wsID string, since int64, limit int, epoch string, withActors bool) (*PullPage, error) {
	page := &PullPage{Changes: []PulledChange{}, SyncEpoch: epoch}
	err := st.Read(ctx, func(tx store.ReadTx) error {
		rows, err := tx.EntitiesSince(wsID, since, limit+1)
		if err != nil {
			return err
		}
		ws, err := tx.Workspace(wsID)
		if err != nil {
			return err
		}
		page.HasMore = len(rows) > limit
		if page.HasMore {
			rows = rows[:limit]
		}
		for _, e := range rows {
			var data rawOrNull
			if !e.Deleted && e.Data != nil {
				data = e.Data
			}
			pc := PulledChange{
				Type: e.Type, ID: e.ID, UpdatedAt: e.UpdatedAt, Deleted: e.Deleted, DeviceID: e.DeviceID, Data: data, Rev: e.Rev,
			}
			if withActors && e.UpdatedBy != "" {
				pc.UpdatedBy = &UpdatedBy{UserID: e.UpdatedBy, Name: e.UpdatedByName}
			}
			page.Changes = append(page.Changes, pc)
		}
		if len(rows) > 0 {
			page.Cursor = rows[len(rows)-1].Rev
		} else {
			page.Cursor = ws.Rev
		}
		return nil
	})
	if err != nil {
		return nil, err
	}
	return page, nil
}
