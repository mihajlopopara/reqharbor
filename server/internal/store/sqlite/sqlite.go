// Package sqlite implements store.Store on the SQLite pools of internal/db. All SQL lives here.
package sqlite

import (
	"context"
	"database/sql"
	"errors"
	"fmt"

	"github.com/mihajlopopara/reqharbor/server/internal/db"
	"github.com/mihajlopopara/reqharbor/server/internal/store"
)

// Store is the SQLite store.
type Store struct {
	writer *sql.DB
	reader *sql.DB
}

// New returns a store over d's writer and reader pools.
func New(d *db.DB) *Store { return &Store{writer: d.Writer, reader: d.Reader} }

// Read implements store.Store.
func (s *Store) Read(ctx context.Context, fn func(store.ReadTx) error) error {
	tx, err := s.reader.BeginTx(ctx, nil)
	if err != nil {
		return err
	}
	defer tx.Rollback() //nolint:errcheck // read-only
	return fn(&rtx{ctx: ctx, tx: tx})
}

// Write implements store.Store.
func (s *Store) Write(ctx context.Context, fn func(store.WriteTx) error) (err error) {
	tx, err := s.writer.BeginTx(ctx, nil) // BEGIN IMMEDIATE
	if err != nil {
		return err
	}
	committed := false
	defer func() {
		if !committed {
			_ = tx.Rollback()
		}
	}()
	if err = fn(&wtx{rtx{ctx: ctx, tx: tx}}); err != nil {
		return err
	}
	if err = tx.Commit(); err != nil {
		return err
	}
	committed = true
	return nil
}

type rtx struct {
	ctx context.Context
	tx  *sql.Tx
}

type wtx struct{ rtx }

func notFound(err error) error {
	if errors.Is(err, sql.ErrNoRows) {
		return store.ErrNotFound
	}
	return err
}

func (t *rtx) row(q string, args ...any) *sql.Row { return t.tx.QueryRowContext(t.ctx, q, args...) }

func (t *rtx) exec(q string, args ...any) (sql.Result, error) {
	return t.tx.ExecContext(t.ctx, q, args...)
}

func scanWorkspace(r *sql.Row) (*store.Workspace, error) {
	var w store.Workspace
	if err := r.Scan(&w.ID, &w.Name, &w.TokenHash, &w.Rev, &w.CreatedAt); err != nil {
		return nil, notFound(err)
	}
	return &w, nil
}

func (t *rtx) WorkspaceByTokenHash(hash string) (*store.Workspace, error) {
	return scanWorkspace(t.row("SELECT id, name, token_hash, rev, created_at FROM workspaces WHERE token_hash = ?", hash))
}

func (t *rtx) Workspace(id string) (*store.Workspace, error) {
	return scanWorkspace(t.row("SELECT id, name, token_hash, rev, created_at FROM workspaces WHERE id = ?", id))
}

func (t *rtx) WorkspaceUsage(id string) (int64, error) {
	var used int64
	err := t.row(`SELECT (SELECT COALESCE(SUM(size), 0) FROM entities WHERE workspace_id = ?1)
	                   + (SELECT COALESCE(SUM(size), 0) FROM shares WHERE workspace_id = ?1 AND revoked_at IS NULL)`, id).Scan(&used)
	return used, err
}

func (t *rtx) LegacyTotalBytes() (int64, error) {
	var used int64
	err := t.row(`SELECT (SELECT COALESCE(SUM(size), 0) FROM entities)
	                   + (SELECT COALESCE(SUM(size), 0) FROM shares WHERE revoked_at IS NULL)`).Scan(&used)
	return used, err
}

func (t *rtx) LiveEntityCount(id string) (int64, error) {
	var n int64
	err := t.row("SELECT COUNT(*) FROM entities WHERE workspace_id = ? AND deleted = 0", id).Scan(&n)
	return n, err
}

func (t *rtx) EntitiesSince(workspaceID string, since int64, limit int) ([]store.Entity, error) {
	rows, err := t.tx.QueryContext(t.ctx, `SELECT type, id, rev, updated_at, deleted, device_id, data, size FROM entities
	     WHERE workspace_id = ? AND rev > ? ORDER BY rev ASC LIMIT ?`, workspaceID, since, limit)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var out []store.Entity
	for rows.Next() {
		var e store.Entity
		var deleted int64
		var data sql.NullString
		if err := rows.Scan(&e.Type, &e.ID, &e.Rev, &e.UpdatedAt, &deleted, &e.DeviceID, &data, &e.Size); err != nil {
			return nil, err
		}
		e.Deleted = deleted == 1
		if data.Valid {
			e.Data = []byte(data.String)
		}
		out = append(out, e)
	}
	return out, rows.Err()
}

func (t *rtx) Share(id string) (*store.Share, error) {
	var s store.Share
	var snap sql.NullString
	var revoked sql.NullInt64
	err := t.row(`SELECT id, workspace_id, kind, entity_id, snapshot, size, created_at, updated_at, revoked_at
	              FROM shares WHERE id = ?`, id).
		Scan(&s.ID, &s.WorkspaceID, &s.Kind, &s.EntityID, &snap, &s.Size, &s.CreatedAt, &s.UpdatedAt, &revoked)
	if err != nil {
		return nil, notFound(err)
	}
	if snap.Valid {
		s.Snapshot = []byte(snap.String)
	}
	if revoked.Valid {
		v := revoked.Int64
		s.RevokedAt = &v
	}
	return &s, nil
}

