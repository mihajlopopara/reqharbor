package sharepage

import (
	"bytes"
	"errors"
	"html/template"
	"net/http"
	"slices"
	"strconv"
	"strings"
	"time"
	"unicode/utf16"

	"github.com/mihajlopopara/reqharbor/server/internal/curlexport"
	"github.com/mihajlopopara/reqharbor/server/internal/jsx"
	"github.com/mihajlopopara/reqharbor/server/internal/web"
)

// Segment is a run of text on the page; IsVar marks a `{{variable}}`, which the template wraps
// in <span class="var">. Escaping is left to html/template.
type Segment struct {
	Text  string
	IsVar bool
}

// WithVars splits text into plain runs and `{{variable}}` references (share-page.ts withVars).
func WithVars(text string) []Segment {
	var out []Segment
	last := 0
	for _, m := range curlexport.VarRe.FindAllStringIndex(text, -1) {
		if m[0] > last {
			out = append(out, Segment{Text: text[last:m[0]]})
		}
		out = append(out, Segment{Text: text[m[0]:m[1]], IsVar: true})
		last = m[1]
	}
	if last < len(text) {
		out = append(out, Segment{Text: text[last:]})
	}
	return out
}

var templates = template.Must(template.New("share").Funcs(template.FuncMap{"segs": WithVars}).
	ParseFS(web.Files, "templates/share.html", "templates/share_error.html"))

// Meta is the share row data the page shows.
type Meta struct {
	ShareID              string
	CreatedAt, UpdatedAt int64
}

type badge struct {
	Known               bool
	Class, Label, Title string
}

var knownMethods = map[string]bool{"GET": true, "POST": true, "PUT": true, "PATCH": true, "DELETE": true, "QUERY": true, "HEAD": true, "OPTIONS": true}

func methodBadge(raw string) badge {
	method := jsx.Trim(raw)
	upper := strings.ToUpper(method)
	if knownMethods[upper] {
		return badge{Known: true, Class: strings.ToLower(upper), Label: upper}
	}
	return customBadge(method)
}

// customBadge shows up to 7 UTF-16 code units of label, as the Worker's slice(0, 7) did.
func customBadge(label string) badge {
	short := label
	if u := utf16.Encode([]rune(label)); len(u) > 7 {
		short = string(utf16.Decode(u[:7])) + "…"
	}
	return badge{Class: "custom", Label: short, Title: label}
}

type kvRow struct {
	Key, Value string
	Enabled    bool
	Extra      string
}

type kvTable struct {
	Label     string
	Count     int
	ExtraHead string
	Rows      []kvRow
}

// newKV returns nil for an empty table, which renders nothing.
func newKV(rows []kvRow, label, extraHead string) *kvTable {
	if len(rows) == 0 {
		return nil
	}
	t := &kvTable{Label: label, ExtraHead: extraHead, Rows: rows}
	for _, r := range rows {
		if r.Enabled {
			t.Count++
		}
	}
	return t
}

func kvRows(kvs []curlexport.KeyValue) []kvRow {
	rows := make([]kvRow, len(kvs))
	for i, kv := range kvs {
		rows[i] = kvRow{Key: kv.Key, Value: kv.Value, Enabled: kv.Enabled}
	}
	return rows
}

type field struct{ Label, Value string }

type authView struct {
	Meta   string
	Fields []field
}

type codeBlock struct{ Label, Text string }

type bodyView struct {
	Codes []codeBlock
	Table *kvTable
}

type reqView struct {
	Key, Name   string
	Crumbs      []string
	Badge       badge
	URL         string
	HTTP        bool
	CanCopy     bool
	ExportError string
	Notes       []string
	Params      *kvTable
	Headers     *kvTable
	Auth        *authView
	QueryHint   bool
	Body        *bodyView
	Resolved    string
	Raw         string
	HasEnv      bool
}

type navItem struct {
	Open, Close bool
	Depth       int
	Name, Key   string
	Badge       badge
}

type envView struct {
	Name  string
	Table *kvTable
}

type pageView struct {
	Title        string
	IsCollection bool
	Meta         string
	Nav          []navItem
	Description  string
	Env          *envView
	Sections     []*reqView
	Style        template.HTML
	Script       template.HTML
	Logo         template.HTML
}

// The inline assets enter the page as whole elements, so html/template never rewrites them and
// the CSP hashes (web.ShareCSP) stay valid.
var (
	styleHTML  = template.HTML("<style>" + web.ShareCSS + "</style>")
	scriptHTML = template.HTML("<script>" + web.ShareJS + "</script>")
	logoHTML   = template.HTML(web.LogoSVG)
)

func formatTime(ms int64) string {
	return time.UnixMilli(ms).UTC().Format("2006-01-02 15:04") + " UTC"
}

