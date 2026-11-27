//go:build devauth

package config

// DevAuthCompiled reports whether this binary contains the dev sign-in provider. This file is
// compiled only with -tags devauth, which the image and release builds never pass.
const DevAuthCompiled = true
