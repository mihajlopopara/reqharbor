package sqlite

import (
	"database/sql"

	"github.com/mihajlopopara/reqharbor/server/internal/store"
)

// Teams (0005_teams.sql, 06-go-server.md §10 "M2 outline"). Business rules (roles, limits, the
// billing owner) live in internal/teams; these are the queries.

const teamCols = "t.id, t.name, t.personal, t.owner_user_id, t.created_at, COALESCE(w.id, '')"

func scanTeam(r scanner, t *store.Team, extra ...any) error {
	var personal int64
	if err := r.Scan(append([]any{&t.ID, &t.Name, &personal, &t.OwnerUserID, &t.CreatedAt, &t.WorkspaceID}, extra...)...); err != nil {
		return notFound(err)
	}
	t.Personal = personal == 1
	return nil
}

func (t *rtx) Team(id string) (*store.Team, error) {
	var out store.Team
	if err := scanTeam(t.row("SELECT "+teamCols+" FROM teams t LEFT JOIN workspaces w ON w.team_id = t.id WHERE t.id = ?", id), &out); err != nil {
		return nil, err
	}
	return &out, nil
}

const memberCols = "m.team_id, m.user_id, m.role, m.joined_at, u.name, u.email, u.avatar_url"

func scanMember(r scanner) (*store.Member, error) {
	var m store.Member
	var avatar sql.NullString
	if err := r.Scan(&m.TeamID, &m.UserID, &m.Role, &m.JoinedAt, &m.Name, &m.Email, &avatar); err != nil {
		return nil, notFound(err)
	}
	m.AvatarURL = nullStr(&avatar)
	return &m, nil
}

func (t *rtx) TeamMember(teamID, userID string) (*store.Member, error) {
	return scanMember(t.row("SELECT "+memberCols+" FROM team_members m JOIN users u ON u.id = m.user_id WHERE m.team_id = ? AND m.user_id = ?",
		teamID, userID))
}

func (t *rtx) TeamMembers(teamID string) ([]store.Member, error) {
	rows, err := t.tx.QueryContext(t.ctx, "SELECT "+memberCols+` FROM team_members m JOIN users u ON u.id = m.user_id
	     WHERE m.team_id = ?
	     ORDER BY CASE m.role WHEN 'owner' THEN 0 WHEN 'admin' THEN 1 WHEN 'editor' THEN 2 ELSE 3 END, m.joined_at, m.user_id`, teamID)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var out []store.Member
	for rows.Next() {
		m, err := scanMember(rows)
		if err != nil {
			return nil, err
		}
		out = append(out, *m)
	}
	return out, rows.Err()
}

func (t *rtx) UserTeams(userID string) ([]store.TeamRow, error) {
	rows, err := t.tx.QueryContext(t.ctx, "SELECT "+teamCols+`, m.role, (SELECT COUNT(*) FROM team_members c WHERE c.team_id = t.id)
	     FROM team_members m JOIN teams t ON t.id = m.team_id LEFT JOIN workspaces w ON w.team_id = t.id
	     WHERE m.user_id = ? AND t.personal = 0
	     ORDER BY t.name COLLATE NOCASE, t.id`, userID)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var out []store.TeamRow
	for rows.Next() {
		var r store.TeamRow
		if err := scanTeam(rows, &r.Team, &r.Role, &r.MemberCount); err != nil {
			return nil, err
		}
		out = append(out, r)
	}
	return out, rows.Err()
}

func (t *rtx) count(q string, args ...any) (int64, error) {
	var n int64
	err := t.row(q, args...).Scan(&n)
	return n, err
}

func (t *rtx) MemberCount(teamID string) (int64, error) {
	return t.count("SELECT COUNT(*) FROM team_members WHERE team_id = ?", teamID)
}

func (t *rtx) OwnerCount(teamID string) (int64, error) {
	return t.count("SELECT COUNT(*) FROM team_members WHERE team_id = ? AND role = 'owner'", teamID)
}

