// Package app wires the HTTP surface (New) and runs the server lifecycle (Run).
package app

import (
	"context"
	"errors"
	"log/slog"
	"net/http"
	"sync/atomic"
	"time"

	"github.com/mihajlopopara/reqharbor/server/internal/accounts"
	"github.com/mihajlopopara/reqharbor/server/internal/auth"
	"github.com/mihajlopopara/reqharbor/server/internal/auth/throttle"
	"github.com/mihajlopopara/reqharbor/server/internal/clientip"
	"github.com/mihajlopopara/reqharbor/server/internal/clock"
	"github.com/mihajlopopara/reqharbor/server/internal/config"
	"github.com/mihajlopopara/reqharbor/server/internal/db"
	"github.com/mihajlopopara/reqharbor/server/internal/discovery"
	"github.com/mihajlopopara/reqharbor/server/internal/httpx"
	"github.com/mihajlopopara/reqharbor/server/internal/ids"
	"github.com/mihajlopopara/reqharbor/server/internal/jobs"
	"github.com/mihajlopopara/reqharbor/server/internal/legacy"
	"github.com/mihajlopopara/reqharbor/server/internal/ratelimit"
	"github.com/mihajlopopara/reqharbor/server/internal/shares"
	"github.com/mihajlopopara/reqharbor/server/internal/spaces"
	"github.com/mihajlopopara/reqharbor/server/internal/store"
)

// Migration states reported by /readyz.
const (
	MigrationsPending = "pending"
	MigrationsRunning = "running"
	MigrationsOK      = "ok"
	MigrationsFailed  = "failed"
)

// server_state keys.
const (
	StateInstanceID = "instance_id"
	StateSyncEpoch  = "sync_epoch"
)

// Deps are the inputs of New.
type Deps struct {
	Config  *config.Config
	Clock   clock.Clock
	Store   store.Store
	Logger  *slog.Logger
	Version string
	// DB enables the /readyz database and disk checks and the breaker's size checks; it may be
	// nil when Store is a stub.
	DB *db.DB
	// Breaker may be nil; New then builds one from Config and DB.
	Breaker *jobs.Breaker
	// HTTPClient makes the outbound sign-in provider calls; nil means a client with the
	// providers' default timeout. Tests pass one that trusts their fake providers.
	HTTPClient *http.Client
}

// App is the HTTP application.
type App struct {
	cfg     *config.Config
	clock   clock.Clock
	store   store.Store
	logger  *slog.Logger
	version string
	db      *db.DB

	Breaker       *jobs.Breaker
	Resolver      *clientip.Resolver
	CreateLimiter *ratelimit.Limiter
	ShareLimiter  *ratelimit.Limiter
	SyncLimiter   *ratelimit.Limiter // /v1/spaces/*/sync per session
	Auth          *auth.Service
	Hasher        *auth.Hasher
	Throttle      *throttle.Throttle

	ready      atomic.Bool
	setupCode  string
	migrations atomic.Value // string
	epoch      atomic.Pointer[string]
	instanceID string
	discovery  discovery.Handler
	handler    http.Handler
}

