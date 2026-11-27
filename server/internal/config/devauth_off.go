//go:build !devauth

package config

// DevAuthCompiled reports whether this binary contains the dev sign-in provider. Release
// binaries and the image are built without the devauth tag (06-go-server.md §5.4).
const DevAuthCompiled = false
