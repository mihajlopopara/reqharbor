package db

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"io"
	"net/url"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"time"

	"github.com/mihajlopopara/reqharbor/server/internal/ids"
)

// Restore (06-go-server.md §6 "Restore"). Replacing reqharbor.db by hand rolls workspaces.rev back
// below the cursors that clients hold, and clients never lower their cursor, so they would skip
// every new rev up to their old cursor. Restore instead shifts every restored rev above the
// pre-restore head and rotates sync_epoch, so that every client receives every entity again and
// M0-or-newer clients also re-push what they changed after the backup.

// DefaultRevOffset is added to the highest restored rev when there is no pre-restore head to go
// by (no current database, or a workspace it does not have): far above any real head, and still
// room for about two million restores below 2^53.
const DefaultRevOffset int64 = 1 << 32

// maxSafeRev is the largest rev a JavaScript or JSON client can represent exactly (2^53 - 1).
const maxSafeRev int64 = 1<<53 - 1

// server_state keys written by Restore (app reads instance_id and sync_epoch at startup).
const (
	stateInstanceID   = "instance_id"
	stateSyncEpoch    = "sync_epoch"
	stateRestoredAt   = "restored_at"
	stateRestoredFrom = "restored_from"
)

// Suffixes of the restore's working files next to reqharbor.db.
const (
	restoreTmpSuffix = ".restore-tmp"
	preRestoreInfix  = ".pre-restore-"
)

// RestoreOptions customise Restore.
type RestoreOptions struct {
	// RevBase, when > 0, is the base added to every restored rev instead of the computed one
	// (--rev-base). It must exceed every recorded pre-restore head and every restored head.
	RevBase int64
	// Now stamps restored_at, the pre-restore file name and the new epoch (time.Now when nil).
	Now func() time.Time
}

// RestoreResult summarises a restore.
type RestoreResult struct {
	From string
	// BackupVersion is the backup's schema version; Version the version after migrating it.
	BackupVersion, Version int
	// Workspaces is the number of workspaces whose revs were shifted (legacy and spaces).
	Workspaces int
	// MinBase and MaxBase are the smallest and largest base added to a workspace's revs.
	MinBase, MaxBase int64
	// Unrecorded is the number of workspaces that had no pre-restore head (DefaultRevOffset).
	Unrecorded       int
	PreviousEpoch    string
	NewEpoch         string
	InstanceID       string
	InstanceReplaced bool // the backup's instance_id differed from the current one, which was kept
	// PreRestorePath is where the previous database was kept ("" when there was none).
	PreRestorePath string
}

// RestoreError is a refusal with a message for the admin (no stack of wrapped errors).
type RestoreError struct{ Msg string }

func (e *RestoreError) Error() string { return e.Msg }

func refuse(format string, args ...any) error {
	return &RestoreError{Msg: fmt.Sprintf(format, args...)}
}