// New builds the application. Every route except /healthz and /readyz answers 503 starting
// until Init has succeeded.
func New(d Deps) *App {
	if d.Clock == nil {
		d.Clock = clock.System{}
	}
	if d.Logger == nil {
		d.Logger = slog.Default()
	}
	a := &App{cfg: d.Config, clock: d.Clock, store: d.Store, logger: d.Logger, version: d.Version, db: d.DB, Breaker: d.Breaker}
	a.migrations.Store(MigrationsPending)
	if a.Breaker == nil {
		var sizes jobs.Sizes
		if d.DB != nil {
			sizes = d.DB
		}
		a.Breaker = jobs.NewBreaker(jobs.BreakerLimits{
			MaxDBBytes:           d.Config.MaxDBBytes,
			MinFreeDiskBytes:     d.Config.MinFreeDiskBytes,
			LegacyTotalBytes:     d.Config.LegacyTotalBytes,
			OpenSignupTotalBytes: d.Config.OpenSignupTotalBytes,
		}, sizes, d.Store, d.Logger)
	}
	a.Resolver = clientip.New(d.Config.TrustedProxies, d.Config.ClientIPHeader, d.Logger, d.Config.LogClientIP)
	a.CreateLimiter = ratelimit.New(d.Clock, d.Config.RateLegacyCreatePerHour, time.Hour)
	a.ShareLimiter = ratelimit.New(d.Clock, d.Config.RateShareCreatePerHour, time.Hour)
	a.SyncLimiter = ratelimit.New(d.Clock, d.Config.RateSyncPerMin, time.Minute)

	shareSvc := &shares.Service{Store: d.Store, Clock: d.Clock, PublicURL: d.Config.PublicURL}
	a.Hasher = auth.NewHasher(d.Config.Argon2MemoryKiB)
	a.Throttle = throttle.New(nil, d.Clock) // the key is read from server_state in Init
	a.Auth = auth.New(auth.Deps{
		Store: d.Store, Clock: d.Clock, Config: d.Config, Logger: d.Logger, ClientIP: a.Resolver,
		Hasher: a.Hasher, Throttle: a.Throttle, Growth: a.accountGrowth, OpenSignupFull: a.Breaker.OpenSignupFull,
		UserCreated: a.discovery.SetupDone,
		HTTPClient:  d.HTTPClient,
	})
	a.discovery.UsersExist = a.usersExist
	mux := http.NewServeMux()
	mux.Handle("/healthz", httpx.Methods{{Name: "GET", Handler: http.HandlerFunc(a.healthz)}})
	mux.Handle("/readyz", httpx.Methods{{Name: "GET", Handler: http.HandlerFunc(a.readyz)}})
	mux.Handle("/v1/server", a.discovery.Routes())
	legacy.Register(mux, &legacy.Deps{
		Store:         d.Store,
		Clock:         d.Clock,
		Config:        d.Config,
		Breaker:       a.Breaker,
		ClientIP:      a.Resolver,
		CreateLimiter: a.CreateLimiter,
		ShareLimiter:  a.ShareLimiter,
		Shares:        shareSvc,
		Epoch:         a.SyncEpoch,
	})
	a.Auth.Register(mux)
	spaces.Register(mux, &spaces.Deps{
		Store:        d.Store,
		Clock:        d.Clock,
		Config:       d.Config,
		Auth:         a.Auth,
		Breaker:      a.Breaker,
		Shares:       shareSvc,
		SyncLimiter:  a.SyncLimiter,
		ShareLimiter: a.ShareLimiter,
		Epoch:        a.SyncEpoch,
	})
	mux.Handle("/s/{file}", shareSvc.PublicHandler())
	mux.Handle("/", httpx.NotFoundHandler)

	gated := http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if !a.ready.Load() && r.URL.Path != "/healthz" && r.URL.Path != "/readyz" {
			httpx.WriteError(w, httpx.NewError(503, "starting", "Server is starting; try again shortly").WithHeader("Retry-After", "5"))
			return
		}
		mux.ServeHTTP(w, r)
	})
	a.handler = httpx.Wrap(httpx.CapturePattern(gated), d.Logger, httpx.SecurityOptions{HSTS: d.Config.HSTS})
	return a
}

// ForceReady opens the routes without Init. Tests use it with a stub store.
func (a *App) ForceReady() {
	a.migrations.Store(MigrationsOK)
	a.ready.Store(true)
}

// Handler returns the root handler.
func (a *App) Handler() http.Handler { return a.handler }

// SetMigrations records the migration state shown by /readyz.
func (a *App) SetMigrations(state string) { a.migrations.Store(state) }

// Ready reports whether Init has completed.
func (a *App) Ready() bool { return a.ready.Load() }

// SyncEpoch is the current sync epoch (see restore, §6).
func (a *App) SyncEpoch() string {
	if p := a.epoch.Load(); p != nil {
		return *p
	}
	return ""
}

// InstanceID is the server's permanent instance id.
func (a *App) InstanceID() string { return a.instanceID }

// Sweepers returns the in-memory limiters and the throttle's account-wide state for the
// sweeper job.
func (a *App) Sweepers() []jobs.Sweeper {
	out := []jobs.Sweeper{a.CreateLimiter, a.ShareLimiter, a.SyncLimiter, a.Throttle}
	for _, l := range a.Auth.Limiters() {
		out = append(out, l)
	}
	return out
}

// SetupCode is the first-run setup code generated by Init ("" once an account exists).
func (a *App) SetupCode() string { return a.setupCode }

// accountGrowth is the breaker check for account creation (a growing write, §6).
func (a *App) accountGrowth() error {
	if a.Breaker != nil && a.Breaker.Open() {
		return legacy.ServerFull("This server is out of storage space; try again later")
	}
	return nil
}

