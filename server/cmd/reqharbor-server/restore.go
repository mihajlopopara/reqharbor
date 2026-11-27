package main

import (
	"context"
	"errors"
	"flag"
	"fmt"
	"io"
	"time"

	"github.com/mihajlopopara/reqharbor/server/internal/db"
)

const restoreUsage = "usage: reqharbor-server restore FILE [--rev-base N]   (the server must be stopped)"

// restore replaces the database with a backup, shifting every rev above the previous head and
// rotating the sync epoch (06-go-server.md §6 Restore). The server must be stopped: restore takes
// the single-instance lock. With compose:
//
//	docker compose stop reqharbor
//	docker compose run --rm reqharbor restore /data/backups/<file>.db
//	docker compose start reqharbor
func restore(args, environ []string, stdout, stderr io.Writer) int {
	fs := flag.NewFlagSet("restore", flag.ContinueOnError)
	fs.SetOutput(stderr)
	revBase := fs.Int64("rev-base", 0, "base added to every restored rev, for a restore without the previous database (default: computed)")
	if err := fs.Parse(args); err != nil {
		return 2
	}
	// Accept the flag after FILE too (restore FILE --rev-base N).
	var file string
	if fs.NArg() > 0 {
		file = fs.Arg(0)
		if err := fs.Parse(fs.Args()[1:]); err != nil {
			return 2
		}
	}
	if file == "" || fs.NArg() != 0 {
		fmt.Fprintln(stderr, restoreUsage)
		return 2
	}
	dir := dataDir(environ)
	res, err := db.Restore(context.Background(), dir, file, db.RestoreOptions{RevBase: *revBase, Now: time.Now})
	if errors.Is(err, db.ErrLocked) {
		fmt.Fprintln(stderr, "reqharbor-server: the server is running on this data directory; stop it first (docker compose stop reqharbor), then run restore again")
		return 1
	}
	if err != nil {
		fmt.Fprintln(stderr, "reqharbor-server: restore failed:", err)
		return 1
	}
	migrated := ""
	if res.Version != res.BackupVersion {
		migrated = fmt.Sprintf(", migrated to v%d", res.Version)
	}
	fmt.Fprintf(stdout, "Restored %s (schema v%d%s).\n", res.From, res.BackupVersion, migrated)
	fmt.Fprintf(stdout, "Workspaces shifted: %d (every rev moved up by %d to %d, above the previous head).\n", res.Workspaces, res.MinBase, res.MaxBase)
	if res.Unrecorded > 0 {
		fmt.Fprintf(stdout, "%d of them had no previous head on record, so their revs were moved up by 2^32 above the backup's highest rev.\n", res.Unrecorded)
	}
	prev := ""
	if res.PreviousEpoch != "" {
		prev = " (was " + res.PreviousEpoch + ")"
	}
	fmt.Fprintf(stdout, "New sync epoch: %s%s. Instance id unchanged: %s.\n", res.NewEpoch, prev, res.InstanceID)
	if res.InstanceReplaced {
		fmt.Fprintln(stdout, "Note: the backup came with a different instance id; the current server's id was kept.")
	}
	if res.PreRestorePath != "" {
		fmt.Fprintf(stdout, "The previous database is kept as %s; delete it once you no longer need it.\n", res.PreRestorePath)
	}
	fmt.Fprint(stdout, `Next: start the server.
- ReqHarbor apps with sync-epoch support notice the new epoch on their next sync, download everything
  again and re-upload what they changed after this backup was taken.
- Older apps download everything again but do not re-upload their own later changes; have their
  users update the app before they sync.
- Devices that signed in or refreshed their sign-in after this backup was taken must sign in again.
  Workspaces and accounts created after it no longer exist.
- Revocations made after this backup are undone: signed-out sessions, disabled accounts, reset
  passwords and revoked invites are back as they were. Repeat those runs now (user disable,
  user sessions revoke, user set-password or user reset-link, invite revoke).
- Team changes made after this backup are undone too: removed members are members again, demoted
  members have their old roles back and revoked team invite links work again. Tell team admins to
  repeat those removals, demotions and revocations.
`)
	return 0
}
