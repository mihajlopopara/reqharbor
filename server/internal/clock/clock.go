// Package clock abstracts time so every component can be tested with a fake clock.
package clock

import (
	"sync"
	"time"
)

// Clock returns the current time. All database timestamps are integer milliseconds since the
// Unix epoch (see Millis).
type Clock interface {
	Now() time.Time
}

// System is the real wall clock.
type System struct{}

// Now returns time.Now().
func (System) Now() time.Time { return time.Now() }

// Millis returns t as integer milliseconds since the Unix epoch.
func Millis(t time.Time) int64 { return t.UnixMilli() }

// Fake is a manually driven clock for tests. The zero value starts at the Unix epoch; use
// NewFake for a realistic start time.
type Fake struct {
	mu  sync.Mutex
	now time.Time
}

// NewFake returns a fake clock set to t.
func NewFake(t time.Time) *Fake { return &Fake{now: t} }

// Now returns the fake time.
func (f *Fake) Now() time.Time {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.now
}

// Set moves the clock to t.
func (f *Fake) Set(t time.Time) {
	f.mu.Lock()
	f.now = t
	f.mu.Unlock()
}

// Advance moves the clock forward by d.
func (f *Fake) Advance(d time.Duration) {
	f.mu.Lock()
	f.now = f.now.Add(d)
	f.mu.Unlock()
}
