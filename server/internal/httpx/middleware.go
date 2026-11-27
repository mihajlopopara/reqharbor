package httpx

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"errors"
	"log/slog"
	"net/http"
	"runtime/debug"
	"strings"
	"time"
)

type ctxKey int

const (
	keyLogger ctxKey = iota
	keyRequestID
	keyRoute
)

// LoggerFrom returns the request logger, or slog.Default.
func LoggerFrom(ctx context.Context) *slog.Logger {
	if l, ok := ctx.Value(keyLogger).(*slog.Logger); ok {
		return l
	}
	return slog.Default()
}

// RequestIDFrom returns the request id set by the middleware.
func RequestIDFrom(ctx context.Context) string {
	s, _ := ctx.Value(keyRequestID).(string)
	return s
}

type routeHolder struct{ pattern string }

// SecurityOptions configures the security-header middleware.
type SecurityOptions struct {
	// HSTS is the Strict-Transport-Security value; empty means none is sent.
	HSTS string
}

// API defaults for responses whose handler did not set them.
const (
	APICSP          = "default-src 'none'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'"
	permissionsPol  = "camera=(), microphone=(), geolocation=(), interest-cohort=()"
	defaultNoStore  = "no-store"
	authPathPrefix  = "/auth/"
	defaultCORPSame = "same-origin"
)

// headerWriter fills in the security defaults just before the header is written.
type headerWriter struct {
	http.ResponseWriter
	opts        SecurityOptions
	allowCookie bool
	wrote       bool
	status      int
	bytes       int64
}

func (h *headerWriter) applyDefaults() {
	if h.wrote {
		return
	}
	h.wrote = true
	hd := h.ResponseWriter.Header()
	setDefault := func(k, v string) {
		if hd.Get(k) == "" {
			hd.Set(k, v)
		}
	}
	hd.Set("X-Content-Type-Options", "nosniff")
	hd.Set("Referrer-Policy", "no-referrer")
	hd.Set("X-Frame-Options", "DENY")
	hd.Set("Cross-Origin-Opener-Policy", "same-origin")
	setDefault("Cross-Origin-Resource-Policy", defaultCORPSame)
	hd.Set("Permissions-Policy", permissionsPol)
	if h.opts.HSTS != "" {
		hd.Set("Strict-Transport-Security", h.opts.HSTS)
	} else {
		hd.Del("Strict-Transport-Security")
	}
	setDefault("Content-Security-Policy", APICSP)
	setDefault("Cache-Control", defaultNoStore)
	if !h.allowCookie {
		hd.Del("Set-Cookie")
	}
}

func (h *headerWriter) WriteHeader(code int) {
	if h.wrote {
		return
	}
	h.applyDefaults()
	h.status = code
	h.ResponseWriter.WriteHeader(code)
}

func (h *headerWriter) Write(b []byte) (int, error) {
	if !h.wrote {
		h.WriteHeader(http.StatusOK)
	}
	n, err := h.ResponseWriter.Write(b)
	h.bytes += int64(n)
	return n, err
}

// Unwrap lets http.ResponseController reach the underlying writer.
func (h *headerWriter) Unwrap() http.ResponseWriter { return h.ResponseWriter }

// Flush implements http.Flusher.
func (h *headerWriter) Flush() {
	if !h.wrote {
		h.WriteHeader(http.StatusOK)
	}
	if f, ok := h.ResponseWriter.(http.Flusher); ok {
		f.Flush()
	}
}

// Wrap installs the outer middleware stack around the router: request id, access log, security
// headers and panic recovery. The router is called with a route holder in the context so the
// access log can report the matched pattern (never the raw path).
func Wrap(router http.Handler, logger *slog.Logger, opts SecurityOptions) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		start := time.Now()
		id := newRequestID()
		hw := &headerWriter{ResponseWriter: w, opts: opts, allowCookie: strings.HasPrefix(r.URL.Path, authPathPrefix)}
		hw.Header().Set("X-Request-Id", id)
		route := &routeHolder{}
		reqLogger := logger.With("req_id", id)
		ctx := context.WithValue(r.Context(), keyLogger, reqLogger)
		ctx = context.WithValue(ctx, keyRequestID, id)
		ctx = context.WithValue(ctx, keyRoute, route)
		r = r.WithContext(ctx)

		defer func() {
			if rec := recover(); rec != nil {
				if err, ok := rec.(error); ok && errors.Is(err, http.ErrAbortHandler) {
					panic(rec)
				}
				reqLogger.Error("panic", "panic", rec, "stack", string(debug.Stack()))
				if !hw.wrote {
					WriteError(hw, Internal())
				}
			}
			status := hw.status
			if status == 0 {
				status = http.StatusOK
			}
			pattern := route.pattern
			if pattern == "" {
				pattern = "-"
			}
			level := slog.LevelInfo
			if pattern == "/healthz" || pattern == "/readyz" {
				level = slog.LevelDebug // container health probes every 30 s
			}
			reqLogger.LogAttrs(r.Context(), level, "request",
				slog.String("method", r.Method),
				slog.String("route", pattern),
				slog.Int("status", status),
				slog.Int64("dur_ms", time.Since(start).Milliseconds()),
				slog.Int64("bytes", hw.bytes),
			)
		}()
		router.ServeHTTP(hw, r)
		if !hw.wrote {
			// A handler that wrote nothing still gets the security defaults (and loses any
			// Set-Cookie) instead of the server's implicit 200.
			hw.WriteHeader(http.StatusOK)
		}
	})
}

// CapturePattern wraps a ServeMux so the access log learns the matched route pattern. ServeMux
// sets r.Pattern on the request it is given.
func CapturePattern(mux http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		defer func() { // also on panic, so the 500 is logged with its route
			if h, ok := r.Context().Value(keyRoute).(*routeHolder); ok {
				h.pattern = r.Pattern
			}
		}()
		mux.ServeHTTP(w, r)
	})
}

func newRequestID() string {
	var b [12]byte
	_, _ = rand.Read(b[:])
	return hex.EncodeToString(b[:])
}
