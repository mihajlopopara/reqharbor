// Package discovery serves GET /v1/server (06-go-server.md §5.1).
package discovery

import (
	"net/http"
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
	SetupRequired bool     `json:"setupRequired"`
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

// Build returns the discovery document. In S1 (server-v0.1.0) there are no accounts: accounts is
// false and the method list is empty, which clients treat as "token sync only".
func Build(cfg *config.Config, version, instanceID, syncEpoch string) Info {
	var minClient *string
	if cfg.MinClientVersion != "" {
		v := cfg.MinClientVersion
		minClient = &v
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
			Methods:       []Method{},
			Signup:        cfg.Signup,
			PasswordReset: "admin",
			SetupRequired: false,
		},
		Features: Features{Accounts: false, Teams: false, LegacyWorkspaces: cfg.LegacyWorkspaces},
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

// Handler serves the document built at startup; Set rebuilds it.
type Handler struct{ body atomic.Pointer[[]byte] }

// Set replaces the served document.
func (h *Handler) Set(info Info) error {
	b, err := httpx.Marshal(info)
	if err != nil {
		return err
	}
	h.body.Store(&b)
	return nil
}

// Routes returns the GET-only route handler.
func (h *Handler) Routes() http.Handler {
	return httpx.Methods{{Name: "GET", Handler: httpx.Func(func(w http.ResponseWriter, r *http.Request) error {
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
