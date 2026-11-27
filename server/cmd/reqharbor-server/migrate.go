package main

import (
	"context"
	"flag"
	"fmt"
	"io"
	"strings"
	"time"

	"github.com/mihajlopopara/reqharbor/server/internal/db"
)

// dataDir reads REQHARBOR_DATA_DIR without validating the rest of the configuration, so the
// maintenance commands work without REQHARBOR_PUBLIC_URL.
func dataDir(environ []string) string {
	for _, kv := range environ {
		if v, ok := strings.CutPrefix(kv, "REQHARBOR_DATA_DIR="); ok && strings.TrimSpace(v) != "" {
			return strings.TrimSpace(v)
		}
	}
	return "./data"
}

func envValue(environ []string, name string) string {
	for _, kv := range environ {
		if v, ok := strings.CutPrefix(kv, name+"="); ok {
			return strings.TrimSpace(v)
		}
	}
	return ""
}

func migrate(args, environ []string, stdout, stderr io.Writer) int {
	fs := flag.NewFlagSet("migrate", flag.ContinueOnError)
	fs.SetOutput(stderr)
	status := fs.Bool("status", false, "print the applied and pending migrations")
	to := fs.Int("to", 0, "apply migrations up to this version (default: all)")
	if err := fs.Parse(args); err != nil {
		return 2
	}
	d, err := db.Open(dataDir(environ), db.Options{})
	if err != nil {
		fmt.Fprintln(stderr, "reqharbor-server:", err)
		return 1
	}
	defer d.Close()
	ctx := context.Background()
	st, err := d.MigrationStatus(ctx)
	if err != nil {
		fmt.Fprintln(stderr, "reqharbor-server:", err)
		return 1
	}
	if *status {
		fmt.Fprintf(stdout, "schema version %d (latest %d)\n", st.Current, st.Latest)
		for _, m := range st.Pending {
			fmt.Fprintf(stdout, "pending %04d_%s\n", m.Version, m.Name)
		}
		return 0
	}
	applied, err := d.Migrate(ctx, *to, time.Now)
	for _, m := range applied {
		fmt.Fprintf(stdout, "applied %04d_%s\n", m.Version, m.Name)
	}
	if err != nil {
		fmt.Fprintln(stderr, "reqharbor-server:", err)
		return 1
	}
	if len(applied) == 0 {
		fmt.Fprintf(stdout, "schema version %d: nothing to apply\n", st.Current)
	}
	return 0
}
