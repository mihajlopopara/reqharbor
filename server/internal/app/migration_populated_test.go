package app_test

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"testing"
	"time"

	"github.com/mihajlopopara/reqharbor/server/internal/db"
	"github.com/mihajlopopara/reqharbor/server/internal/jsonx"
	"github.com/mihajlopopara/reqharbor/server/internal/sharepage"
	tu "github.com/mihajlopopara/reqharbor/server/internal/testutil"
)

// The populated-DB migration test (06-go-server.md §8.6): a Worker-era database (schema v2,
// testdata/legacy_seed.sql) migrated through 0003 and 0004 keeps every row, and every legacy route
// answers with the JSON that database meant before the migrations (the Worker's wire contract,
// computed straight from the v2 rows), apart from the added syncEpoch on pulls (W10).

var seedTokens = map[string]string{
	"01JSEEDWS00000000000000001": "rh_SeedOne" + strings.Repeat("A", 36),
	"01JSEEDWS00000000000000002": "rh_SeedTwo" + strings.Repeat("A", 36),
	"01JSEEDWS00000000000000003": "rh_SeedSix" + strings.Repeat("A", 36),
}

type refEntity struct {
	Type, ID  string
	Rev       int64
	UpdatedAt int64
	Deleted   bool
	DeviceID  string
	Data      *string
	Size      int64
}

type refShare struct {
	ID, Kind, EntityID   string
	Snapshot             *string
	CreatedAt, UpdatedAt int64
	RevokedAt            *int64
	Size                 int64
}

type refWorkspace struct {
	ID, Name       string
	Rev, CreatedAt int64
	Entities       []refEntity // by rev
	Shares         []refShare
}

// legacyRows reads the v2 columns of the three legacy tables (their state before 0003 and 0004),
// as a reference model and as a text dump for the after-each-migration equality check.
func legacyRows(t *testing.T, d *db.DB) (map[string]*refWorkspace, string) {
	t.Helper()
	var dump strings.Builder
	out := map[string]*refWorkspace{}
	rows, err := d.Reader.Query("SELECT id, name, token_hash, rev, created_at FROM workspaces ORDER BY id")
	if err != nil {
		t.Fatal(err)
	}
	for rows.Next() {
		w := &refWorkspace{}
		var hash string
		if err := rows.Scan(&w.ID, &w.Name, &hash, &w.Rev, &w.CreatedAt); err != nil {
			t.Fatal(err)
		}
		out[w.ID] = w
		fmt.Fprintf(&dump, "W %s %q %s %d %d\n", w.ID, w.Name, hash, w.Rev, w.CreatedAt)
	}
	rows.Close()
	rows, err = d.Reader.Query("SELECT workspace_id, type, id, rev, updated_at, deleted, data, device_id, size FROM entities ORDER BY workspace_id, rev")
	if err != nil {
		t.Fatal(err)
	}
	for rows.Next() {
		var ws string
		var e refEntity
		if err := rows.Scan(&ws, &e.Type, &e.ID, &e.Rev, &e.UpdatedAt, &e.Deleted, &e.Data, &e.DeviceID, &e.Size); err != nil {
			t.Fatal(err)
		}
		out[ws].Entities = append(out[ws].Entities, e)
		data := "NULL"
		if e.Data != nil {
			data = *e.Data
		}
		fmt.Fprintf(&dump, "E %s %s %s %d %d %v %q %d %s\n", ws, e.Type, e.ID, e.Rev, e.UpdatedAt, e.Deleted, e.DeviceID, e.Size, data)
	}
	rows.Close()
	rows, err = d.Reader.Query("SELECT id, workspace_id, kind, entity_id, snapshot, created_at, updated_at, revoked_at, size FROM shares ORDER BY id")
	if err != nil {
		t.Fatal(err)
	}
	for rows.Next() {
		var ws string
		var s refShare
		if err := rows.Scan(&s.ID, &ws, &s.Kind, &s.EntityID, &s.Snapshot, &s.CreatedAt, &s.UpdatedAt, &s.RevokedAt, &s.Size); err != nil {
			t.Fatal(err)
		}
		out[ws].Shares = append(out[ws].Shares, s)
		snap, rev := "NULL", "NULL"
		if s.Snapshot != nil {
			snap = *s.Snapshot
		}
		if s.RevokedAt != nil {
			rev = fmt.Sprint(*s.RevokedAt)
		}
		fmt.Fprintf(&dump, "S %s %s %s %s %d %d %s %d %s\n", s.ID, ws, s.Kind, s.EntityID, s.CreatedAt, s.UpdatedAt, rev, s.Size, snap)
	}
	rows.Close()
	return out, dump.String()
}

