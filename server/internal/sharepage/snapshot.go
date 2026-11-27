// Package sharepage renders the public, read-only share page (AC-SHARE-3, AC-QRY-1). It ports
// worker/src/snapshot.ts (normalizeSnapshot and friends: never trust the shapes in stored
// snapshots) and worker/src/share-page.ts (tree, crumbs, badges, tables, auth and body blocks,
// the environment block and the error pages), rendered with html/template (06-go-server.md §1.5).
package sharepage

import (
	"errors"
	"math"
	"strconv"

	"github.com/mihajlopopara/reqharbor/server/internal/curlexport"
	"github.com/mihajlopopara/reqharbor/server/internal/jsonx"
	"github.com/mihajlopopara/reqharbor/server/internal/jsx"
)

// Request is a normalised request of a snapshot.
type Request struct {
	curlexport.Request
	// Key is the request's anchor id on the page ("r0", "r1", …).
	Key  string
	Name string
	// FolderID is nil for a request at the collection root.
	FolderID  *string
	SortOrder float64
	// Kind is the request kind; "" for an absent kind (or null), which means HTTP.
	Kind string
	// KindSet is true when the snapshot carried a non-null kind (string or not).
	KindSet bool
}

// IsHTTP reports whether the page can preview the request: an absent kind or "http"
// (AC-FWD-6). Any other kind (graphql, grpc, websocket, socketio, …) gets the import card.
func (r *Request) IsHTTP() bool { return !r.KindSet || r.Kind == "http" }

// Folder is a normalised folder.
type Folder struct {
	ID             string
	Name           string
	ParentFolderID *string
	SortOrder      float64
}

// Environment is a normalised environment with its secret variables removed.
type Environment struct {
	Name      string
	Variables []curlexport.Variable
}

// Snapshot is a normalised share snapshot.
type Snapshot struct {
	Kind        string
	Name        string
	Description string
	// CollectionAuth is set for collection shares.
	CollectionAuth *curlexport.Auth
	Folders        []Folder
	Requests       []*Request
	Environment    *Environment
}

// str is snapshot.ts str(): a string, a number as JavaScript prints it, else d.
func str(v *jsonx.Value, d string) string {
	switch {
	case v == nil:
		return d
	case v.Kind == jsonx.String:
		return v.S
	case v.Kind == jsonx.Number:
		return jsx.NumberString(v.S)
	}
	return d
}

func boolean(v *jsonx.Value, d bool) bool {
	if v != nil && v.Kind == jsonx.Bool {
		return v.B
	}
	return d
}

// num is snapshot.ts num(): a finite number, else d.
func num(v *jsonx.Value, d float64) float64 {
	if v == nil || v.Kind != jsonx.Number {
		return d
	}
	f, err := strconv.ParseFloat(v.S, 64)
	if errors.Is(err, strconv.ErrSyntax) || math.IsInf(f, 0) || math.IsNaN(f) {
		return d
	}
	return f
}

func arr(v *jsonx.Value) []*jsonx.Value {
	if v.IsArray() {
		return v.Arr
	}
	return nil
}

var empty = &jsonx.Value{Kind: jsonx.Object}

func obj(v *jsonx.Value) *jsonx.Value {
	if v.IsObject() {
		return v
	}
	return empty
}

func strPtr(v *jsonx.Value) *string {
	if s, ok := v.Str(); ok {
		return &s
	}
	return nil
}

var authTypes = map[string]bool{"none": true, "inherit": true, "basic": true, "bearer": true, "apikey": true}

var bodyTypes = map[string]bool{
	"none": true, "json": true, "xml": true, "text": true, "form": true, "formEncoded": true, "graphql": true, "binary": true,
}

func kvList(v *jsonx.Value) []curlexport.KeyValue {
	items := arr(v)
	out := make([]curlexport.KeyValue, len(items))
	for i, x := range items {
		o := obj(x)
		out[i] = curlexport.KeyValue{Key: str(o.Get("key"), ""), Value: str(o.Get("value"), ""), Enabled: boolean(o.Get("enabled"), true)}
	}
	return out
}

// NormalizeAuth is snapshot.ts normalizeAuth.
func NormalizeAuth(v *jsonx.Value) curlexport.Auth {
	o := obj(v)
	t, _ := o.Get("type").Str()
	if !authTypes[t] {
		t = "none"
	}
	ak := obj(o.Get("apiKey"))
	in := "header"
	if s, _ := ak.Get("in").Str(); s == "query" {
		in = "query"
	}
	return curlexport.Auth{
		Type:     t,
		Username: str(o.Get("username"), ""),
		Password: str(o.Get("password"), ""),
		Token:    str(o.Get("token"), ""),
		APIKey:   curlexport.APIKey{Key: str(ak.Get("key"), ""), Value: str(ak.Get("value"), ""), In: in},
	}
}

