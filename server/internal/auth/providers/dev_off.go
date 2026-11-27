//go:build !devauth

package providers

import "github.com/mihajlopopara/reqharbor/server/internal/config"

// DevCompiled reports whether this binary contains the dev provider. The image and release
// binaries are built without the devauth tag, so the provider's code (dev.go) is not in them.
const DevCompiled = false

// DevFormPath is unused without devauth; the route is never registered.
const DevFormPath = ""

func newDev(*config.Config) Provider { return nil }