func (t *rtx) ActiveShares(workspaceID string) ([]store.ShareListItem, error) {
	rows, err := t.tx.QueryContext(t.ctx, `SELECT id, kind, entity_id, created_at, updated_at,
	        COALESCE(json_extract(snapshot, '$.collection.name'), json_extract(snapshot, '$.request.name')) AS name
	     FROM shares WHERE workspace_id = ? AND revoked_at IS NULL ORDER BY created_at DESC, id`, workspaceID)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var out []store.ShareListItem
	for rows.Next() {
		var it store.ShareListItem
		var name any
		if err := rows.Scan(&it.ID, &it.Kind, &it.EntityID, &it.CreatedAt, &it.UpdatedAt, &name); err != nil {
			return nil, err
		}
		// json_extract returns TEXT only for JSON strings; numbers, booleans and objects are
		// not names (the Worker checks typeof name === "string").
		if s, ok := name.(string); ok {
			it.Name = &s
		}
		out = append(out, it)
	}
	return out, rows.Err()
}

func (t *rtx) ServerState(key string) (string, error) {
	var v string
	if err := t.row("SELECT value FROM server_state WHERE key = ?", key).Scan(&v); err != nil {
		return "", notFound(err)
	}
	return v, nil
}

func (t *wtx) LockWorkspace(string) error { return nil }

func (t *wtx) InsertWorkspace(w store.Workspace) error {
	_, err := t.exec("INSERT INTO workspaces (id, name, token_hash, rev, created_at) VALUES (?, ?, ?, ?, ?)",
		w.ID, w.Name, w.TokenHash, w.Rev, w.CreatedAt)
	return err
}

func (t *wtx) RotateToken(id, oldHash, newHash string) (bool, error) {
	res, err := t.exec("UPDATE workspaces SET token_hash = ? WHERE id = ? AND token_hash = ?", newHash, id, oldHash)
	if err != nil {
		return false, err
	}
	n, err := res.RowsAffected()
	return n == 1, err
}

func (t *wtx) SetWorkspaceRev(id string, rev int64) error {
	_, err := t.exec("UPDATE workspaces SET rev = ? WHERE id = ?", rev, id)
	return err
}

func (t *wtx) EntityMeta(workspaceID, typ, id string) (*store.EntityMeta, error) {
	var m store.EntityMeta
	err := t.row("SELECT updated_at, device_id, size FROM entities WHERE workspace_id = ? AND type = ? AND id = ?",
		workspaceID, typ, id).Scan(&m.UpdatedAt, &m.DeviceID, &m.Size)
	if err != nil {
		return nil, notFound(err)
	}
	return &m, nil
}

func (t *wtx) UpsertEntity(workspaceID string, e store.Entity) error {
	var data any
	if e.Data != nil {
		data = string(e.Data)
	}
	deleted := 0
	if e.Deleted {
		deleted = 1
	}
	_, err := t.exec(`INSERT INTO entities (workspace_id, type, id, rev, updated_at, deleted, device_id, data, size)
	     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
	     ON CONFLICT (workspace_id, type, id) DO UPDATE SET
	       rev = excluded.rev, updated_at = excluded.updated_at, deleted = excluded.deleted,
	       device_id = excluded.device_id, data = excluded.data, size = excluded.size`,
		workspaceID, e.Type, e.ID, e.Rev, e.UpdatedAt, deleted, e.DeviceID, data, e.Size)
	return err
}

func (t *wtx) BumpEntityRev(workspaceID, typ, id string, rev int64) error {
	res, err := t.exec("UPDATE entities SET rev = ? WHERE workspace_id = ? AND type = ? AND id = ?", rev, workspaceID, typ, id)
	if err != nil {
		return err
	}
	if n, _ := res.RowsAffected(); n != 1 {
		return fmt.Errorf("bump rev of %s %s: %d rows changed", typ, id, n)
	}
	return nil
}

func (t *wtx) InsertShare(s store.Share) error {
	_, err := t.exec(`INSERT INTO shares (id, workspace_id, kind, entity_id, snapshot, size, created_at, updated_at, revoked_at)
	     VALUES (?, ?, ?, ?, ?, ?, ?, ?, NULL)`,
		s.ID, s.WorkspaceID, s.Kind, s.EntityID, string(s.Snapshot), s.Size, s.CreatedAt, s.UpdatedAt)
	return err
}

func (t *wtx) UpdateShareSnapshot(id, workspaceID string, snapshot []byte, size, updatedAt int64) (bool, error) {
	res, err := t.exec("UPDATE shares SET snapshot = ?, size = ?, updated_at = ? WHERE id = ? AND workspace_id = ? AND revoked_at IS NULL",
		string(snapshot), size, updatedAt, id, workspaceID)
	if err != nil {
		return false, err
	}
	n, err := res.RowsAffected()
	return n == 1, err
}

func (t *wtx) RevokeShare(id string, revokedAt int64) error {
	_, err := t.exec("UPDATE shares SET revoked_at = ?, snapshot = NULL, size = 0 WHERE id = ? AND revoked_at IS NULL", revokedAt, id)
	return err
}

func (t *wtx) SetServerState(key, value string) error {
	_, err := t.exec("INSERT INTO server_state (key, value) VALUES (?, ?) ON CONFLICT (key) DO UPDATE SET value = excluded.value", key, value)
	return err
}
