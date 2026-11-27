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
	// InviteRetention keeps team invites this long after they expired or were revoked.
	InviteRetention = 30 * 24 * time.Hour
	// DepartureRetention keeps team departures (removal notices and rejoin blocks) this long.
	// Invites live at most 30 days, so an older removal can no longer be bypassed.
	DepartureRetention = 31 * 24 * time.Hour
)

// GC deletes expired auth requests, one-time tokens and sessions that expired or were revoked
// more than 7 days ago, login_throttle rows whose last failure is more than 24 h old, and (M2)
// team invites expired or revoked more than 30 days ago, team departures older than 31 days and
// expired signup tickets.
func GC(ctx context.Context, st store.Store, c clock.Clock) (store.GCStats, error) {
	now := c.Now()
	var stats store.GCStats
	err := st.Write(ctx, func(tx store.WriteTx) error {
		var err error
		if stats, err = tx.GC(clock.Millis(now), clock.Millis(now.Add(-DeadRetention)), clock.Millis(now.Add(-ThrottleRetention))); err != nil {
			return err
		}
		teams, err := tx.GCTeams(clock.Millis(now), clock.Millis(now.Add(-InviteRetention)), clock.Millis(now.Add(-DepartureRetention)))
		stats.Invites, stats.Departures, stats.Tickets = teams.Invites, teams.Departures, teams.Tickets
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
				logger.Debug("gc", "auth_requests", stats.AuthRequests, "tokens", stats.Tokens, "sessions", stats.Sessions, "throttle", stats.Throttle,
					"invites", stats.Invites, "departures", stats.Departures, "tickets", stats.Tickets)
			}
		}
	}
}