func (t *rtx) OwnedTeamCount(userID string) (int64, error) {
	return t.count("SELECT COUNT(*) FROM teams WHERE owner_user_id = ? AND personal = 0", userID)
}

func (t *rtx) OwnerUsage(userID string) (int64, error) {
	return t.count(`WITH owned AS (SELECT w.id FROM teams t JOIN workspaces w ON w.team_id = t.id WHERE t.owner_user_id = ?)
	       SELECT (SELECT COALESCE(SUM(size), 0) FROM entities WHERE workspace_id IN (SELECT id FROM owned))
	            + (SELECT COALESCE(SUM(size), 0) FROM shares WHERE workspace_id IN (SELECT id FROM owned) AND revoked_at IS NULL)`, userID)
}

const inviteCols = "i.id, i.team_id, i.token_hash, i.role, i.created_by, u.name, i.created_at, i.expires_at, i.max_uses, i.uses, i.revoked_at"

func scanInvite(r scanner) (*store.TeamInvite, error) {
	var i store.TeamInvite
	var by, byName sql.NullString
	var maxUses, revoked sql.NullInt64
	if err := r.Scan(&i.ID, &i.TeamID, &i.TokenHash, &i.Role, &by, &byName, &i.CreatedAt, &i.ExpiresAt, &maxUses, &i.Uses, &revoked); err != nil {
		return nil, notFound(err)
	}
	i.CreatedBy, i.CreatedByName = nullStr(&by), nullStr(&byName)
	i.MaxUses, i.RevokedAt = nullInt(&maxUses), nullInt(&revoked)
	return &i, nil
}

const inviteFrom = " FROM team_invites i LEFT JOIN users u ON u.id = i.created_by "

func (t *rtx) TeamInvite(id string) (*store.TeamInvite, error) {
	return scanInvite(t.row("SELECT "+inviteCols+inviteFrom+"WHERE i.id = ?", id))
}

func (t *rtx) TeamInviteByHash(tokenHash string) (*store.TeamInvite, error) {
	return scanInvite(t.row("SELECT "+inviteCols+inviteFrom+"WHERE i.token_hash = ?", tokenHash))
}

func (t *rtx) ActiveTeamInvites(teamID string, now int64) ([]store.TeamInvite, error) {
	rows, err := t.tx.QueryContext(t.ctx, "SELECT "+inviteCols+inviteFrom+`
	     WHERE i.team_id = ?1 AND i.revoked_at IS NULL AND i.expires_at > ?2 AND (i.max_uses IS NULL OR i.uses < i.max_uses)
	     ORDER BY i.created_at DESC, i.id`, teamID, now)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var out []store.TeamInvite
	for rows.Next() {
		i, err := scanInvite(rows)
		if err != nil {
			return nil, err
		}
		out = append(out, *i)
	}
	return out, rows.Err()
}

func (t *rtx) InvitesCreatedSince(teamID string, since int64) (int64, error) {
	return t.count("SELECT COUNT(*) FROM team_invites WHERE team_id = ? AND created_at > ?", teamID, since)
}

func (t *rtx) RemovedSince(teamID, userID string, since int64) (bool, error) {
	n, err := t.count("SELECT COUNT(*) FROM team_departures WHERE team_id = ? AND user_id = ? AND reason = 'removed' AND at >= ?",
		teamID, userID, since)
	return n > 0, err
}