// The expected responses, from the reference model.

func expectWorkspace(w *refWorkspace, quota int64) map[string]any {
	var live, used int64
	for _, e := range w.Entities {
		if !e.Deleted {
			live++
		}
		used += e.Size
	}
	for _, s := range w.Shares {
		if s.RevokedAt == nil {
			used += s.Size
		}
	}
	return map[string]any{"workspaceId": w.ID, "name": w.Name, "createdAt": w.CreatedAt, "entityCount": live, "bytesUsed": used, "quotaBytes": quota}
}

func expectPull(w *refWorkspace, since int64, limit int) map[string]any {
	changes := []any{}
	var page []refEntity
	more := false
	for _, e := range w.Entities {
		if e.Rev <= since {
			continue
		}
		if len(page) == limit {
			more = true
			break
		}
		page = append(page, e)
	}
	cursor := w.Rev
	if len(page) > 0 {
		cursor = page[len(page)-1].Rev
	}
	for _, e := range page {
		var data any
		if !e.Deleted && e.Data != nil {
			data = json.RawMessage(*e.Data)
		}
		changes = append(changes, map[string]any{"type": e.Type, "id": e.ID, "updatedAt": e.UpdatedAt, "deleted": e.Deleted, "deviceId": e.DeviceID, "data": data, "rev": e.Rev})
	}
	return map[string]any{"changes": changes, "cursor": cursor, "hasMore": more}
}

func shareName(snapshot string) any {
	var v map[string]any
	if json.Unmarshal([]byte(snapshot), &v) != nil {
		return nil
	}
	for _, root := range []string{"collection", "request"} {
		if m, ok := v[root].(map[string]any); ok {
			if n, ok := m["name"].(string); ok {
				return n
			}
		}
	}
	return nil
}

func expectShareList(w *refWorkspace, publicURL string) map[string]any {
	active := []refShare{}
	for _, s := range w.Shares {
		if s.RevokedAt == nil {
			active = append(active, s)
		}
	}
	sort.SliceStable(active, func(i, j int) bool {
		if active[i].CreatedAt != active[j].CreatedAt {
			return active[i].CreatedAt > active[j].CreatedAt
		}
		return active[i].ID < active[j].ID
	})
	list := []any{}
	for _, s := range active {
		list = append(list, map[string]any{"shareId": s.ID, "url": publicURL + "/s/" + s.ID, "kind": s.Kind, "entityId": s.EntityID,
			"name": shareName(*s.Snapshot), "createdAt": s.CreatedAt, "updatedAt": s.UpdatedAt})
	}
	return map[string]any{"shares": list}
}

