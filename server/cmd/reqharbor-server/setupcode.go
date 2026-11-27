package main

import (
	"flag"
	"fmt"
	"io"

	"github.com/mihajlopopara/reqharbor/server/internal/accounts"
	"github.com/mihajlopopara/reqharbor/server/internal/auth"
	"github.com/mihajlopopara/reqharbor/server/internal/store"
)

// setupCode prints a fresh first-run setup code, replacing the one `serve` logged. It works only
// while no account exists (06-go-server.md §5.6).
func setupCode(args, environ []string, stdout, stderr io.Writer) int {
	fs := flag.NewFlagSet("setup-code", flag.ContinueOnError)
	fs.SetOutput(stderr)
	if err := fs.Parse(args); err != nil {
		return 2
	}
	e, code := openCLI(environ, stderr)
	if e == nil {
		return code
	}
	defer e.close()
	var c string
	err := e.write(func(tx store.WriteTx) error {
		var err error
		c, err = accounts.NewSetupCode(tx, "", auth.SetupCodeTTL, e.now())
		return err
	})
	if err != nil {
		return fail(stderr, err)
	}
	fmt.Fprintf(stdout, "%s\nIn the ReqHarbor app, sign in to %s and enter this setup code. It works once, within %s, and replaces any earlier code.\n",
		c, e.cfg.PublicURL, auth.SetupCodeTTL)
	return 0
}