func renderAuth(req *Request, snap *Snapshot) *authView {
	a := req.Auth
	switch a.Type {
	case "basic":
		return &authView{"Basic auth", []field{{"Username", a.Username}, {"Password", a.Password}}}
	case "bearer":
		return &authView{"Bearer token", []field{{"Token", a.Token}}}
	case "apikey":
		in := "header"
		if a.APIKey.In == "query" {
			in = "query"
		}
		return &authView{"API key in " + in, []field{{"Key", a.APIKey.Key}, {"Value", a.APIKey.Value}}}
	case "inherit":
		t := "none"
		if snap.CollectionAuth != nil {
			t = snap.CollectionAuth.Type
		}
		if t == "none" {
			t = "no auth"
		}
		return &authView{Meta: "Inherits " + t + " from the collection"}
	}
	return nil
}

func renderBody(req *Request) *bodyView {
	b := req.Body
	code := func(label, text string) *bodyView { return &bodyView{Codes: []codeBlock{{label, text}}} }
	switch b.Type {
	case "json":
		return code("JSON Content", PrettyJSON(b.Text))
	case "xml":
		return code("XML Content", b.Text)
	case "text":
		return code("Text Content", b.Text)
	case "graphql":
		v := b.GraphQL.Variables
		vars := ""
		if s, ok := v.Str(); ok {
			vars = PrettyJSON(s)
		} else if v != nil && !v.IsNull() {
			vars = prettyValue(v)
		}
		out := code("GraphQL Query", b.GraphQL.Query)
		if vars != "" {
			out.Codes = append(out.Codes, codeBlock{"GraphQL Variables", vars})
		}
		return out
	case "formEncoded":
		if t := newKV(kvRows(b.FormEncoded), "Form-encode", ""); t != nil {
			return &bodyView{Table: t}
		}
	case "form":
		rows := make([]kvRow, len(b.Form))
		for i, f := range b.Form {
			extra := "Text"
			if f.Kind == "file" {
				extra = "File"
			}
			rows[i] = kvRow{Key: f.Key, Value: f.Value, Enabled: f.Enabled, Extra: extra}
		}
		if t := newKV(rows, "Form", "Kind"); t != nil {
			return &bodyView{Table: t}
		}
	case "binary":
		return code("Binary file", b.BinaryPath)
	}
	return nil
}

// MaxPageExpansionBytes bounds the variable expansion of one page view across all its requests
// (W11), on top of curlexport.MaxExpansionBytes per request. The page is rendered on every
// unauthenticated GET and nothing is cached, so without it a 1 MB snapshot of many requests
// could still multiply the per-request budget. Requests past it show "Can't export as cURL".
const MaxPageExpansionBytes = 2 << 20

func renderRequest(req *Request, snap *Snapshot, crumbs []string, vars curlexport.Vars, budget *curlexport.Budget) (*reqView, error) {
	v := &reqView{Key: req.Key, Name: req.Name, Crumbs: crumbs, URL: req.URL, HTTP: req.IsHTTP()}
	if !v.HTTP {
		// AC-FWD-6: an import card instead of a GET; no Copy button and no cURL block.
		v.Badge = customBadge(req.Kind)
		return v, nil
	}
	v.Badge = methodBadge(req.Method)
	v.HasEnv = snap.Environment != nil
	opts := curlexport.Options{InheritedAuth: snap.CollectionAuth}
	raw, err := curlexport.ToCurl(&req.Request, curlexport.Options{InheritedAuth: opts.InheritedAuth, KeepVariables: true})
	resolved := raw
	if err == nil && v.HasEnv {
		opts.Variables, opts.Budget = vars, budget
		resolved, err = curlexport.ToCurl(&req.Request, opts)
	}
	var ee *curlexport.ExportError
	switch {
	case err == nil:
		v.CanCopy, v.Raw, v.Resolved = true, raw, resolved
	case errors.As(err, &ee):
		v.ExportError = ee.Msg
	default:
		return nil, err
	}
	s := req.Settings
	if !s.FollowRedirects {
		v.Notes = append(v.Notes, "Redirects not followed")
	}
	if s.TimeoutMs != 30000 {
		v.Notes = append(v.Notes, "Timeout "+jsx.FormatNumber(s.TimeoutMs)+" ms")
	}
	if !s.VerifyTLS {
		v.Notes = append(v.Notes, "TLS verification off")
	}
	v.QueryHint = strings.ToUpper(req.Method) == "QUERY" && req.Body.Type == "none"
	v.Params = newKV(kvRows(req.Params), "Query", "")
	v.Headers = newKV(kvRows(req.Headers), "Headers", "")
	v.Auth = renderAuth(req, snap)
	v.Body = renderBody(req)
	return v, nil
}

