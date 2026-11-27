// Command reqharbor-server is the self-hostable ReqHarbor sync and share server.
package main

import (
	"fmt"
	"io"
	"os"

	"github.com/mihajlopopara/reqharbor/server/internal/app"
)

// Set at link time: -ldflags "-X main.version=… -X main.commit=…".
var (
	version = "dev"
	commit  = "unknown"
)

const usage = `usage: reqharbor-server <command> [flags]

commands:
  serve                     run the server (default)
  migrate [--status] [--to N]
  healthcheck [--url URL]   exit 0 when /healthz answers 200 (Docker HEALTHCHECK)
  version
`

func main() {
	os.Exit(run(os.Args[1:], os.Environ(), os.Stdout, os.Stderr))
}

// run dispatches a subcommand and returns the exit code.
func run(args, environ []string, stdout, stderr io.Writer) int {
	cmd := "serve"
	if len(args) > 0 {
		cmd, args = args[0], args[1:]
	}
	switch cmd {
	case "serve":
		return serve(args, environ, stdout, stderr)
	case "migrate":
		return migrate(args, environ, stdout, stderr)
	case "healthcheck":
		return healthcheck(args, environ, stderr)
	case "version", "--version", "-v":
		fmt.Fprintf(stdout, "reqharbor-server %s (commit %s)\n", app.ResolveVersion(version), commit)
		return 0
	case "help", "--help", "-h":
		fmt.Fprint(stdout, usage)
		return 0
	default:
		fmt.Fprintf(stderr, "unknown command %q\n\n%s", cmd, usage)
		return 2
	}
}
