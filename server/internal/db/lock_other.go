//go:build !unix

package db

import (
	"errors"
	"os"
)

// Non-unix builds are for development only: no lock, no free-space check.

func lockFile(path string) (*os.File, error) {
	return os.OpenFile(path, os.O_RDWR|os.O_CREATE, 0o600)
}

func unlockFile(f *os.File) error { return f.Close() }

func freeBytes(string) (int64, error) {
	return 0, errors.New("free disk space is not available on this platform")
}
