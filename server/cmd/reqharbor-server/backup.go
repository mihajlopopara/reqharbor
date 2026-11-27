package main

import (
	"context"
	"flag"
	"fmt"
	"io"
	"path/filepath"
	"time"

	"github.com/mihajlopopara/reqharbor/server/internal/db"
)

// backup writes an online snapshot of the database (VACUUM INTO) while serve keeps running
// (06-go-server.md §6 Backups). In the image: docker compose exec reqharbor /reqharbor-server backup.
func backup(args, environ []string, stdout, stderr io.Writer) int {
	fs := flag.NewFlagSet("backup", flag.ContinueOnError)
	fs.SetOutput(stderr)
	out := fs.String("out", "", "backup file to write (default: <data dir>/backups/manual-YYYYMMDD-HHMMSS.db)")
	if err := fs.Parse(args); err != nil {
		return 2
	}
	if fs.NArg() != 0 {
		fmt.Fprintln(stderr, "usage: reqharbor-server backup [--out PATH]")
		return 2
	}
	dir := dataDir(environ)
	path := *out
	if path == "" {
		path = filepath.Join(dir, "backups", "manual-"+time.Now().UTC().Format("20060102-150405")+".db")
	}
	ctx := context.Background()
	if err := db.BackupFile(ctx, dir, path); err != nil {
		fmt.Fprintln(stderr, "reqharbor-server: backup failed:", err)
		return 1
	}
	info, err := db.VerifyBackup(ctx, path)
	if err != nil {
		fmt.Fprintln(stderr, "reqharbor-server: the backup was written but does not verify:", err)
		return 1
	}
	fmt.Fprintln(stdout, path)
	fmt.Fprintf(stdout, "Backup complete: %d bytes, schema v%d. Restore it only with `reqharbor-server restore %s` (never by copying the file).\n",
		info.Bytes, info.Version, path)
	return 0
}