// Restore replaces the database in dataDir with the backup at file. The server must be stopped:
// Restore takes the single-instance lock and returns ErrLocked while serve holds it.
//
//  1. Record the pre-restore head of every workspace (and instance_id) from the current database,
//     if there is one, and checkpoint its WAL (refused while another process holds it open).
//  2. Copy file to reqharbor.db.restore-tmp, run integrity_check, apply the downgrade guard and
//     migrate the copy forward.
//  3. Shift the revs of every workspace above max(pre-restore head, restored head), in one write
//     transaction, in two passes because entities has a unique (workspace_id, rev) index.
//  4. Rotate sync_epoch and record restored_at and restored_from; instance_id stays.
//  5. Rename the current database to reqharbor.db.pre-restore-<ts> (never deleted here) and its
//     -wal to <that>-wal, drop its -shm, and rename the copy into place.
func Restore(ctx context.Context, dataDir, file string, opts RestoreOptions) (res *RestoreResult, err error) {
	now := opts.Now
	if now == nil {
		now = time.Now
	}
	if opts.RevBase < 0 {
		return nil, refuse("--rev-base must be a positive integer")
	}
	src, err := filepath.Abs(file)
	if err != nil {
		return nil, err
	}
	fi, err := os.Stat(src)
	if err != nil {
		return nil, refuse("cannot read backup %s: %v", file, err)
	}
	if !fi.Mode().IsRegular() {
		return nil, refuse("backup %s is not a regular file", file)
	}

	lock, err := AcquireLock(dataDir)
	if err != nil {
		return nil, err
	}
	defer func() {
		if e := lock.Release(); e != nil && err == nil {
			err = e
		}
	}()

	live := filepath.Join(dataDir, FileName)
	tmp := live + restoreTmpSuffix
	for _, p := range []string{live, tmp} {
		if lfi, e := os.Stat(p); e == nil && os.SameFile(fi, lfi) {
			return nil, refuse("backup %s is the live database; restore a file from %s instead", file, filepath.Join(dataDir, "backups"))
		}
	}

	// 1. The pre-restore heads.
	res = &RestoreResult{From: filepath.Base(src)}
	pre := map[string]int64{}
	liveExists, err := Exists(dataDir)
	if err != nil {
		return nil, err
	}
	if liveExists {
		if pre, res.InstanceID, res.PreviousEpoch, err = recordCurrent(ctx, live); err != nil {
			return nil, err
		}
	}

	// 2. The prepared copy. Any failure from here on leaves the live database untouched.
	removeWithSidecars(tmp)
	defer func() {
		if err != nil {
			removeWithSidecars(tmp)
		}
	}()
	if err := copyFile(src, tmp); err != nil {
		return nil, fmt.Errorf("copy backup: %w", err)
	}
	t, err := openPath(dataDir, tmp)
	if err != nil {
		var tooNew *SchemaTooNewError
		if errors.As(err, &tooNew) {
			return nil, refuse("backup %s has schema v%d, newer than this reqharbor-server (max v%d); use a newer image", file, tooNew.DB, tooNew.Binary)
		}
		return nil, refuse("backup %s cannot be opened: %v", file, err)
	}
	closed := false
	defer func() {
		if !closed {
			t.Close()
		}
	}()
	if err := checkIntegrity(ctx, t.Writer, "integrity_check"); err != nil {
		return nil, refuse("backup %s is damaged: %v", file, err)
	}
	if res.BackupVersion, err = CurrentVersion(ctx, t.Writer); err != nil {
		return nil, refuse("backup %s is not a ReqHarbor database: %v", file, err)
	}
	var hasWorkspaces int
	if err := t.Writer.QueryRowContext(ctx, "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'workspaces'").Scan(&hasWorkspaces); err != nil {
		return nil, err
	}
	if res.BackupVersion < 1 || hasWorkspaces == 0 {
		return nil, refuse("backup %s is not a ReqHarbor database", file)
	}
	if _, err := t.Migrate(ctx, 0, now); err != nil {
		return nil, fmt.Errorf("migrate backup: %w", err)
	}
	if res.Version, err = CurrentVersion(ctx, t.Writer); err != nil {
		return nil, err
	}

	// 3 and 4 in one write transaction on the copy.
	if err := shiftAndRotate(ctx, t.Writer, pre, liveExists, opts.RevBase, now(), res); err != nil {
		return nil, err
	}
	if err := t.Close(); err != nil {
		return nil, err
	}
	closed = true
	removeSidecars(tmp)

	// 5. Swap. The WAL was checkpointed in step 1, but it moves along with the database anyway
	// (<pre-restore>-wal), so nothing written to it since can be lost; the -shm is rebuilt by
	// SQLite from the WAL and is dropped.
	if liveExists {
		res.PreRestorePath = preRestorePath(live, now())
	}
	if err := swapIn(live, tmp, res.PreRestorePath); err != nil {
		res.PreRestorePath = ""
		return nil, err
	}
	syncDir(dataDir)
	return res, nil
}

// rename is os.Rename; tests replace it to inject failures into swapIn.
var rename = os.Rename

// swapIn is step 5: when pre is not empty, the live database moves to pre and its -wal to
// pre-wal (the -shm is rebuilt from the WAL by SQLite, so it is dropped); then tmp moves to live.
// A failure puts the live database back; if even that fails, the error says where the current
// database now is, so the admin can move it back by hand.
func swapIn(live, tmp, pre string) error {
	putBack := func(cause error, withWAL bool) error {
		var errs []error
		if withWAL {
			if err := rename(pre+"-wal", live+"-wal"); err != nil && !errors.Is(err, os.ErrNotExist) {
				errs = append(errs, err)
			}
		}
		if err := rename(pre, live); err != nil {
			errs = append(errs, err)
		}
		if len(errs) > 0 {
			return fmt.Errorf("%w; moving the current database back also failed (%v): it is now %s (and its -wal %s-wal, if any); rename it to %s before starting the server",
				cause, errors.Join(errs...), pre, pre, live)
		}
		return cause
	}
	if pre != "" {
		if err := rename(live, pre); err != nil {
			return fmt.Errorf("keep the current database: %w", err)
		}
		if err := rename(live+"-wal", pre+"-wal"); err != nil && !errors.Is(err, os.ErrNotExist) {
			return putBack(fmt.Errorf("keep the current database's WAL: %w", err), false)
		}
		_ = os.Remove(live + "-shm")
		_ = os.Remove(live + "-journal")
	}
	if err := rename(tmp, live); err != nil {
		err = fmt.Errorf("move the restored database into place: %w", err)
		if pre != "" {
			return putBack(err, true)
		}
		return err
	}
	return nil
}

