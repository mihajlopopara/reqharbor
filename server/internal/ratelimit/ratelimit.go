// Package ratelimit is an in-process GCRA limiter in a sharded map (06-go-server.md §5.9).
// Windows live in memory and reset on restart (W5); the server is single-instance.
package ratelimit

import (
	"crypto/sha256"
	"hash/maphash"
	"sync"
	"time"

	"github.com/mihajlopopara/reqharbor/server/internal/clock"
)

const shards = 32

// Limiter allows `limit` events per `per` with a burst of `limit`.
type Limiter struct {
	clock    clock.Clock
	interval time.Duration // emission interval T = per / limit
	burst    time.Duration // tolerance τ = T * (limit - 1)
	seed     maphash.Seed
	shards   [shards]shard
}

type shard struct {
	mu  sync.Mutex
	tat map[[32]byte]time.Time // theoretical arrival time per key
}

// New returns a limiter for limit events per period.
func New(c clock.Clock, limit int64, per time.Duration) *Limiter {
	if limit < 1 {
		limit = 1
	}
	l := &Limiter{
		clock:    c,
		interval: per / time.Duration(limit),
		seed:     maphash.MakeSeed(),
	}
	l.burst = l.interval * time.Duration(limit-1)
	for i := range l.shards {
		l.shards[i].tat = make(map[[32]byte]time.Time)
	}
	return l
}

// Allow records one event for key and reports whether it is allowed. When it is not, retry is
// how long until the next event would be. Keys are stored only as SHA-256 digests.
func (l *Limiter) Allow(key string) (ok bool, retry time.Duration) {
	k := sha256.Sum256([]byte(key))
	s := &l.shards[maphash.Bytes(l.seed, k[:])%shards]
	now := l.clock.Now()
	s.mu.Lock()
	defer s.mu.Unlock()
	tat, found := s.tat[k]
	if !found || tat.Before(now) {
		tat = now
	}
	allowAt := tat.Add(-l.burst)
	if now.Before(allowAt) {
		return false, allowAt.Sub(now)
	}
	s.tat[k] = tat.Add(l.interval)
	return true, 0
}

// Check reports whether one more event for key would be allowed, without recording it. Callers
// that count only failures (failed token exchanges) Check first and Allow on each failure.
func (l *Limiter) Check(key string) (ok bool, retry time.Duration) {
	k := sha256.Sum256([]byte(key))
	s := &l.shards[maphash.Bytes(l.seed, k[:])%shards]
	now := l.clock.Now()
	s.mu.Lock()
	defer s.mu.Unlock()
	tat, found := s.tat[k]
	if !found || tat.Before(now) {
		return true, 0
	}
	if allowAt := tat.Add(-l.burst); now.Before(allowAt) {
		return false, allowAt.Sub(now)
	}
	return true, 0
}

// Sweep drops keys whose window has fully drained.
func (l *Limiter) Sweep() {
	now := l.clock.Now()
	for i := range l.shards {
		s := &l.shards[i]
		s.mu.Lock()
		for k, tat := range s.tat {
			if !tat.After(now) {
				delete(s.tat, k)
			}
		}
		s.mu.Unlock()
	}
}

// Len returns the number of tracked keys (tests).
func (l *Limiter) Len() int {
	n := 0
	for i := range l.shards {
		l.shards[i].mu.Lock()
		n += len(l.shards[i].tat)
		l.shards[i].mu.Unlock()
	}
	return n
}
