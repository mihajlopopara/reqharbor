package app

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"net"
	"net/http"
	"path/filepath"
	"sync"
	"time"

	"github.com/mihajlopopara/reqharbor/server/internal/clock"
	"github.com/mihajlopopara/reqharbor/server/internal/config"
	"github.com/mihajlopopara/reqharbor/server/internal/db"
	"github.com/mihajlopopara/reqharbor/server/internal/jobs"
	"github.com/mihajlopopara/reqharbor/server/internal/store"
	"github.com/mihajlopopara/reqharbor/server/internal/store/sqlite"
)

// RunOptions customise Run (tests).
type RunOptions struct {
	// Listener replaces net.Listen on cfg.Listen.
	Listener net.Listener
	// Clock replaces the system clock.
	Clock clock.Clock
	// BeforeMigrate runs in the background goroutine just before the migrations (tests block
	// here to observe the starting state).
	BeforeMigrate func(ctx context.Context)
	// Started receives the App once the listener serves.
	Started func(*App)
	// WrapStore wraps the SQLite store (tests: a store that blocks a write in flight).
	WrapStore func(store.Store) store.Store
}

// writerIdleWait bounds how long shutdown waits, after REQHARBOR_SHUTDOWN_TIMEOUT, for a write
// transaction of a cut-off request to finish before the single-instance lock is released.
const writerIdleWait = 10 * time.Second