func (a *App) usersExist(ctx context.Context) bool {
	var n int64
	err := a.store.Read(ctx, func(tx store.ReadTx) error {
		var err error
		n, err = tx.UserCount()
		return err
	})
	return err == nil && n > 0
}

// Init runs after the migrations (§6 Startup order, step 5): it creates the instance id, sync
// epoch and throttle key if absent and, while no user exists, a fresh setup code; then it samples
// the breaker, builds the discovery document and opens the routes.
func (a *App) Init(ctx context.Context) error {
	var instanceID, epoch, setupCode string
	var users int64
	var adminExists bool
	var throttleKey []byte
	err := a.store.Write(ctx, func(tx store.WriteTx) error {
		get := func(key string) (string, error) {
			v, err := tx.ServerState(key)
			if errors.Is(err, store.ErrNotFound) {
				v = ids.ULID(a.clock.Now())
				err = tx.SetServerState(key, v)
			}
			return v, err
		}
		var err error
		if instanceID, err = get(StateInstanceID); err != nil {
			return err
		}
		if epoch, err = get(StateSyncEpoch); err != nil {
			return err
		}
		if throttleKey, err = throttle.EnsureKey(tx); err != nil {
			return err
		}
		if users, err = tx.UserCount(); err != nil {
			return err
		}
		if users == 0 {
			// A new setup code at each start, invalidating the previous one (§5.6).
			setupCode, err = accounts.NewSetupCode(tx, a.cfg.SetupCode, auth.SetupCodeTTL, a.clock.Now())
			return err
		}
		adminExists, err = tx.AdminExists()
		return err
	})
	if err != nil {
		return err
	}
	a.instanceID = instanceID
	a.epoch.Store(&epoch)
	a.Throttle.SetKey(throttleKey)
	a.setupCode = setupCode
	if setupCode != "" {
		shown := setupCode
		if a.cfg.SetupCode != "" {
			shown = "from REQHARBOR_SETUP_CODE"
		}
		a.logger.Warn("First-run setup: in the ReqHarbor app, sign in to " + a.cfg.PublicURL + " and enter setup code " + shown)
	}
	if a.cfg.AdminEmail != "" && adminExists {
		a.logger.Info("REQHARBOR_ADMIN_EMAIL is ignored: an admin already exists")
	}
	if err := a.Breaker.Sample(ctx); err != nil {
		a.logger.Warn("initial breaker sample failed", "err", err.Error())
	}
	var methods []discovery.Method
	for _, m := range auth.Methods(a.cfg) {
		methods = append(methods, discovery.Method{ID: m.ID, Label: m.Label})
	}
	if err := a.discovery.Set(discovery.Build(a.cfg, a.version, instanceID, epoch, methods, users == 0)); err != nil {
		return err
	}
	a.migrations.Store(MigrationsOK)
	a.ready.Store(true)
	return nil
}

func (a *App) healthz(w http.ResponseWriter, _ *http.Request) {
	_ = httpx.WriteJSON(w, http.StatusOK, struct {
		Status string `json:"status"`
	}{"ok"}, nil)
}

type readyChecks struct {
	DB         string `json:"db"`
	Migrations string `json:"migrations"`
	Disk       string `json:"disk"`
}

func (a *App) readyz(w http.ResponseWriter, r *http.Request) {
	checks := readyChecks{DB: "ok", Migrations: a.migrations.Load().(string), Disk: "ok"}
	if a.db != nil {
		ctx, cancel := context.WithTimeout(r.Context(), 2*time.Second)
		if err := a.db.Ping(ctx); err != nil {
			checks.DB = "error"
		}
		cancel()
		if minFree := a.Breaker.Limits().MinFreeDiskBytes; minFree > 0 {
			if free, err := a.db.FreeDiskBytes(); err != nil || free < minFree {
				checks.Disk = "low"
			}
		}
	} else if a.store != nil {
		if err := a.store.Read(r.Context(), func(store.ReadTx) error { return nil }); err != nil {
			checks.DB = "error"
		}
	}
	status, code := "ok", http.StatusOK
	if checks.DB != "ok" || checks.Migrations != MigrationsOK || checks.Disk != "ok" || !a.ready.Load() {
		status, code = "unavailable", http.StatusServiceUnavailable
	}
	_ = httpx.WriteJSON(w, code, struct {
		Status string      `json:"status"`
		Checks readyChecks `json:"checks"`
	}{status, checks}, nil)
}