// byOrder sorts by sortOrder, then by name: strings.ToLower and then bytes, instead of the
// Worker's ICU localeCompare (W6, cosmetic).
func byOrder(ao, bo float64, an, bn string) int {
	if ao != bo {
		if ao < bo {
			return -1
		}
		return 1
	}
	if c := strings.Compare(strings.ToLower(an), strings.ToLower(bn)); c != 0 {
		return c
	}
	return strings.Compare(an, bn)
}

const maxTreeDepth = 32

// Render renders the share page for a normalised snapshot.
func Render(snap *Snapshot, meta Meta) ([]byte, error) {
	vars := curlexport.Vars{}
	budget := curlexport.NewBudget(MaxPageExpansionBytes)
	if snap.Environment != nil {
		vars = curlexport.VariableMap(snap.Environment.Variables)
	}

	// Tree (share-page.ts buildTree): parents that do not exist, or a folder that is its own
	// parent, put the item at the root.
	folderIDs := map[string]bool{}
	for _, f := range snap.Folders {
		folderIDs[f.ID] = true
	}
	folders := map[string][]Folder{}
	for _, f := range snap.Folders {
		parent := ""
		if p := f.ParentFolderID; p != nil && *p != "" && folderIDs[*p] && *p != f.ID {
			parent = "/" + *p
		}
		folders[parent] = append(folders[parent], f)
	}
	requests := map[string][]*Request{}
	for _, r := range snap.Requests {
		parent := ""
		if p := r.FolderID; p != nil && *p != "" && folderIDs[*p] {
			parent = "/" + *p
		}
		requests[parent] = append(requests[parent], r)
	}
	for _, list := range folders {
		slices.SortStableFunc(list, func(a, b Folder) int { return byOrder(a.SortOrder, b.SortOrder, a.Name, b.Name) })
	}
	for _, list := range requests {
		slices.SortStableFunc(list, func(a, b *Request) int { return byOrder(a.SortOrder, b.SortOrder, a.Name, b.Name) })
	}

	page := &pageView{
		Title: snap.Name, IsCollection: snap.Kind == "collection", Description: snap.Description,
		Style: styleHTML, Script: scriptHTML, Logo: logoHTML,
	}
	visited := map[string]bool{}
	var walk func(parent string, depth int, crumbs []string) error
	walk = func(parent string, depth int, crumbs []string) error {
		if depth > maxTreeDepth {
			return nil
		}
		d := min(depth, 8)
		for _, f := range folders[parent] {
			if visited[f.ID] { // guards against parent cycles in client data
				continue
			}
			visited[f.ID] = true
			page.Nav = append(page.Nav, navItem{Open: true, Depth: d, Name: f.Name})
			if err := walk("/"+f.ID, depth+1, append(slices.Clip(crumbs), f.Name)); err != nil {
				return err
			}
			page.Nav = append(page.Nav, navItem{Close: true})
		}
		for _, r := range requests[parent] {
			b := methodBadge(r.Method)
			if !r.IsHTTP() {
				b = customBadge(r.Kind)
			}
			page.Nav = append(page.Nav, navItem{Depth: d, Name: r.Name, Key: r.Key, Badge: b})
			sec, err := renderRequest(r, snap, crumbs, vars, budget)
			if err != nil {
				return err
			}
			page.Sections = append(page.Sections, sec)
		}
		return nil
	}
	var crumbs []string
	if page.IsCollection {
		crumbs = []string{snap.Name}
	}
	if err := walk("", 0, crumbs); err != nil {
		return nil, err
	}

	if env := snap.Environment; env != nil {
		rows := make([]kvRow, len(env.Variables))
		for i, v := range env.Variables {
			rows[i] = kvRow{Key: v.Key, Value: v.Value, Enabled: v.Enabled}
		}
		page.Env = &envView{Name: env.Name, Table: newKV(rows, "Variables", "")}
	}
	kind, plural := "request", "s"
	if page.IsCollection {
		kind = "collection"
	}
	if len(snap.Requests) == 1 {
		plural = ""
	}
	page.Meta = "Read-only shared " + kind + " · " + strconv.Itoa(len(snap.Requests)) + " request" + plural +
		" · Updated " + formatTime(meta.UpdatedAt)

	var b bytes.Buffer
	if err := templates.ExecuteTemplate(&b, "page", page); err != nil {
		return nil, err
	}
	return b.Bytes(), nil
}

type errorView struct {
	Title, Message string
	Style          template.HTML
	Logo           template.HTML
}

// RenderError renders the 404 or 410 page (share-page.ts renderErrorPage).
func RenderError(status int) ([]byte, error) {
	v := errorView{Title: "Share not found", Message: "This share link does not exist. Check that you copied the whole URL.", Style: styleHTML, Logo: logoHTML}
	if status == http.StatusGone {
		v.Title, v.Message = "This link has been revoked", "The owner revoked this share link. Ask them for a new one."
	}
	var b bytes.Buffer
	if err := templates.ExecuteTemplate(&b, "error", v); err != nil {
		return nil, err
	}
	return b.Bytes(), nil
}