func normalizeBody(v *jsonx.Value) curlexport.Body {
	o := obj(v)
	t, _ := o.Get("type").Str()
	if !bodyTypes[t] {
		t = "none"
	}
	gq := obj(o.Get("graphql"))
	formItems := arr(o.Get("form"))
	form := make([]curlexport.FormField, len(formItems))
	for i, x := range formItems {
		f := obj(x)
		kind := "text"
		if s, _ := f.Get("kind").Str(); s == "file" {
			kind = "file"
		}
		ct, _ := f.Get("contentType").Str()
		form[i] = curlexport.FormField{
			Key: str(f.Get("key"), ""), Value: str(f.Get("value"), ""), Enabled: boolean(f.Get("enabled"), true),
			Kind: kind, ContentType: ct,
		}
	}
	return curlexport.Body{
		Type:        t,
		Text:        str(o.Get("text"), ""),
		Form:        form,
		FormEncoded: kvList(o.Get("formEncoded")),
		GraphQL:     curlexport.GraphQL{Query: str(gq.Get("query"), ""), Variables: gq.Get("variables")},
		BinaryPath:  str(o.Get("binaryPath"), ""),
	}
}

func normalizeRequest(v *jsonx.Value, index int) *Request {
	o := obj(v)
	s := obj(o.Get("settings"))
	// Kept raw like the core model (default "GET" only when absent); the exporter trims and
	// validates it.
	method, ok := o.Get("method").Str()
	if !ok {
		method = "GET"
	}
	name := str(o.Get("name"), "")
	if name == "" {
		name = "Untitled request"
	}
	r := &Request{
		Key:       "r" + strconv.Itoa(index),
		Name:      name,
		FolderID:  strPtr(o.Get("folderId")),
		SortOrder: num(o.Get("sortOrder"), float64(index)),
		Request: curlexport.Request{
			Method:  method,
			URL:     str(o.Get("url"), ""),
			Params:  kvList(o.Get("params")),
			Headers: kvList(o.Get("headers")),
			Auth:    NormalizeAuth(o.Get("auth")),
			Body:    normalizeBody(o.Get("body")),
			Settings: &curlexport.Settings{
				FollowRedirects: boolean(s.Get("followRedirects"), true),
				TimeoutMs:       num(s.Get("timeoutMs"), 30000),
				VerifyTLS:       boolean(s.Get("verifyTls"), true),
			},
		},
	}
	// AC-FWD-6: a request kind this page cannot preview is never shown as a GET.
	if k := o.Get("kind"); k != nil && !k.IsNull() {
		r.KindSet = true
		r.Kind, _ = k.Str()
		if r.Kind == "" && k.Kind != jsonx.String {
			r.Kind = "unknown"
		}
	}
	return r
}

// NormalizeSnapshot is snapshot.ts normalizeSnapshot for kind "collection" or "request".
func NormalizeSnapshot(kind string, snapshot *jsonx.Value) *Snapshot {
	root := obj(snapshot)
	var env *Environment
	if e := root.Get("environment"); e.IsObject() {
		name := str(e.Get("name"), "")
		if name == "" {
			name = "Environment"
		}
		env = &Environment{Name: name, Variables: []curlexport.Variable{}}
		for _, x := range arr(e.Get("variables")) {
			if x.IsObject() && x.Get("secret").IsTrue() {
				continue
			}
			o := obj(x)
			env.Variables = append(env.Variables, curlexport.Variable{
				Key: str(o.Get("key"), ""), Value: str(o.Get("value"), ""), Enabled: boolean(o.Get("enabled"), true),
			})
		}
	}

	if kind == "request" {
		req := normalizeRequest(root.Get("request"), 0)
		req.FolderID = nil
		return &Snapshot{Kind: kind, Name: req.Name, Folders: []Folder{}, Requests: []*Request{req}, Environment: env}
	}
	c := obj(root.Get("collection"))
	name := str(c.Get("name"), "")
	if name == "" {
		name = "Untitled collection"
	}
	auth := NormalizeAuth(c.Get("auth"))
	out := &Snapshot{Kind: kind, Name: name, Description: str(c.Get("description"), ""), CollectionAuth: &auth, Environment: env}
	for i, x := range arr(c.Get("folders")) {
		f := obj(x)
		id := str(f.Get("id"), "")
		if id == "" {
			id = "folder-" + strconv.Itoa(i)
		}
		fname := str(f.Get("name"), "")
		if fname == "" {
			fname = "Untitled folder"
		}
		out.Folders = append(out.Folders, Folder{ID: id, Name: fname, ParentFolderID: strPtr(f.Get("parentFolderId")), SortOrder: num(f.Get("sortOrder"), float64(i))})
	}
	for i, x := range arr(c.Get("requests")) {
		out.Requests = append(out.Requests, normalizeRequest(x, i))
	}
	return out
}
