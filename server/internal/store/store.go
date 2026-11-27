// Package store is the persistence boundary (06-go-server.md §2.3). Handlers and services
// depend only on these interfaces; every SQL statement lives in store/sqlite.
package store

import (
	"context"
	"errors"
)

// ErrNotFound is returned by single-row lookups that find nothing.
var ErrNotFound = errors.New("store: not found")

// Store runs read and write transactions.
type Store interface {
	// Read runs fn in one read transaction (a consistent snapshot).
	Read(ctx context.Context, fn func(ReadTx) error) error
	// Write runs fn in one serialised write transaction; fn's error rolls it back.
	Write(ctx context.Context, fn func(WriteTx) error) error
}

// Workspace is a legacy token workspace.
type Workspace struct {
	ID        string
	Name      string
	TokenHash string
	Rev       int64
	CreatedAt int64
}

// Entity is a stored synced entity.
type Entity struct {
	Type      string
	ID        string
	Rev       int64
	UpdatedAt int64
	Deleted   bool
	DeviceID  string
	Data      []byte // nil for tombstones
	Size      int64
	// UpdatedBy is the user id of the author of this version ("" for legacy workspaces). On
	// reads, UpdatedByName is that user's current name, or nil when the user no longer exists.
	UpdatedBy     string
	UpdatedByName *string
}

// EntityMeta is what LWW needs about the stored copy.
type EntityMeta struct {
	UpdatedAt int64
	DeviceID  string
	Size      int64
}

// Share is one share link.
type Share struct {
	ID          string
	WorkspaceID string
	Kind        string
	EntityID    string
	Snapshot    []byte // nil once revoked
	Size        int64
	CreatedAt   int64
	UpdatedAt   int64
	RevokedAt   *int64
	// CreatedBy is the creating user's id in an account space (nil for legacy shares); Options
	// is the stored share-options JSON (nil when none).
	CreatedBy *string
	Options   []byte
}

// ShareListItem is an active share as listed by GET /v1/shares.
type ShareListItem struct {
	ID        string
	Kind      string
	EntityID  string
	Name      *string // the root entity's name when it is a string
	CreatedAt int64
	UpdatedAt int64
	// CreatedBy and Options as on Share; CreatedByName is the creator's current name, nil when
	// the user no longer exists ("Former member").
	CreatedBy     *string
	CreatedByName *string
	Options       []byte
}

// SpaceAccess is what space authorisation needs about a workspace and one user (§4).
type SpaceAccess struct {
	Workspace Workspace
	// TeamID is nil for a legacy workspace, which /v1/spaces never addresses.
	TeamID   *string
	TeamName string
	Personal bool
	// OwnerUserID is the team's billing owner; OwnerSignupVia is that user's users.signup_via
	// (the REQHARBOR_OPEN_SIGNUP_TOTAL_BYTES class of a personal space) and OwnerName its name.
	OwnerUserID    string
	OwnerSignupVia string
	OwnerName      string
	// Role is the user's membership role, nil when the user is not a member.
	Role *string
}

// SpaceRow is one space a user is a member of (GET /v1/me/spaces).
type SpaceRow struct {
	WorkspaceID string
	Name        string
	CreatedAt   int64
	TeamID      string
	TeamName    string
	Personal    bool
	Role        string
}

// ReadTx are the queries available in any transaction.
type ReadTx interface {
	AccountsReadTx
	TeamsReadTx
	WorkspaceByTokenHash(hash string) (*Workspace, error)
	Workspace(id string) (*Workspace, error)
	// WorkspaceUsage is entity data plus active share snapshots, in bytes (QA-R1-024).
	WorkspaceUsage(id string) (int64, error)
	// LiveEntityCount counts entities that are not tombstones.
	LiveEntityCount(id string) (int64, error)
	// EntitiesSince returns up to limit entities with rev > since, by rev.
	EntitiesSince(workspaceID string, since int64, limit int) ([]Entity, error)
	Share(id string) (*Share, error)
	ActiveShares(workspaceID string) ([]ShareListItem, error)
	ServerState(key string) (string, error)
	// LegacyTotalBytes sums WorkspaceUsage over every legacy workspace (team_id IS NULL).
	LegacyTotalBytes() (int64, error)
	// OpenSignupTotalBytes sums WorkspaceUsage over the personal spaces of accounts created
	// through open signup (users.signup_via = 'open').
	OpenSignupTotalBytes() (int64, error)
	// SpaceAccess loads workspace id with userID's membership; ErrNotFound for an unknown id.
	SpaceAccess(workspaceID, userID string) (*SpaceAccess, error)
	// UserSpaces lists the spaces userID is a member of, the personal space first.
	UserSpaces(userID string) ([]SpaceRow, error)
}

// WriteTx adds the writes.
type WriteTx interface {
	ReadTx
	AccountsWriteTx
	TeamsWriteTx
	// LockWorkspace is a no-op on SQLite (the writer is serialised); Postgres would SELECT … FOR UPDATE.
	LockWorkspace(id string) error
	InsertWorkspace(w Workspace) error
	// RotateToken swaps the token hash only if it still equals oldHash; it reports whether it did.
	RotateToken(id, oldHash, newHash string) (bool, error)
	SetWorkspaceRev(id string, rev int64) error
	EntityMeta(workspaceID, typ, id string) (*EntityMeta, error)
	UpsertEntity(workspaceID string, e Entity) error
	BumpEntityRev(workspaceID, typ, id string, rev int64) error
	InsertShare(s Share) error
	// UpdateShareSnapshot replaces an active share's snapshot, and its options when options is
	// not nil; false when it is revoked or missing.
	UpdateShareSnapshot(id, workspaceID string, snapshot, options []byte, size, updatedAt int64) (bool, error)
	// RevokeShare revokes an active share and drops its snapshot.
	RevokeShare(id string, revokedAt int64) error
	SetServerState(key, value string) error
}