// Run is `serve`: the startup order of 06-go-server.md §6, then serving until ctx ends, then the
// graceful shutdown: http.Server.Shutdown within REQHARBOR_SHUTDOWN_TIMEOUT (in-flight requests,
// such as a push, finish; connections still open after it are closed), then the background jobs
// are cancelled and awaited, then PRAGMA wal_checkpoint(TRUNCATE), then the pools close and the
// single-instance lock is released.
func Run(ctx context.Context, cfg *config.Config, logger *slog.Logger, version string, opts RunOptions) error {
	clk := opts.Clock
	if clk == nil {
		clk = clock.System{}
	}
	// 2. Single-instance lock and pools (the downgrade guard runs in Open).
	d, err := db.Open(cfg.DataDir, db.Options{Lock: true})
	if err != nil {
		return err
	}
	var st store.Store = sqlite.New(d)
	if opts.WrapStore != nil {
		st = opts.WrapStore(st)
	}
	a := New(Deps{Config: cfg, Clock: clk, Store: st, Logger: logger, Version: version, DB: d})

	// 3. Bind and serve at once: /healthz answers, everything else is 503 starting.
	ln := opts.Listener
	if ln == nil {
		if ln, err = net.Listen("tcp", cfg.Listen); err != nil {
			d.Close()
			return fmt.Errorf("listen on %s: %w", cfg.Listen, err)
		}
	}
	srv := &http.Server{
		Handler:           a.Handler(),
		ReadHeaderTimeout: 10 * time.Second,
		ReadTimeout:       120 * time.Second,
		WriteTimeout:      120 * time.Second,
		IdleTimeout:       120 * time.Second,
		MaxHeaderBytes:    64 << 10,
		ErrorLog:          slog.NewLogLogger(logger.Handler(), slog.LevelWarn),
	}
	serveErr := make(chan error, 1)
	go func() { serveErr <- srv.Serve(ln) }()
	logger.Info("listening", "addr", ln.Addr().String(), "public_url", cfg.PublicURL, "version", version)
	if opts.Started != nil {
		opts.Started(a)
	}

	jobsCtx, stopJobs := context.WithCancel(context.Background())
	var wg sync.WaitGroup

	// OIDC discovery runs from the start (it needs no database) and retries in the background;
	// the provider is listed once it succeeds (§5.4).
	if a.Auth.HasDynamicProviders() {
		wg.Add(1)
		go func() { defer wg.Done(); a.RunProviders(jobsCtx) }()
	}

	// 4–6. In the background: pre-migration backup, migrations, server state, ready.
	wg.Add(1)
	go func() {
		defer wg.Done()
		if err := startup(jobsCtx, a, d, clk, logger, opts.BeforeMigrate); err != nil {
			if jobsCtx.Err() != nil {
				return // shutting down during startup
			}
			a.SetMigrations(MigrationsFailed)
			logger.Error("startup failed; the server stays unready (see /readyz)", "err", err.Error())
			return
		}
		var methods []string
		for _, m := range a.Auth.Methods() {
			methods = append(methods, m.ID)
		}
		logger.Info("ready", "signup", cfg.Signup, "legacy_workspaces", cfg.LegacyWorkspaces, "methods", methods, "instance_id", a.InstanceID(),
			"email", cfg.SMTPEnabled(), "backup_interval", cfg.BackupInterval.String())
		if a.Mail != nil {
			wg.Add(1)
			go func() { defer wg.Done(); a.Mail.Run(jobsCtx) }()
		}
		if cfg.BackupInterval > 0 {
			wg.Add(1)
			go func() {
				defer wg.Done()
				jobs.RunBackups(jobsCtx, d, clk, cfg.BackupInterval, int(cfg.BackupKeep), logger)
			}()
		}
		wg.Add(4)
		go func() { defer wg.Done(); a.Breaker.Run(jobsCtx, jobs.SampleInterval) }()
		go func() { defer wg.Done(); jobs.RunSweepers(jobsCtx, time.Minute, a.Sweepers()...) }()
		go func() { defer wg.Done(); jobs.RunGC(jobsCtx, st, clk, jobs.GCInterval, logger) }()
		go func() {
			defer wg.Done()
			jobs.RunMaintenance(jobsCtx, d, clk, jobs.MaintenanceInterval, jobs.MaintenanceFirstDelay, logger)
		}()
	}()

	var runErr error
	select {
	case <-ctx.Done():
	case err := <-serveErr:
		if !errors.Is(err, http.ErrServerClosed) {
			runErr = err
		}
	}

	// Graceful shutdown (§6), in this order.
	logger.Info("shutting down", "timeout", cfg.ShutdownTimeout.String())
	// 1. Stop accepting, let in-flight requests finish within the budget, then cut the rest.
	shCtx, cancel := context.WithTimeout(context.Background(), cfg.ShutdownTimeout)
	if err := srv.Shutdown(shCtx); err != nil {
		logger.Warn("graceful shutdown timed out; closing the remaining connections", "err", err.Error())
		_ = srv.Close()
		// Closing a connection does not stop its handler: one may still be inside a write
		// transaction. Give it a bounded time to finish before the lock is released, so a restore
		// or a new serve never starts while this process still writes.
		idleCtx, idleCancel := context.WithTimeout(context.Background(), writerIdleWait)
		if !d.WaitWriterIdle(idleCtx) {
			logger.Warn("a write transaction is still running after the shutdown budget; releasing the lock anyway", "waited", writerIdleWait.String())
		}
		idleCancel()
	}
	cancel()
	// 2. Cancel the background jobs and wait for them.
	stopJobs()
	wg.Wait()
	// 3. Fold the WAL into the database file, so a copy of /data taken now is complete.
	ckCtx, ckCancel := context.WithTimeout(context.Background(), 10*time.Second)
	if err := d.Checkpoint(ckCtx); err != nil {
		logger.Warn("WAL checkpoint failed", "err", err.Error())
	}
	ckCancel()
	// 4. Close the pools and release the lock.
	if err := d.Close(); err != nil && runErr == nil {
		runErr = err
	}
	logger.Info("stopped")
	return runErr
}

func startup(ctx context.Context, a *App, d *db.DB, clk clock.Clock, logger *slog.Logger, hook func(context.Context)) error {
	status, err := d.MigrationStatus(ctx)
	if err != nil {
		return err
	}
	if len(status.Pending) > 0 && status.Current > 0 {
		name := fmt.Sprintf("pre-migrate-v%d-v%d-%s.db", status.Current, status.Latest, clk.Now().UTC().Format("20060102-150405"))
		path := filepath.Join(d.BackupsDir(), name)
		logger.Info("backing up before migrating", "path", path)
		if err := d.BackupTo(ctx, path); err != nil {
			return fmt.Errorf("pre-migration backup: %w", err)
		}
	}
	if hook != nil {
		hook(ctx)
	}
	a.SetMigrations(MigrationsRunning)
	applied, err := d.Migrate(ctx, 0, clk.Now)
	for _, m := range applied {
		logger.Info("migration applied", "version", m.Version, "name", m.Name)
	}
	if err != nil {
		return err
	}
	return a.Init(ctx)
}