func (t *rtx) UserDepartures(userID string) ([]store.Departure, error) {
	rows, err := t.tx.QueryContext(t.ctx, `SELECT d.team_id, d.user_id, d.space_id, d.team_name, d.reason, d.at, d.by_user_id
	     FROM team_departures d
	     WHERE d.user_id = ?1
	       AND d.at = (SELECT MAX(d2.at) FROM team_departures d2 WHERE d2.team_id = d.team_id AND d2.user_id = ?1)
	       AND NOT EXISTS (SELECT 1 FROM team_members m WHERE m.team_id = d.team_id AND m.user_id = ?1)
	     ORDER BY d.at DESC, d.team_id`, userID)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var out []store.Departure
	for rows.Next() {
		var d store.Departure
		var by sql.NullString
		if err := rows.Scan(&d.TeamID, &d.UserID, &d.SpaceID, &d.TeamName, &d.Reason, &d.At, &by); err != nil {
			return nil, err
		}
		d.ByUserID = nullStr(&by)
		out = append(out, d)
	}
	return out, rows.Err()
}

// ticketUsable is the condition on signup_tickets s for a usable ticket at ?now: unused, unexpired,
// and its link unrevoked, unexpired and, when limited, with room left counting both the link's
// uses and the tickets already spent on signups (a limited link never opens more signups than it
// has uses).
const ticketUsable = `s.used_at IS NULL AND s.expires_at > ?2 AND EXISTS (
	  SELECT 1 FROM team_invites i WHERE i.id = s.invite_id AND i.revoked_at IS NULL AND i.expires_at > ?2
	     AND (i.max_uses IS NULL OR i.uses + (SELECT COUNT(*) FROM signup_tickets u
	                                         WHERE u.invite_id = i.id AND u.used_at IS NOT NULL) < i.max_uses))`

func (t *rtx) UsedSignupTickets(inviteID string) (int64, error) {
	return t.count("SELECT COUNT(*) FROM signup_tickets WHERE invite_id = ? AND used_at IS NOT NULL", inviteID)
}

func (t *rtx) SignupTicketUsable(ticketHash string, now int64) (bool, error) {
	n, err := t.count("SELECT COUNT(*) FROM signup_tickets s WHERE s.ticket_hash = ?1 AND "+ticketUsable, ticketHash, now)
	return n > 0, err
}

func (t *wtx) InsertTeam(tm store.Team, joinedAt int64) error {
	if _, err := t.exec("INSERT INTO teams (id, name, personal, owner_user_id, created_at) VALUES (?, ?, 0, ?, ?)",
		tm.ID, tm.Name, tm.OwnerUserID, tm.CreatedAt); err != nil {
		return err
	}
	if _, err := t.exec("INSERT INTO team_members (team_id, user_id, role, joined_at) VALUES (?, ?, 'owner', ?)",
		tm.ID, tm.OwnerUserID, joinedAt); err != nil {
		return err
	}
	// The sentinel token_hash is never a SHA-256 hex digest, so legacy auth can never match it.
	_, err := t.exec("INSERT INTO workspaces (id, name, token_hash, rev, created_at, team_id) VALUES (?, ?, 'space:' || ?, 0, ?, ?)",
		tm.WorkspaceID, tm.Name, tm.WorkspaceID, tm.CreatedAt, tm.ID)
	return err
}

func (t *wtx) RenameTeam(id, name string) error {
	if err := one(t.exec("UPDATE teams SET name = ? WHERE id = ? AND personal = 0", name, id)); err != nil {
		return err
	}
	_, err := t.exec("UPDATE workspaces SET name = ? WHERE team_id = ?", name, id)
	return err
}

func (t *wtx) DeleteTeam(id string) error {
	return one(t.exec("DELETE FROM teams WHERE id = ? AND personal = 0", id))
}

func (t *wtx) InsertMember(teamID, userID, role string, joinedAt int64) error {
	_, err := t.exec("INSERT INTO team_members (team_id, user_id, role, joined_at) VALUES (?, ?, ?, ?)", teamID, userID, role, joinedAt)
	return err
}

func (t *wtx) SetMemberRole(teamID, userID, role string) error {
	return one(t.exec("UPDATE team_members SET role = ? WHERE team_id = ? AND user_id = ?", role, teamID, userID))
}

func (t *wtx) DeleteMember(teamID, userID string) error {
	return one(t.exec("DELETE FROM team_members WHERE team_id = ? AND user_id = ?", teamID, userID))
}

