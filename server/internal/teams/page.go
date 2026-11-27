package teams

import (
	"bytes"
	"html/template"
	"net/http"
	"strconv"

	"github.com/mihajlopopara/reqharbor/server/internal/httpx"
	"github.com/mihajlopopara/reqharbor/server/internal/web"
)

var invitePageTmpl = template.Must(template.New("invite").ParseFS(web.Files, "templates/layout.html", "templates/invite.html"))

// pageData is what layout.html and invite.html use.
type pageData struct {
	Title, ServerName, PublicURL string
	Insecure                     bool
	Style, Logo, Script          template.HTML
}

// invitePage is GET /i: a static page (the token is in the fragment, which browsers never send),
// with the /i CSP, noindex and no-store. It is served whatever REQHARBOR_TEAMS says; with teams off
// its preview call fails and the page says the link is no longer valid.
func (d *Deps) invitePage(w http.ResponseWriter, r *http.Request) {
	data := pageData{
		Title: "Team invite", ServerName: d.Config.ServerName, PublicURL: d.Config.PublicURL,
		Style:  template.HTML("<style>" + web.AuthCSS + "</style>"),    //nolint:gosec // embedded, hashed
		Logo:   template.HTML(web.LogoSVG),                             //nolint:gosec // embedded
		Script: template.HTML("<script>" + web.InviteJS + "</script>"), //nolint:gosec // embedded, hashed
	}
	var buf bytes.Buffer
	if err := invitePageTmpl.ExecuteTemplate(&buf, "layout", data); err != nil {
		httpx.HandleError(w, r, err)
		return
	}
	h := w.Header()
	h.Set("Content-Type", "text/html; charset=utf-8")
	h.Set("Content-Security-Policy", web.InviteCSP)
	h.Set("X-Robots-Tag", "noindex, nofollow")
	h.Set("Cache-Control", "no-store")
	h.Set("Content-Length", strconv.Itoa(buf.Len()))
	w.WriteHeader(http.StatusOK)
	_, _ = w.Write(buf.Bytes())
}