func TestMigrate_PopulatedLegacyDatabaseKeepsRowsAndRouteResponses(t *testing.T) {
	ctx := context.Background()
	dir := t.TempDir()
	d, err := db.Open(dir, db.Options{})
	if err != nil {
		t.Fatal(err)
	}
	if _, err := d.Migrate(ctx, 2, time.Now); err != nil {
		t.Fatal(err)
	}
	seed, err := os.ReadFile(filepath.Join("..", "db", "testdata", "legacy_seed.sql"))
	if err != nil {
		t.Fatal(err)
	}
	if _, err := d.Writer.Exec(string(seed)); err != nil {
		t.Fatal(err)
	}
	ref, dumpV2 := legacyRows(t, d)
	var entities, shares, revoked int
	for _, w := range ref {
		entities += len(w.Entities)
		for _, s := range w.Shares {
			shares++
			if s.RevokedAt != nil {
				revoked++
			}
		}
	}
	if len(ref) != 3 || entities != 1000 || shares != 9 || revoked != 4 {
		t.Fatalf("seed: %d workspaces, %d entities, %d shares (%d revoked)", len(ref), entities, shares, revoked)
	}

	// The legacy rows are identical after each migration.
	for _, to := range []int{3, 4} {
		if _, err := d.Migrate(ctx, to, time.Now); err != nil {
			t.Fatalf("migrate to %d: %v", to, err)
		}
		if _, dump := legacyRows(t, d); dump != dumpV2 {
			t.Fatalf("legacy rows changed by migration %04d", to)
		}
		rows, err := d.Writer.Query("PRAGMA foreign_key_check")
		if err != nil {
			t.Fatal(err)
		}
		if rows.Next() {
			t.Fatalf("foreign_key_check reported violations after %04d", to)
		}
		rows.Close()
	}
	var n int
	if err := d.Reader.QueryRow("SELECT COUNT(*) FROM sqlite_master WHERE name = 'rate_limits'").Scan(&n); err != nil || n != 0 {
		t.Fatal("rate_limits survived 0003")
	}
	if err := d.Reader.QueryRow("SELECT COUNT(*) FROM entities WHERE updated_by <> ''").Scan(&n); err != nil || n != 0 {
		t.Fatalf("%d legacy entities gained an author", n)
	}
	if err := d.Reader.QueryRow("SELECT COUNT(*) FROM workspaces WHERE team_id IS NOT NULL").Scan(&n); err != nil || n != 0 {
		t.Fatalf("%d legacy workspaces gained a team", n)
	}
	d.Close()

	// Serve the migrated database; every legacy read route answers as before.
	h := tu.New(t, tu.WithDataDir(dir))
	epoch := h.App.SyncEpoch()
	if epoch == "" {
		t.Fatal("no sync epoch")
	}
	for id, w := range ref {
		token := seedTokens[id]
		res := h.Call(t, "/v1/workspace", tu.Req{Token: token})
		if res.StatusCode != 200 {
			t.Fatalf("%s: GET /v1/workspace %d %s", id, res.StatusCode, res.Body)
		}
		tu.JSONEq(t, res.Body, expectWorkspace(w, h.Config.QuotaLegacyBytes))

		// Pull: the default page size, a small page size, and from the middle; then follow every
		// page to the end.
		for _, q := range []struct {
			since int64
			limit int
		}{{0, 500}, {0, 7}, {w.Rev / 2, 500}, {w.Rev / 2, 1}, {w.Rev, 500}, {w.Rev + 10, 500}} {
			since := q.since
			for page := 0; ; page++ {
				path := fmt.Sprintf("/v1/sync?since=%d", since)
				if q.limit != 500 {
					path += fmt.Sprintf("&limit=%d", q.limit)
				}
				res := h.Call(t, path, tu.Req{Token: token})
				if res.StatusCode != 200 {
					t.Fatalf("%s %s: %d %s", id, path, res.StatusCode, res.Body)
				}
				got := res.JSON(t)
				if got["syncEpoch"] != epoch {
					t.Fatalf("%s %s: syncEpoch %v", id, path, got["syncEpoch"])
				}
				delete(got, "syncEpoch") // the one addition (W10)
				want := expectPull(w, since, q.limit)
				tu.JSONEq(t, got, want)
				if !want["hasMore"].(bool) || page > 2 {
					break
				}
				since = want["cursor"].(int64)
			}
		}

		res = h.Call(t, "/v1/shares", tu.Req{Token: token})
		if res.StatusCode != 200 {
			t.Fatalf("%s: GET /v1/shares %d", id, res.StatusCode)
		}
		tu.JSONEq(t, res.Body, expectShareList(w, h.Config.PublicURL))

		for _, s := range w.Shares {
			js := h.Call(t, "/s/"+s.ID+".json", tu.Req{})
			page := h.Call(t, "/s/"+s.ID, tu.Req{})
			if s.RevokedAt != nil {
				if js.StatusCode != 410 || page.StatusCode != 410 {
					t.Fatalf("revoked %s: %d %d", s.ID, js.StatusCode, page.StatusCode)
				}
				tu.JSONEq(t, js.Body, `{"error":{"code":"gone","message":"This share link has been revoked"}}`)
				want, err := sharepage.RenderError(410)
				if err != nil || string(page.Body) != string(want) {
					t.Fatalf("revoked %s page differs (%v)", s.ID, err)
				}
				continue
			}
			if js.StatusCode != 200 || page.StatusCode != 200 {
				t.Fatalf("share %s: %d %d", s.ID, js.StatusCode, page.StatusCode)
			}
			tu.JSONEq(t, js.Body, map[string]any{"shareId": s.ID, "kind": s.Kind, "createdAt": s.CreatedAt, "updatedAt": s.UpdatedAt,
				"snapshot": json.RawMessage(*s.Snapshot)})
			snap, err := jsonx.Parse([]byte(*s.Snapshot))
			if err != nil {
				t.Fatal(err)
			}
			want, err := sharepage.Render(sharepage.NormalizeSnapshot(s.Kind, snap), sharepage.Meta{ShareID: s.ID, CreatedAt: s.CreatedAt, UpdatedAt: s.UpdatedAt})
			if err != nil || string(page.Body) != string(want) {
				t.Fatalf("share %s page differs from the page of its v2 snapshot (%v)", s.ID, err)
			}
		}
	}

	// The write routes work on the migrated rows.
	w1, tok1 := ref["01JSEEDWS00000000000000001"], seedTokens["01JSEEDWS00000000000000001"]
	var target refEntity
	for _, e := range w1.Entities {
		if !e.Deleted && e.DeviceID == "" {
			target = e
			break
		}
	}
	push := func(token string, c map[string]any) map[string]any {
		t.Helper()
		res := h.Call(t, "/v1/sync", tu.Req{Token: token, JSON: tu.Changes(c)})
		if res.StatusCode != 200 {
			t.Fatalf("push: %d %s", res.StatusCode, res.Body)
		}
		return res.JSON(t)["results"].([]any)[0].(map[string]any)
	}
	if r := push(tok1, tu.Change(target.Type, target.ID, target.UpdatedAt-1, nil)); r["status"] != "stale" {
		t.Fatalf("older change on a migrated row: %v", r)
	}
	applied := push(tok1, tu.Change(target.Type, target.ID, target.UpdatedAt+1, map[string]any{"id": target.ID, "name": "renamed"}))
	if applied["status"] != "applied" || applied["rev"].(float64) <= float64(w1.Rev) {
		t.Fatalf("newer change on a migrated row: %v", applied)
	}
	res := h.Call(t, fmt.Sprintf("/v1/sync?since=%d", int64(applied["rev"].(float64))-1), tu.Req{Token: tok1})
	if ch := res.JSON(t)["changes"].([]any); len(ch) != 1 || ch[0].(map[string]any)["data"].(map[string]any)["name"] != "renamed" {
		t.Fatalf("pull after the push: %s", res.Body)
	}
	before := h.Call(t, "/v1/workspace", tu.Req{Token: tok1}).JSON(t)["entityCount"].(float64)
	if r := push(tok1, tu.Tombstone(target.Type, target.ID, target.UpdatedAt+2)); r["status"] != "applied" {
		t.Fatalf("tombstone: %v", r)
	}
	if after := h.Call(t, "/v1/workspace", tu.Req{Token: tok1}).JSON(t)["entityCount"].(float64); after != before-1 {
		t.Fatalf("entityCount %v -> %v", before, after)
	}
	// Shares: a revoked seed share is 410 on update, an active one can be updated and revoked, a
	// new one created.
	if r := h.Call(t, "/v1/shares/SeedShareW1Revokd00001", tu.Req{Method: "PUT", Token: tok1, JSON: map[string]any{"snapshot": tu.SampleCollectionSnapshot()}}); r.StatusCode != 410 {
		t.Fatalf("update revoked: %d", r.StatusCode)
	}
	if r := h.Call(t, "/v1/shares/SeedShareW1Active00001", tu.Req{Method: "PUT", Token: tok1, JSON: map[string]any{"snapshot": tu.SampleCollectionSnapshot()}}); r.StatusCode != 200 {
		t.Fatalf("update active: %d %s", r.StatusCode, r.Body)
	}
	if r := h.Call(t, "/v1/shares/SeedShareW1Active00002", tu.Req{Method: "DELETE", Token: tok1}); r.StatusCode != 200 {
		t.Fatalf("revoke: %d", r.StatusCode)
	}
	if r := h.Call(t, "/s/SeedShareW1Active00002.json", tu.Req{}); r.StatusCode != 410 {
		t.Fatalf("revoked share still served: %d", r.StatusCode)
	}
	createShare(t, h, tok1, nil, "")
	// Another workspace's share is not this token's.
	if r := h.Call(t, "/v1/shares/SeedShareW2Active00001", tu.Req{Method: "DELETE", Token: tok1}); r.StatusCode != 404 {
		t.Fatalf("cross-workspace revoke: %d", r.StatusCode)
	}
	// Rotation swaps the seeded token.
	rot := h.Call(t, "/v1/workspace/token/rotate", tu.Req{Method: "POST", Token: tok1})
	if rot.StatusCode != 200 {
		t.Fatalf("rotate: %d %s", rot.StatusCode, rot.Body)
	}
	if r := h.Call(t, "/v1/workspace", tu.Req{Token: tok1}); r.StatusCode != 401 {
		t.Fatalf("old token after rotate: %d", r.StatusCode)
	}
	if r := h.Call(t, "/v1/workspace", tu.Req{Token: rot.JSON(t)["token"].(string)}); r.StatusCode != 200 {
		t.Fatalf("new token: %d", r.StatusCode)
	}
	if h.Count(t, "entities", "") != 1000 {
		t.Fatalf("entities %d (a tombstone keeps its row)", h.Count(t, "entities", ""))
	}
}