func (t *wtx) SetTeamOwner(teamID, userID string) error {
	return one(t.exec("UPDATE teams SET owner_user_id = ? WHERE id = ? AND personal = 0", userID, teamID))
}

func (t *wtx) InsertDeparture(d store.Departure) error {
	_, err := t.exec(`INSERT OR REPLACE INTO team_departures (team_id, user_id, space_id, team_name, reason, at, by_user_id)
	     VALUES (?, ?, ?, ?, ?, ?, ?)`, d.TeamID, d.UserID, d.SpaceID, d.TeamName, d.Reason, d.At, d.ByUserID)
	return err
}

func (t *wtx) InsertTeamInvite(i store.TeamInvite) error {
	_, err := t.exec(`INSERT INTO team_invites (id, team_id, token_hash, role, created_by, created_at, expires_at, max_uses, uses, revoked_at)
	     VALUES (?, ?, ?, ?, ?, ?, ?, ?, 0, NULL)`, i.ID, i.TeamID, i.TokenHash, i.Role, i.CreatedBy, i.CreatedAt, i.ExpiresAt, i.MaxUses)
	return err
}

func (t *wtx) UseTeamInvite(id string) error {
	return one(t.exec("UPDATE team_invites SET uses = uses + 1 WHERE id = ?", id))
}

func (t *wtx) RevokeTeamInvite(id, teamID string, now int64) (bool, error) {
	res, err := t.exec("UPDATE team_invites SET revoked_at = ? WHERE id = ? AND team_id = ? AND revoked_at IS NULL", now, id, teamID)
	if err != nil {
		return false, err
	}
	n, err := res.RowsAffected()
	return n == 1, err
}

func (t *wtx) RevokeTeamInvitesBy(teamID, userID string, now int64) (int64, error) {
	res, err := t.exec("UPDATE team_invites SET revoked_at = ? WHERE team_id = ? AND created_by = ? AND revoked_at IS NULL", now, teamID, userID)
	if err != nil {
		return 0, err
	}
	return res.RowsAffected()
}

func (t *wtx) RevokeAllTeamInvites(teamID string, now int64) (int64, error) {
	res, err := t.exec("UPDATE team_invites SET revoked_at = ? WHERE team_id = ? AND revoked_at IS NULL", now, teamID)
	if err != nil {
		return 0, err
	}
	return res.RowsAffected()
}

func (t *wtx) InsertSignupTicket(s store.SignupTicket) error {
	_, err := t.exec("INSERT INTO signup_tickets (ticket_hash, invite_id, expires_at, used_at) VALUES (?, ?, ?, NULL)",
		s.TicketHash, s.InviteID, s.ExpiresAt)
	return err
}

func (t *wtx) UseSignupTicket(ticketHash string, now int64) (bool, error) {
	res, err := t.exec("UPDATE signup_tickets AS s SET used_at = ?2 WHERE s.ticket_hash = ?1 AND "+ticketUsable, ticketHash, now)
	if err != nil {
		return false, err
	}
	n, err := res.RowsAffected()
	return n == 1, err
}

func (t *wtx) GCTeams(now, inviteBefore, departureBefore int64) (store.GCStats, error) {
	var st store.GCStats
	steps := []struct {
		dst  *int64
		q    string
		args []any
	}{
		{&st.Tickets, "DELETE FROM signup_tickets WHERE expires_at <= ?", []any{now}},
		{&st.Invites, "DELETE FROM team_invites WHERE expires_at <= ?1 OR revoked_at <= ?1", []any{inviteBefore}},
		{&st.Departures, "DELETE FROM team_departures WHERE at <= ?", []any{departureBefore}},
	}
	for _, s := range steps {
		res, err := t.exec(s.q, s.args...)
		if err != nil {
			return st, err
		}
		*s.dst, _ = res.RowsAffected()
	}
	return st, nil
}
