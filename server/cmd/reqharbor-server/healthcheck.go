package main

import (
	"flag"
	"fmt"
	"io"
	"net/http"
	"time"

	"github.com/mihajlopopara/reqharbor/server/internal/config"
)

func healthcheck(args, environ []string, stderr io.Writer) int {
	fs := flag.NewFlagSet("healthcheck", flag.ContinueOnError)
	fs.SetOutput(stderr)
	target := fs.String("url", "", "URL to check (default: /healthz on REQHARBOR_LISTEN)")
	if err := fs.Parse(args); err != nil {
		return 2
	}
	u := *target
	if u == "" {
		var err error
		if u, err = config.HealthcheckURL(envValue(environ, "REQHARBOR_LISTEN")); err != nil {
			fmt.Fprintln(stderr, "reqharbor-server:", err)
			return 1
		}
	}
	client := &http.Client{Timeout: 3 * time.Second}
	resp, err := client.Get(u)
	if err != nil {
		fmt.Fprintln(stderr, "unhealthy:", err)
		return 1
	}
	resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		fmt.Fprintln(stderr, "unhealthy: status", resp.StatusCode)
		return 1
	}
	return 0
}
