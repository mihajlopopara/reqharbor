package jobs

import (
	"context"
	"log/slog"
	"time"

	"github.com/mihajlopopara/reqharbor/server/internal/clock"
)

// MaintenanceInterval is how often the daily maintenance runs (06-go-server.md §6).
const MaintenanceInterval = 24 * time.Hour

// MaintenanceFirstDelay is how long after ready a server that never ran the maintenance runs it.
const MaintenanceFirstDelay = time.Hour

// Maintainer runs the database maintenance and remembers when it last completed; *db.DB
// implements it (PRAGMA optimize and PRAGMA incremental_vacuum, the time in server_state), so
// a server restarted more often than daily still runs it once a day.
type Maintainer interface {
	Maintain(ctx context.Context) error
	LastMaintenance(ctx context.Context) (at time.Time, ok bool, err error)
	SetLastMaintenance(ctx context.Context, at time.Time) error
}

// NextMaintenance is how long to wait from now for the next run: firstDelay when it never ran,
// otherwise until interval after the last run (0 when that is past). A last run in the future
// (the clock moved back) waits at most one interval.
func NextMaintenance(now, last time.Time, ran bool, interval, firstDelay time.Duration) time.Duration {
	if !ran {
		return firstDelay
	}
	return min(max(last.Add(interval).Sub(now), 0), interval)
}

// RunMaintenance runs Maintain whenever it is due, until ctx ends: interval after the last
// completed run as recorded by the Maintainer, or firstDelay after start when there is none. A
// failed run is retried after firstDelay.
func RunMaintenance(ctx context.Context, m Maintainer, clk clock.Clock, interval, firstDelay time.Duration, logger *slog.Logger) {
	last, ran, err := m.LastMaintenance(ctx)
	if err != nil {
		if ctx.Err() != nil {
			return
		}
		logger.Warn("database maintenance: cannot read its last run; scheduling it as never run", "err", err.Error())
		ran = false
	}
	wait := NextMaintenance(clk.Now(), last, ran, interval, firstDelay)
	for {
		t := time.NewTimer(wait)
		select {
		case <-ctx.Done():
			t.Stop()
			return
		case <-t.C:
		}
		start := time.Now()
		if err := m.Maintain(ctx); err != nil {
			if ctx.Err() != nil {
				return
			}
			logger.Warn("database maintenance failed", "err", err.Error())
			wait = firstDelay
			continue
		}
		if err := m.SetLastMaintenance(ctx, clk.Now()); err != nil && ctx.Err() == nil {
			logger.Warn("database maintenance: cannot record its run", "err", err.Error())
		}
		logger.Debug("database maintenance", "dur_ms", time.Since(start).Milliseconds())
		wait = interval
	}
}
