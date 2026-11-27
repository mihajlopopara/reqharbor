// Package discovery serves GET /v1/server (06-go-server.md §5.1).
package discovery

import (
	"context"
	"net/http"
	"sync"
	"sync/atomic"

	"github.com/mihajlopopara/reqharbor/server/internal/config"
	"github.com/mihajlopopara/reqharbor/server/internal/httpx"
	"github.com/mihajlopopara/reqharbor/server/internal/syncsvc"
)

// Software is the discovery `software` value clients look for.
const Software = "reqharbor-server"

// APIVersion increases only for a breaking change to /v1/*.
const APIVersion = 1

// Method is a sign-in method.
type Method struct {
	ID    string `json:"id"`
	Label string `json:"label"`
}

// Info is the discovery document.
type Info struct {
	Software         string   `json:"software"`
	Version          string   `json:"version"`
	APIVersion       int      `json:"apiVersion"`
	InstanceID       string   `json:"instanceId"`
	SyncEpoch        string   `json:"syncEpoch"`
	Name             string   `json:"name"`
	PublicURL        string   `json:"publicUrl"`
	MinClientVersion *string  `json:"minClientVersion"`
	Auth             Auth     `json:"auth"`
	Features         Features `json:"features"`
	Sync             Sync     `json:"sync"`
	Limits           Limits   `json:"limits"`
}

// Auth describes sign-in.
type Auth struct {
	Methods       []Method `json:"methods"`
	Signup        string   `json:"signup"`
	PasswordReset string   `json:"passwordReset"`
	// EmailVerification is true when password signups are confirmed by an emailed code (SMTP
	// configured, S9). Additive: clients that do not know it ignore it.
	EmailVerification bool `json:"emailVerification"`
	SetupRequired     bool `json:"setupRequired"`
}

// Features are the optional server features.
type Features struct {
	Accounts         bool   `json:"accounts"`
	Teams            bool   `json:"teams"`
	LegacyWorkspaces string `json:"legacyWorkspaces"`
}

// Sync lists what the server stores and previews.
type Sync struct {
	Types        []string `json:"types"`
	RequestKinds []string `json:"requestKinds"`
}

// Limits are the protocol limits.
type Limits struct {
	EntityBytes   int `json:"entityBytes"`
	PushChanges   int `json:"pushChanges"`
	RequestBytes  int `json:"requestBytes"`
	SnapshotBytes int `json:"snapshotBytes"`
	JSONDepth     int `json:"jsonDepth"`
}

// Build returns the discovery document. Since S4 accounts is true and methods lists the enabled
// sign-in methods (server-v0.1.0 had accounts:false and no methods, which clients treat as
// "token sync only"). setupRequired is true while no user exists.
func Build(cfg *config.Config, version, instanceID, syncEpoch string, methods []Method, setupRequired bool) Info {
	if methods == nil {
		methods = []Method{}
	}
	var minClient *string
	if cfg.MinClientVersion != "" {
		v := cfg.MinClientVersion
		minClient = &v
	}
	reset := "admin"
	if cfg.SMTPEnabled() {
		reset = "email"
	}
	return Info{
		Software:         Software,
		Version:          version,
		APIVersion:       APIVersion,
		InstanceID:       instanceID,
		SyncEpoch:        syncEpoch,
		Name:             cfg.ServerName,
		PublicURL:        cfg.PublicURL,
		MinClientVersion: minClient,
		Auth: Auth{
			Methods:           methods,
			Signup:            cfg.Signup,
			PasswordReset:     reset, // "email" with SMTP (S9): /auth/forgot sends the link
			EmailVerification: cfg.SMTPEnabled() && cfg.PasswordAuth,
			SetupRequired:     setupRequired,
		},
		Features: Features{Accounts: true, Teams: cfg.Teams, LegacyWorkspaces: cfg.LegacyWorkspaces},
		Sync:     Sync{Types: append([]string(nil), syncsvc.Types...), RequestKinds: []string{"http"}},
		Limits: Limits{
			EntityBytes:   syncsvc.EntityDataBytes,
			PushChanges:   syncsvc.ChangesPerPush,
			RequestBytes:  httpx.MaxBodyBytes,
			SnapshotBytes: 1024 * 1024,
			JSONDepth:     100,
		},
	}
}

// Handler serves the document built at startup; Set rebuilds it. While setupRequired is true,
// each request asks UsersExist (an account may have been created by the pages or by the CLI in
// another process) and rebuilds the document once one exists; it never goes back to true.
type Handler struct {
	body atomic.Pointer[[]byte]

	mu         sync.Mutex
	info       Info
	set        bool
	setup      atomic.Bool
	UsersExist func(ctx context.Context) bool
	// Methods, when set, supplies auth.methods each time the document is built, so a provider
	// that becomes available later (OIDC after discovery) is listed; MethodsChanged rebuilds.
	Methods func() []Method
}

// Set replaces the served document.
func (h *Handler) Set(info Info) error {
	h.mu.Lock()
	defer h.mu.Unlock()
	return h.store(info)
}

// store builds and serves info; h.mu is held.
func (h *Handler) store(info Info) error {
	if h.Methods != nil {
		info.Auth.Methods = h.Methods()
		if info.Auth.Methods == nil {
			info.Auth.Methods = []Method{}
		}
	}
	b, err := httpx.Marshal(info)
	if err != nil {
		return err
	}
	h.info, h.set = info, true
	h.setup.Store(info.Auth.SetupRequired)
	h.body.Store(&b)
	return nil
}

// MethodsChanged rebuilds the document with the current methods (a no-op before the first Set,
// which reads them itself).
func (h *Handler) MethodsChanged() {
	h.mu.Lock()
	defer h.mu.Unlock()
	if h.set {
		_ = h.store(h.info)
	}
}

// SetupDone clears setupRequired (the first account exists).
func (h *Handler) SetupDone() {
	if !h.setup.Load() {
		return
	}
	h.mu.Lock()
	defer h.mu.Unlock()
	info := h.info
	info.Auth.SetupRequired = false
	_ = h.store(info)
}

// Current returns the served document.
func (h *Handler) Current() Info {
	h.mu.Lock()
	defer h.mu.Unlock()
	return h.info
}

// Routes returns the GET-only route handler.
func (h *Handler) Routes() http.Handler {
	return httpx.Methods{{Name: "GET", Handler: httpx.Func(func(w http.ResponseWriter, r *http.Request) error {
		if h.setup.Load() && h.UsersExist != nil && h.UsersExist(r.Context()) {
			h.SetupDone()
		}
		b := h.body.Load()
		if b == nil {
			return httpx.NewError(503, "starting", "Server is starting; try again shortly").WithHeader("Retry-After", "5")
		}
		w.Header().Set("Content-Type", httpx.ContentTypeJSON)
		w.Header().Set("Cache-Control", "no-store")
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write(*b)
		return nil
	})}}
}