// recordCurrent is step 1 on the live database: it reads every workspace head, instance_id and
// sync_epoch, then folds the WAL into the file. It uses one plain connection and no downgrade
// guard, so an older binary can restore over a database that a newer one already migrated (the
// documented rollback to a pre-migrate backup); it only reads workspaces, entities and
// server_state. The checkpoint must complete: if another process (a backup, a CLI command, a
// replication sidecar such as Litestream) holds the database open, the restore is refused.
func recordCurrent(ctx context.Context, live string) (heads map[string]int64, instance, epoch string, err error) {
	u := url.URL{Scheme: "file", Path: live}
	conn, err := sql.Open("sqlite", u.String()+"?_pragma=busy_timeout(5000)")
	if err != nil {
		return nil, "", "", err
	}
	defer conn.Close()
	conn.SetMaxOpenConns(1)
	if heads, instance, epoch, err = readHeads(ctx, conn); err != nil {
		return nil, "", "", fmt.Errorf("read current database %s: %w", live, err)
	}
	if err := checkpointTruncate(ctx, conn); err != nil {
		if errors.Is(err, ErrCheckpointBusy) {
			return nil, "", "", refuse("another process has %s open (a backup, a reqharbor-server command, Litestream or another sidecar); stop it first, then run restore again (%v)", live, err)
		}
		return nil, "", "", fmt.Errorf("checkpoint current database %s: %w", live, err)
	}
	return heads, instance, epoch, nil
}

// readHeads returns each workspace's head (its rev, or a higher entity rev), the instance id and
// the sync epoch of a database.
func readHeads(ctx context.Context, q *sql.DB) (map[string]int64, string, string, error) {
	heads := map[string]int64{}
	var tables int
	if err := q.QueryRowContext(ctx, "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name IN ('workspaces', 'entities')").Scan(&tables); err != nil {
		return nil, "", "", err
	}
	if tables == 2 {
		var err error
		if heads, err = workspaceHeads(ctx, q); err != nil {
			return nil, "", "", err
		}
	}
	instance, err := serverState(ctx, q, stateInstanceID)
	if err != nil {
		return nil, "", "", err
	}
	epoch, err := serverState(ctx, q, stateSyncEpoch)
	return heads, instance, epoch, err
}

type querier interface {
	QueryContext(context.Context, string, ...any) (*sql.Rows, error)
	QueryRowContext(context.Context, string, ...any) *sql.Row
}

