package shares

import (
	"context"
	"encoding/json"
	"errors"
	"net/http"
	"regexp"
	"strconv"
	"strings"

	"github.com/mihajlopopara/reqharbor/server/internal/httpx"
	"github.com/mihajlopopara/reqharbor/server/internal/ids"
	"github.com/mihajlopopara/reqharbor/server/internal/jsonx"
	"github.com/mihajlopopara/reqharbor/server/internal/sharepage"
	"github.com/mihajlopopara/reqharbor/server/internal/store"
	"github.com/mihajlopopara/reqharbor/server/internal/web"
)

// pathIDRe is the Worker's SHARE_PATH: a segment that does not match is a route 404.
var pathIDRe = regexp.MustCompile(`^[A-Za-z0-9]{1,64}$`)

const robots = "noindex, nofollow"

// PublicHandler serves GET /s/{id}, GET /s/{id}.json and OPTIONS /s/{id}.json, all registered on
// the single mux pattern /s/{file}.
func (s *Service) PublicHandler() http.Handler {
	jsonRoute := httpx.Methods{
		{Name: "GET", Handler: httpx.Func(s.publicJSON)},
		{Name: "OPTIONS", Handler: http.HandlerFunc(preflight)},
	}
	pageRoute := httpx.Methods{{Name: "GET", Handler: httpx.Func(s.publicPage)}}
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		file := r.PathValue("file")
		id, isJSON := strings.CutSuffix(file, ".json")
		if !pathIDRe.MatchString(id) {
			httpx.WriteError(w, httpx.RouteNotFound(r))
			return
		}
		if isJSON {
			jsonRoute.ServeHTTP(w, r)
		} else {
			pageRoute.ServeHTTP(w, r)
		}
	})
}

func preflight(w http.ResponseWriter, _ *http.Request) {
	h := w.Header()
	h.Set("Access-Control-Allow-Origin", "*")
	h.Set("Access-Control-Allow-Methods", "GET, OPTIONS")
	h.Set("Access-Control-Max-Age", "86400")
	w.WriteHeader(http.StatusNoContent)
}

func shareIDFrom(r *http.Request) string {
	id, _ := strings.CutSuffix(r.PathValue("file"), ".json")
	return id
}

// loadPublic returns an active share, or 404 / 410.
func (s *Service) loadPublic(ctx context.Context, id string) (*store.Share, error) {
	if !ids.ShareIDRe.MatchString(id) {
		return nil, httpx.NotFound("Share not found")
	}
	var sh *store.Share
	err := s.Store.Read(ctx, func(tx store.ReadTx) error {
		var err error
		sh, err = tx.Share(id)
		return err
	})
	if errors.Is(err, store.ErrNotFound) {
		return nil, httpx.NotFound("Share not found")
	}
	if err != nil {
		return nil, err
	}
	if sh.RevokedAt != nil || sh.Snapshot == nil {
		return nil, httpx.Gone()
	}
	return sh, nil
}

type publicShare struct {
	ShareID   string          `json:"shareId"`
	Kind      string          `json:"kind"`
	CreatedAt int64           `json:"createdAt"`
	UpdatedAt int64           `json:"updatedAt"`
	Snapshot  json.RawMessage `json:"snapshot"`
}

func (s *Service) publicJSON(w http.ResponseWriter, r *http.Request) error {
	sh, err := s.loadPublic(r.Context(), shareIDFrom(r))
	if err != nil {
		if e, ok := httpx.AsAPIError(err); ok {
			// Errors keep the default no-store but stay readable cross-origin.
			e = e.WithHeader("X-Robots-Tag", robots).
				WithHeader("Access-Control-Allow-Origin", "*").
				WithHeader("Cross-Origin-Resource-Policy", "cross-origin")
			return e
		}
		return err
	}
	return httpx.WriteJSON(w, http.StatusOK, publicShare{
		ShareID: sh.ID, Kind: sh.Kind, CreatedAt: sh.CreatedAt, UpdatedAt: sh.UpdatedAt, Snapshot: sh.Snapshot,
	}, map[string]string{
		"X-Robots-Tag":                 robots,
		"Cache-Control":                "public, max-age=60",
		"Access-Control-Allow-Origin":  "*",
		"Cross-Origin-Resource-Policy": "cross-origin",
	})
}

func writeHTML(w http.ResponseWriter, status int, body []byte, cache string) error {
	h := w.Header()
	h.Set("Content-Type", "text/html; charset=utf-8")
	h.Set("Content-Security-Policy", web.ShareCSP)
	h.Set("X-Robots-Tag", robots)
	h.Set("Cache-Control", cache)
	h.Set("Content-Length", strconv.Itoa(len(body)))
	w.WriteHeader(status)
	_, _ = w.Write(body)
	return nil
}

// publicPage renders the read-only share page (internal/sharepage).
func (s *Service) publicPage(w http.ResponseWriter, r *http.Request) error {
	sh, err := s.loadPublic(r.Context(), shareIDFrom(r))
	if err != nil {
		if e, ok := httpx.AsAPIError(err); ok && (e.Status == http.StatusNotFound || e.Status == http.StatusGone) {
			page, rerr := sharepage.RenderError(e.Status)
			if rerr != nil {
				return rerr
			}
			return writeHTML(w, e.Status, page, "no-store")
		}
		return err
	}
	snap, err := jsonx.Parse(sh.Snapshot)
	if err != nil {
		return err
	}
	page, err := sharepage.Render(sharepage.NormalizeSnapshot(sh.Kind, snap),
		sharepage.Meta{ShareID: sh.ID, CreatedAt: sh.CreatedAt, UpdatedAt: sh.UpdatedAt})
	if err != nil {
		return err
	}
	return writeHTML(w, http.StatusOK, page, "public, max-age=60")
}
