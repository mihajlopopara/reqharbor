package jobs

import (
	"context"
	"log/slog"
	"time"

	"github.com/mihajlopopara/reqharbor/server/internal/clock"
	"github.com/mihajlopopara/reqharbor/server/internal/store"
)

// GC timings (06-go-server.md §6 Background jobs).
const (
	GCInterval = 10 * time.Minute
	// DeadRetention keeps expired or revoked one-time tokens and sessions this long.
	DeadRetention = 7 * 24 * time.Hour
	// ThrottleRetention deletes login_throttle rows this long after their last failure.
	ThrottleRetention = 24 * time.Hour
)

// GC deletes expired auth requests, one-time tokens and sessions that expired or were revoked
// more than 7 days ago, and login_throttle rows whose last failure is more than 24 h old.
func GC(ctx context.Context, st store.Store, c clock.Clock) (store.GCStats, error) {
	now := c.Now()
	var stats store.GCStats
	err := st.Write(ctx, func(tx store.WriteTx) error {
		var err error
		stats, err = tx.GC(clock.Millis(now), clock.Millis(now.Add(-DeadRetention)), clock.Millis(now.Add(-ThrottleRetention)))
		return err
	})
	return stats, err
}

// RunGC runs GC every interval until ctx ends.
func RunGC(ctx context.Context, st store.Store, c clock.Clock, interval time.Duration, logger *slog.Logger) {
	t := time.NewTicker(interval)
	defer t.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-t.C:
			stats, err := GC(ctx, st, c)
			if err != nil {
				if ctx.Err() == nil {
					logger.Warn("gc failed", "err", err.Error())
				}
				continue
			}
			if stats != (store.GCStats{}) {
				logger.Debug("gc", "auth_requests", stats.AuthRequests, "tokens", stats.Tokens, "sessions", stats.Sessions, "throttle", stats.Throttle)
			}
		}
	}
}