func workspaceHeads(ctx context.Context, q querier) (map[string]int64, error) {
	rows, err := q.QueryContext(ctx, `SELECT w.id, MAX(w.rev, COALESCE((SELECT MAX(e.rev) FROM entities e WHERE e.workspace_id = w.id), 0))
	  FROM workspaces w`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	heads := map[string]int64{}
	for rows.Next() {
		var id string
		var head int64
		if err := rows.Scan(&id, &head); err != nil {
			return nil, err
		}
		heads[id] = head
	}
	return heads, rows.Err()
}

// serverState reads one server_state value; "" when the table or the key is missing (a backup
// older than 0003).
func serverState(ctx context.Context, q querier, key string) (string, error) {
	var n int
	if err := q.QueryRowContext(ctx, "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'server_state'").Scan(&n); err != nil || n == 0 {
		return "", err
	}
	var v string
	err := q.QueryRowContext(ctx, "SELECT value FROM server_state WHERE key = ?", key).Scan(&v)
	if errors.Is(err, sql.ErrNoRows) {
		return "", nil
	}
	return v, err
}

func shiftAndRotate(ctx context.Context, w *sql.DB, pre map[string]int64, recorded bool, revBase int64, now time.Time, res *RestoreResult) (err error) {
	tx, err := w.BeginTx(ctx, nil) // BEGIN IMMEDIATE
	if err != nil {
		return err
	}
	defer func() {
		if err != nil {
			_ = tx.Rollback()
		}
	}()
	restored, err := workspaceHeads(ctx, tx)
	if err != nil {
		return err
	}
	var maxRestored int64
	for _, h := range restored {
		maxRestored = max(maxRestored, h)
	}
	wsIDs := make([]string, 0, len(restored))
	for id := range restored {
		wsIDs = append(wsIDs, id)
	}
	sort.Strings(wsIDs)
	bases := make(map[string]int64, len(wsIDs))
	for _, id := range wsIDs {
		head := restored[id]
		preHead, ok := pre[id]
		var base int64
		switch {
		case revBase > 0:
			if revBase <= head || (ok && revBase <= preHead) {
				return refuse("--rev-base %d is too low: workspace %s had rev %d; use a value above it", revBase, id, max(head, preHead))
			}
			base = revBase
		case recorded && ok:
			base = max(preHead, head) + 1
		default:
			// No recorded head (disk loss, or a workspace the current database does not have):
			// clients may hold any cursor up to the old head, so go far above it.
			base = maxRestored + DefaultRevOffset
			res.Unrecorded++
		}
		if base+head > maxSafeRev {
			return refuse("workspace %s: shifting rev %d by %d passes 2^53; restore with a smaller --rev-base", id, head, base)
		}
		bases[id] = base
	}
	for i, id := range wsIDs {
		base := bases[id]
		// Two passes keep the unique (workspace_id, rev) index free of transient collisions.
		if _, err := tx.ExecContext(ctx, "UPDATE entities SET rev = -rev WHERE workspace_id = ?", id); err != nil {
			return err
		}
		if _, err := tx.ExecContext(ctx, "UPDATE entities SET rev = -rev + ? WHERE workspace_id = ?", base, id); err != nil {
			return err
		}
		if _, err := tx.ExecContext(ctx, "UPDATE workspaces SET rev = rev + ? WHERE id = ?", base, id); err != nil {
			return err
		}
		if i == 0 || base < res.MinBase {
			res.MinBase = base
		}
		res.MaxBase = max(res.MaxBase, base)
	}
	res.Workspaces = len(wsIDs)

	backupInstance, err := serverState(ctx, tx, stateInstanceID)
	if err != nil {
		return err
	}
	// instance_id stays: it is still the same server, and a new one would pause every client
	// binding as a foreign server. Normally the backup carries the same id; a backup older than
	// 0003 has none, and then the current database's id is kept.
	switch {
	case res.InstanceID == "":
		res.InstanceID = backupInstance
	case backupInstance != "" && backupInstance != res.InstanceID:
		res.InstanceReplaced = true
	}
	res.NewEpoch = ids.ULID(now)
	set := func(key, value string) error {
		_, err := tx.ExecContext(ctx, "INSERT INTO server_state (key, value) VALUES (?, ?) ON CONFLICT (key) DO UPDATE SET value = excluded.value", key, value)
		return err
	}
	pairs := [][2]string{
		{stateSyncEpoch, res.NewEpoch},
		{stateRestoredAt, strconv.FormatInt(now.UnixMilli(), 10)},
		{stateRestoredFrom, res.From},
	}
	if res.InstanceID != "" {
		pairs = append(pairs, [2]string{stateInstanceID, res.InstanceID})
	}
	for _, p := range pairs {
		if err := set(p[0], p[1]); err != nil {
			return err
		}
	}
	return tx.Commit()
}

func preRestorePath(live string, now time.Time) string {
	base := live + preRestoreInfix + now.UTC().Format("20060102-150405")
	p := base
	for i := 2; ; i++ {
		if _, err := os.Lstat(p); errors.Is(err, os.ErrNotExist) {
			return p
		}
		p = base + "-" + strconv.Itoa(i)
	}
}

func copyFile(src, dst string) (err error) {
	in, err := os.Open(src)
	if err != nil {
		return err
	}
	defer in.Close()
	out, err := os.OpenFile(dst, os.O_WRONLY|os.O_CREATE|os.O_EXCL, 0o600)
	if err != nil {
		return err
	}
	defer func() {
		if cerr := out.Close(); err == nil {
			err = cerr
		}
	}()
	if _, err = io.Copy(out, in); err != nil {
		return err
	}
	return out.Sync()
}

func removeSidecars(path string) {
	for _, s := range []string{"-wal", "-shm", "-journal"} {
		_ = os.Remove(path + s)
	}
}

func removeWithSidecars(path string) {
	_ = os.Remove(path)
	removeSidecars(path)
}

// syncDir makes the renames durable where the platform supports it.
func syncDir(dir string) {
	if f, err := os.Open(dir); err == nil {
		_ = f.Sync()
		f.Close()
	}
}
