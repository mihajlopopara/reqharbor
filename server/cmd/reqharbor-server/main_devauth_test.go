//go:build devauth

package main

import (
	"strings"
	"testing"
)

// A devauth build refuses to start on a listener that is not a loopback IP literal
// (06-go-server.md §5.4 guard (3)), whether or not REQHARBOR_DEV_AUTH is set.
func TestDevauthServeRefusesANonLoopbackListener(t *testing.T) {
	for _, listen := range []string{":8080", "0.0.0.0:8080", "[::]:8080", "192.168.1.5:8080", "localhost:8080"} {
		for _, dev := range []string{"true", "false"} {
			env := []string{"REQHARBOR_DATA_DIR=" + t.TempDir(), "REQHARBOR_PUBLIC_URL=https://rh.example.com",
				"REQHARBOR_LISTEN=" + listen, "REQHARBOR_DEV_AUTH=" + dev}
			code, _, errs := runCLI(env, "serve")
			if code != 2 || !strings.Contains(errs, "REQHARBOR_LISTEN must be a loopback address") {
				t.Fatalf("%s dev=%s: %d %q", listen, dev, code, errs)
			}
		}
	}
}
