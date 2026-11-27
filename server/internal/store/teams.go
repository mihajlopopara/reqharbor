package store

// Team rows (0005_teams.sql, 06-go-server.md §10 "M2 outline"). All timestamps are Unix
// milliseconds.

// Team is one team with its space. Personal teams are the hidden one-member teams behind the
// personal spaces (DEV-4).
type Team struct {
	ID          string
	Name        string
	Personal    bool
	OwnerUserID string // the billing owner
	CreatedAt   int64
	WorkspaceID string
}

// Member is one membership with the member's profile.
type Member struct {
	TeamID    string
	UserID    string
	Role      string
	JoinedAt  int64
	Name      string
	Email     string
	AvatarURL *string
}

// TeamRow is one team a user belongs to (GET /v1/teams).
type TeamRow struct {
	Team
	Role        string
	MemberCount int64
}

// TeamInvite is one invite link. MaxUses nil means unlimited; CreatedByName is the creator's
// current name on reads (nil when the creator no longer exists or was never recorded).
type TeamInvite struct {
	ID            string
	TeamID        string
	TokenHash     string
	Role          string
	CreatedBy     *string
	CreatedByName *string
	CreatedAt     int64
	ExpiresAt     int64
	MaxUses       *int64
	Uses          int64
	RevokedAt     *int64
}

// Usable reports whether the link can still be accepted at now (removal blocks are separate).
func (i *TeamInvite) Usable(now int64) bool {
	return i.RevokedAt == nil && i.ExpiresAt > now && (i.MaxUses == nil || i.Uses < *i.MaxUses)
}

// Departure reasons.
const (
	DepartureRemoved = "removed"
	DepartureLeft    = "left"
	DepartureDeleted = "deleted"
)

// Departure is a removal, a leave or a team deletion as one member saw it.
type Departure struct {
	TeamID   string
	UserID   string
	SpaceID  string
	TeamName string
	Reason   string
	At       int64
	ByUserID *string
}

// SignupTicket lets an invitee without an account sign up in invite mode.
type SignupTicket struct {
	TicketHash string
	InviteID   string
	ExpiresAt  int64
}

// TeamsReadTx are the team queries.
type TeamsReadTx interface {
	// Team returns a team (personal or not) with its space; ErrNotFound for an unknown id.
	Team(id string) (*Team, error)
	// TeamMember returns userID's membership of teamID, or ErrNotFound.
	TeamMember(teamID, userID string) (*Member, error)
	// TeamMembers lists a team's members, by role (owner first) then join time.
	TeamMembers(teamID string) ([]Member, error)
	// UserTeams lists the non-personal teams userID belongs to, by name.
	UserTeams(userID string) ([]TeamRow, error)
	MemberCount(teamID string) (int64, error)
	OwnerCount(teamID string) (int64, error)
	// OwnedTeamCount counts the non-personal teams whose billing owner is userID.
	OwnedTeamCount(userID string) (int64, error)
	// OwnerUsage is the billing owner's aggregate: entity data plus active share snapshots of the
	// personal space and every team space userID owns.
	OwnerUsage(userID string) (int64, error)

	TeamInvite(id string) (*TeamInvite, error)
	TeamInviteByHash(tokenHash string) (*TeamInvite, error)
	// ActiveTeamInvites lists the invites of a team that are usable at now, newest first.
	ActiveTeamInvites(teamID string, now int64) ([]TeamInvite, error)
	// InvitesCreatedSince counts a team's invites created after since (the daily create limit).
	InvitesCreatedSince(teamID string, since int64) (int64, error)
	// RemovedSince reports whether userID was removed from teamID at or after since.
	RemovedSince(teamID, userID string, since int64) (bool, error)
	// UserDepartures returns, per team, the latest departure of userID from a team it is not a
	// member of now, newest first.
	UserDepartures(userID string) ([]Departure, error)
	// UsedSignupTickets counts the invite's tickets already spent on a signup.
	UsedSignupTickets(inviteID string) (int64, error)
	// SignupTicketUsable reports whether the ticket is unused, unexpired and its invite usable,
	// with room left on a limited invite (uses plus used tickets below max_uses).
	SignupTicketUsable(ticketHash string, now int64) (bool, error)
}

// TeamsWriteTx are the team writes.
type TeamsWriteTx interface {
	// InsertTeam creates a non-personal team, its owner membership and its space.
	InsertTeam(t Team, joinedAt int64) error
	// RenameTeam renames a team and its space.
	RenameTeam(id, name string) error
	// DeleteTeam deletes a non-personal team; members, invites, the space, its entities and its
	// shares cascade.
	DeleteTeam(id string) error
	InsertMember(teamID, userID, role string, joinedAt int64) error
	SetMemberRole(teamID, userID, role string) error
	DeleteMember(teamID, userID string) error
	SetTeamOwner(teamID, userID string) error
	// InsertDeparture records one departure (a same-millisecond duplicate replaces the row).
	InsertDeparture(d Departure) error

	InsertTeamInvite(i TeamInvite) error
	// UseTeamInvite adds one use.
	UseTeamInvite(id string) error
	// RevokeTeamInvite revokes one active invite of the team; false when there is none to revoke.
	RevokeTeamInvite(id, teamID string, now int64) (bool, error)
	// RevokeTeamInvitesBy revokes the team's unrevoked invites created by userID.
	RevokeTeamInvitesBy(teamID, userID string, now int64) (int64, error)
	// RevokeAllTeamInvites revokes every unrevoked invite of the team.
	RevokeAllTeamInvites(teamID string, now int64) (int64, error)

	InsertSignupTicket(t SignupTicket) error
	// UseSignupTicket marks a usable ticket used; false when it is not usable.
	UseSignupTicket(ticketHash string, now int64) (bool, error)

	// GCTeams deletes invites expired or revoked before inviteBefore, departures before
	// departureBefore, and expired signup tickets.
	GCTeams(now, inviteBefore, departureBefore int64) (GCStats, error)
}
