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

	"github.com/mihajlopopara/reqharbor/server/internal/auth"
	"github.com/mihajlopopara/reqharbor/server/internal/clock"
	"github.com/mihajlopopara/reqharbor/server/internal/config"
	"github.com/mihajlopopara/reqharbor/server/internal/db"
	"github.com/mihajlopopara/reqharbor/server/internal/jobs"
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
}

// Run is `serve`: the startup order of 06-go-server.md §6, then serving until ctx ends, then a
// graceful shutdown.
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
	defer d.Close()
	st := sqlite.New(d)
	a := New(Deps{Config: cfg, Clock: clk, Store: st, Logger: logger, Version: version, DB: d})

	// 3. Bind and serve at once: /healthz answers, everything else is 503 starting.
	ln := opts.Listener
	if ln == nil {
		if ln, err = net.Listen("tcp", cfg.Listen); err != nil {
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
	defer func() {
		stopJobs()
		wg.Wait()
	}()

	// 4–6. In the background: pre-migration backup, migrations, server state, ready.
	wg.Add(1)
	go func() {
		defer wg.Done()
		if err := startup(jobsCtx, a, d, clk, logger, opts.BeforeMigrate); err != nil {
			a.SetMigrations(MigrationsFailed)
			logger.Error("startup failed; the server stays unready (see /readyz)", "err", err.Error())
			return
		}
		var methods []string
		for _, m := range auth.Methods(cfg) {
			methods = append(methods, m.ID)
		}
		logger.Info("ready", "signup", cfg.Signup, "legacy_workspaces", cfg.LegacyWorkspaces, "methods", methods, "instance_id", a.InstanceID())
		wg.Add(3)
		go func() { defer wg.Done(); a.Breaker.Run(jobsCtx, jobs.SampleInterval) }()
		go func() { defer wg.Done(); jobs.RunSweepers(jobsCtx, time.Minute, a.Sweepers()...) }()
		go func() { defer wg.Done(); jobs.RunGC(jobsCtx, st, clk, jobs.GCInterval, logger) }()
	}()

	select {
	case <-ctx.Done():
	case err := <-serveErr:
		if !errors.Is(err, http.ErrServerClosed) {
			return err
		}
	}
	logger.Info("shutting down")
	shCtx, cancel := context.WithTimeout(context.Background(), cfg.ShutdownTimeout)
	defer cancel()
	if err := srv.Shutdown(shCtx); err != nil {
		logger.Warn("graceful shutdown incomplete", "err", err.Error())
	}
	return nil
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
