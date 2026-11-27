//go:build unix

package main

import "syscall"

// privateUmask makes every file the process creates (database, -wal, -shm, lock, backups)
// readable by its owner only: the database holds password and token hashes.
func privateUmask() { syscall.Umask(0o077) }
