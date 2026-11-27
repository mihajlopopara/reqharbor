package sqlite

import (
	"database/sql"

	"github.com/mihajlopopara/reqharbor/server/internal/store"
)

// Account spaces (06-go-server.md §4, S6).

func (t *rtx) OpenSignupTotalBytes() (int64, error) {
	var used int64
	err := t.row(`WITH open_spaces AS (
	         SELECT w.id FROM users u
	           JOIN teams t ON t.owner_user_id = u.id AND t.personal = 1
	           JOIN workspaces w ON w.team_id = t.id
	          WHERE u.signup_via = 'open')
	       SELECT (SELECT COALESCE(SUM(size), 0) FROM entities WHERE workspace_id IN (SELECT id FROM open_spaces))
	            + (SELECT COALESCE(SUM(size), 0) FROM shares WHERE workspace_id IN (SELECT id FROM open_spaces) AND revoked_at IS NULL)`).Scan(&used)
	return used, err
}

func (t *rtx) SpaceAccess(workspaceID, userID string) (*store.SpaceAccess, error) {
	var a store.SpaceAccess
	var teamID, teamName, owner, ownerVia, ownerName, role sql.NullString
	var personal sql.NullInt64
	w := &a.Workspace
	err := t.row(`SELECT w.id, w.name, w.token_hash, w.rev, w.created_at, w.team_id, t.name, t.personal, t.owner_user_id, u.signup_via, u.name, m.role
	      FROM workspaces w
	      LEFT JOIN teams t        ON t.id = w.team_id
	      LEFT JOIN users u        ON u.id = t.owner_user_id
	      LEFT JOIN team_members m ON m.team_id = w.team_id AND m.user_id = ?2
	     WHERE w.id = ?1`, workspaceID, userID).
		Scan(&w.ID, &w.Name, &w.TokenHash, &w.Rev, &w.CreatedAt, &teamID, &teamName, &personal, &owner, &ownerVia, &ownerName, &role)
	if err != nil {
		return nil, notFound(err)
	}
	a.TeamID, a.Role = nullStr(&teamID), nullStr(&role)
	a.TeamName, a.OwnerUserID, a.OwnerSignupVia, a.OwnerName = teamName.String, owner.String, ownerVia.String, ownerName.String
	a.Personal = personal.Valid && personal.Int64 == 1
	return &a, nil
}

func (t *rtx) UserSpaces(userID string) ([]store.SpaceRow, error) {
	rows, err := t.tx.QueryContext(t.ctx, `SELECT w.id, w.name, w.created_at, t.id, t.name, t.personal, m.role
	      FROM team_members m
	      JOIN teams t      ON t.id = m.team_id
	      JOIN workspaces w ON w.team_id = t.id
	     WHERE m.user_id = ?
	     ORDER BY t.personal DESC, w.created_at, w.id`, userID)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var out []store.SpaceRow
	for rows.Next() {
		var r store.SpaceRow
		var personal int64
		if err := rows.Scan(&r.WorkspaceID, &r.Name, &r.CreatedAt, &r.TeamID, &r.TeamName, &personal, &r.Role); err != nil {
			return nil, err
		}
		r.Personal = personal == 1
		out = append(out, r)
	}
	return out, rows.Err()
}
