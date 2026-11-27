// Package jobs runs the background goroutines: the breaker sampler and the limiter sweeper (S1),
// later GC and scheduled backups (06-go-server.md §6).
package jobs

import (
	"context"
	"log/slog"
	"sync"
	"sync/atomic"
	"time"

	"github.com/mihajlopopara/reqharbor/server/internal/store"
)

// SampleInterval is how often the breaker re-reads sizes.
const SampleInterval = 10 * time.Second

// Sizes is what the breaker samples; *db.DB implements it.
type Sizes interface {
	SizeBytes(ctx context.Context) (int64, error)
	FreeDiskBytes() (int64, error)
}

// BreakerLimits are the configured thresholds; 0 disables a check.
type BreakerLimits struct {
	MaxDBBytes       int64
	MinFreeDiskBytes int64
	LegacyTotalBytes int64
	// OpenSignupTotalBytes caps the personal spaces of open-signup accounts together.
	OpenSignupTotalBytes int64
}

// Breaker reports whether growing writes must be refused with 507 server_full. Its state comes
// from the last sample, so it is deterministic in tests (call Sample).
type Breaker struct {
	limitsMu sync.Mutex
	limits   BreakerLimits
	sizes    Sizes
	st       store.Store
	logger   *slog.Logger

	dbFull     atomic.Bool
	diskLow    atomic.Bool
	legacyFull atomic.Bool
	openFull   atomic.Bool
	dbBytes    atomic.Int64
	freeBytes  atomic.Int64
	legacy     atomic.Int64
	openBytes  atomic.Int64
}

// NewBreaker returns a breaker; sizes may be nil (no DB or disk checks).
func NewBreaker(limits BreakerLimits, sizes Sizes, st store.Store, logger *slog.Logger) *Breaker {
	return &Breaker{limits: limits, sizes: sizes, st: st, logger: logger}
}

// Limits returns the current thresholds.
func (b *Breaker) Limits() BreakerLimits {
	b.limitsMu.Lock()
	defer b.limitsMu.Unlock()
	return b.limits
}

// SetLimits replaces the thresholds; they apply from the next Sample.
func (b *Breaker) SetLimits(l BreakerLimits) {
	b.limitsMu.Lock()
	b.limits = l
	b.limitsMu.Unlock()
}

// Open reports whether the whole server refuses growing writes (DB size or free disk).
func (b *Breaker) Open() bool { return b.dbFull.Load() || b.diskLow.Load() }

// LegacyFull reports whether the legacy-workspace total-storage cap is reached.
func (b *Breaker) LegacyFull() bool { return b.legacyFull.Load() }

// OpenSignupFull reports whether REQHARBOR_OPEN_SIGNUP_TOTAL_BYTES is reached: growing writes to
// open-signup personal spaces return 507 and open signup shows server_user_limit.
func (b *Breaker) OpenSignupFull() bool { return b.openFull.Load() }

// Sample re-reads the sizes and updates the state.
func (b *Breaker) Sample(ctx context.Context) error {
	lim := b.Limits()
	var firstErr error
	keep := func(err error) {
		if err != nil && firstErr == nil {
			firstErr = err
		}
	}
	if b.sizes != nil {
		if n, err := b.sizes.SizeBytes(ctx); err == nil {
			b.dbBytes.Store(n)
			b.dbFull.Store(lim.MaxDBBytes > 0 && n >= lim.MaxDBBytes)
		} else {
			keep(err)
		}
		if lim.MinFreeDiskBytes > 0 {
			if n, err := b.sizes.FreeDiskBytes(); err == nil {
				b.freeBytes.Store(n)
				b.diskLow.Store(n < lim.MinFreeDiskBytes)
			} else {
				keep(err)
			}
		} else {
			b.diskLow.Store(false)
		}
	}
	if b.st != nil && lim.LegacyTotalBytes > 0 {
		var total int64
		err := b.st.Read(ctx, func(tx store.ReadTx) error {
			var err error
			total, err = tx.LegacyTotalBytes()
			return err
		})
		if err == nil {
			b.legacy.Store(total)
			b.legacyFull.Store(total >= lim.LegacyTotalBytes)
		} else {
			keep(err)
		}
	} else {
		b.legacyFull.Store(false)
	}
	if b.st != nil && lim.OpenSignupTotalBytes > 0 {
		var total int64
		err := b.st.Read(ctx, func(tx store.ReadTx) error {
			var err error
			total, err = tx.OpenSignupTotalBytes()
			return err
		})
		if err == nil {
			b.openBytes.Store(total)
			b.openFull.Store(total >= lim.OpenSignupTotalBytes)
		} else {
			keep(err)
		}
	} else {
		b.openFull.Store(false)
	}
	return firstErr
}

// Run samples every interval until ctx ends, logging state changes.
func (b *Breaker) Run(ctx context.Context, interval time.Duration) {
	t := time.NewTicker(interval)
	defer t.Stop()
	wasOpen, wasLegacy, wasOpenSignup := b.Open(), b.LegacyFull(), b.OpenSignupFull()
	for {
		select {
		case <-ctx.Done():
			return
		case <-t.C:
		}
		if err := b.Sample(ctx); err != nil && ctx.Err() == nil {
			b.logger.Error("breaker sample failed", "err", err.Error())
		}
		if open := b.Open(); open != wasOpen {
			if open {
				b.logger.Warn("storage breaker open: growing writes now return 507 server_full",
					"db_bytes", b.dbBytes.Load(), "free_disk_bytes", b.freeBytes.Load())
			} else {
				b.logger.Info("storage breaker closed")
			}
			wasOpen = open
		}
		if lf := b.LegacyFull(); lf != wasLegacy {
			if lf {
				b.logger.Warn("REQHARBOR_LEGACY_TOTAL_BYTES reached: legacy creates and growing legacy writes return 507", "legacy_bytes", b.legacy.Load())
			}
			wasLegacy = lf
		}
		if of := b.OpenSignupFull(); of != wasOpenSignup {
			if of {
				b.logger.Warn("REQHARBOR_OPEN_SIGNUP_TOTAL_BYTES reached: open signup is paused and growing writes of open-signup accounts return 507", "open_signup_bytes", b.openBytes.Load())
			}
			wasOpenSignup = of
		}
	}
}

// Sweeper is anything with a Sweep method (rate limiters).
type Sweeper interface{ Sweep() }

// RunSweepers calls Sweep on every sweeper each interval until ctx ends.
func RunSweepers(ctx context.Context, interval time.Duration, sweepers ...Sweeper) {
	t := time.NewTicker(interval)
	defer t.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-t.C:
			for _, s := range sweepers {
				s.Sweep()
			}
		}
	}
}
