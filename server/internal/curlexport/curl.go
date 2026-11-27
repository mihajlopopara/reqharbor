// Package curlexport is the share page's cURL exporter (PRODUCT.md §5.5, AC-CEXP-2..5,
// AC-QRY-5): a line-by-line port of worker/src/curl.ts, which ports `prepare_with_boundary` and
// `export_curl` from crates/core/src/curl/export.rs, prepare.rs and urlx.rs (QA-R1-006). Keep it
// in step with the Rust exporter; worker/test/fixtures/curl_parity.json must pass on both sides.
package curlexport

import (
	"encoding/base64"
	"fmt"
	"regexp"
	"strings"

	"github.com/mihajlopopara/reqharbor/server/internal/jsonx"
)

// KeyValue is a header, query or form-encoded row.
type KeyValue struct {
	Key, Value string
	Enabled    bool
}

// FormField is a multipart field.
type FormField struct {
	Key, Value string
	Enabled    bool
	// Kind is "text" or "file" (the value is then the file path).
	Kind string
	// ContentType is the optional per-field content type (`;type=` in cURL).
	ContentType string
}

// APIKey is the API-key auth scheme; In is "header" or "query".
type APIKey struct{ Key, Value, In string }

// Auth is a request or collection auth: Type is none, inherit, basic, bearer or apikey.
type Auth struct {
	Type               string
	Username, Password string
	Token              string
	APIKey             APIKey
}

// GraphQL is a GraphQL body. Variables is the editor's JSON text when it is a JSON string (the
// core model); any other JSON value is serialised first. nil means absent.
type GraphQL struct {
	Query     string
	Variables *jsonx.Value
}

// Body is a request body: Type is none, json, xml, text, form, formEncoded, graphql or binary.
type Body struct {
	Type        string
	Text        string
	Form        []FormField
	FormEncoded []KeyValue
	GraphQL     GraphQL
	BinaryPath  string
}

// Settings are the request settings the exporter reads.
type Settings struct {
	FollowRedirects bool
	TimeoutMs       float64
	VerifyTLS       bool
}

// Request is what the exporter needs of a request.
type Request struct {
	Method string
	URL    string
	// Params are the query rows; when non-empty the URL's query string is rebuilt from the
	// enabled rows (AC-REQ-2).
	Params  []KeyValue
	Headers []KeyValue
	Auth    Auth
	Body    Body
	// Settings nil means the defaults (TLS verified).
	Settings *Settings
}

// Options are the export options.
type Options struct {
	// Variables to resolve (AC-CEXP-5). Ignored when KeepVariables is true.
	Variables Vars
	// KeepVariables is the "Keep {{variables}}" toggle: export raw text.
	KeepVariables bool
	// InheritedAuth is the parent collection's auth, used when the request inherits.
	InheritedAuth *Auth
	// Budget, when set, is shared expansion work across several exports (the share page's
	// per-page budget, W11). Each export also stops at MaxExpansionBytes on its own.
	Budget *Budget
}

// ExportError has the same messages as `prepare::PrepareError`.
type ExportError struct{ Msg string }

func (e *ExportError) Error() string { return e.Msg }

// LineJoin separates command parts: ` \` + newline + 2-space indent (`LINE_JOIN`).
const LineJoin = " \\\n  "

// ShellQuote is POSIX single-quote escaping (`shell::quote`): each ' becomes the four
// characters quote, backslash, quote, quote, and the whole value is wrapped in single quotes.
func ShellQuote(value string) string {
	return "'" + strings.ReplaceAll(value, "'", `'\''`) + "'"
}

var methodRe = regexp.MustCompile(`^[A-Z][A-Z0-9_-]*$`)

func asciiLower(s string) string {
	b := []byte(s)
	for i, c := range b {
		if c >= 'A' && c <= 'Z' {
			b[i] = c + 'a' - 'A'
		}
	}
	return string(b)
}

func eqIgnoreASCIICase(a, b string) bool { return asciiLower(a) == asciiLower(b) }

// percentEncode is `percent_encoding::utf8_percent_encode` with CONTROLS + extra (non-ASCII
// always encoded).
func percentEncode(s string, keep func(b byte) bool) string {
	var out strings.Builder
	for i := 0; i < len(s); i++ {
		b := s[i]
		if keep(b) {
			out.WriteByte(b)
		} else {
			fmt.Fprintf(&out, "%%%02X", b)
		}
	}
	return out.String()
}

func isControlOrHigh(b byte) bool { return b < 0x20 || b == 0x7f || b >= 0x80 }

func inQueryValueSet(b byte) bool { return strings.IndexByte(" &#\"<>", b) >= 0 }

// EncodeQueryValue percent-encodes a query value like urlx.rs.
func EncodeQueryValue(s string) string {
	return percentEncode(s, func(b byte) bool { return !isControlOrHigh(b) && !inQueryValueSet(b) })
}

// EncodeQueryKey percent-encodes a query key like urlx.rs (also `=`).
func EncodeQueryKey(s string) string {
	return percentEncode(s, func(b byte) bool { return !isControlOrHigh(b) && !inQueryValueSet(b) && b != '=' })
}

// FormURLEncode is `prepare::form_urlencode`: everything but ASCII alphanumerics and `-._~`.
func FormURLEncode(s string) string {
	return percentEncode(s, func(b byte) bool {
		return b >= 'a' && b <= 'z' || b >= 'A' && b <= 'Z' || b >= '0' && b <= '9' || strings.IndexByte("-._~", b) >= 0
	})
}

// splitURL is `urlx::split_url`: base, query (or nil) and fragment (or nil).
func splitURL(url string) (string, *string, *string) {
	rest := url
	var fragment *string
	if h := strings.IndexByte(url, '#'); h >= 0 {
		rest = url[:h]
		f := url[h+1:]
		fragment = &f
	}
	if q := strings.IndexByte(rest, '?'); q >= 0 {
		query := rest[q+1:]
		return rest[:q], &query, fragment
	}
	return rest, nil, fragment
}

// buildQuery is `urlx::build_query`.
func buildQuery(params []KeyValue) string {
	var out strings.Builder
	for _, p := range params {
		if !p.Enabled || (p.Key == "" && p.Value == "") {
			continue
		}
		if out.Len() > 0 {
			out.WriteByte('&')
		}
		out.WriteString(EncodeQueryKey(p.Key))
		if p.Value != "" {
			out.WriteString("=" + EncodeQueryValue(p.Value))
		}
	}
	return out.String()
}

var schemeRe = regexp.MustCompile(`^[A-Za-z][A-Za-z0-9+\-.]*$`)

func hasScheme(url string) bool {
	i := strings.Index(url, "://")
	return i > 0 && schemeRe.MatchString(url[:i])
}

func hostOf(rest string) string {
	authority := rest
	if m := strings.IndexAny(rest, "/?#"); m >= 0 {
		authority = rest[:m]
	}
	if at := strings.LastIndexByte(authority, '@'); at >= 0 {
		authority = authority[at+1:]
	}
	if strings.HasPrefix(authority, "[") {
		end := strings.IndexByte(authority, ']')
		if end < 0 {
			return authority
		}
		return authority[:end+1]
	}
	if colon := strings.IndexByte(authority, ':'); colon >= 0 {
		return authority[:colon]
	}
	return authority
}

// WithDefaultScheme is AC-REQ-10 (`urlx::apply_default_scheme`): schemeless URLs get http:// for
// loopback hosts and https:// otherwise.
func WithDefaultScheme(url string) string {
	trimmed := RustTrim(url)
	if hasScheme(trimmed) || strings.HasPrefix(trimmed, "{{") {
		return trimmed
	}
	rest := trimmed
	for strings.HasPrefix(rest, "//") {
		rest = rest[2:]
	}
	scheme := "https"
	switch asciiLower(hostOf(rest)) {
	case "localhost", "127.0.0.1", "[::1]":
		scheme = "http"
	}
	return scheme + "://" + rest
}

var defaultContentType = map[string]string{
	"json":        "application/json",
	"graphql":     "application/json",
	"xml":         "application/xml",
	"text":        "text/plain",
	"formEncoded": "application/x-www-form-urlencoded",
	"form":        "multipart/form-data",
	"binary":      "application/octet-stream",
}

// effectiveAuth is `prepare::effective_auth`; nil means no auth.
func effectiveAuth(auth *Auth, collection *Auth) *Auth {
	if auth.Type == "inherit" {
		if collection != nil && collection.Type != "inherit" && collection.Type != "none" {
			return collection
		}
		return nil
	}
	if auth.Type == "none" {
		return nil
	}
	return auth
}

func needsFormString(v string) bool {
	return strings.HasPrefix(v, "@") || strings.HasPrefix(v, "<") || strings.HasPrefix(v, `"`) || strings.Contains(v, ";")
}

func formPath(p string) string {
	if !strings.ContainsAny(p, `;,"\`) {
		return p
	}
	return `"` + strings.ReplaceAll(strings.ReplaceAll(p, `\`, `\\`), `"`, `\"`) + `"`
}

// GraphQLVariablesText is the variables text the exporter and the share page start from: the
// string itself, "" when absent or null, or the serialised JSON of any other value.
func GraphQLVariablesText(v *jsonx.Value) string {
	if s, ok := v.Str(); ok {
		return s
	}
	if v == nil || v.IsNull() {
		return ""
	}
	b, err := jsonx.Canonical(v)
	if err != nil {
		return ""
	}
	return string(b)
}

type header struct{ k, v string }

type part struct {
	name, kind, value, contentType string
}

const (
	bodyNone = iota
	bodyBytes
	bodyFile
	bodyMultipart
)

var globRe = regexp.MustCompile(`[{}\[\]]`)

// ToCurl exports req as a multi-line bash cURL command. It fails with *ExportError like
// `PrepareError`.
func ToCurl(req *Request, opts Options) (string, error) {
	scope := opts.Variables
	if opts.KeepVariables || scope == nil {
		scope = Vars{}
	}
	// Variable expansion is budgeted (W11); the first overflow makes the export fail with
	// ErrTooLarge, and r returns "" from then on so no further work is done.
	budget := NewBudget(MaxExpansionBytes)
	if opts.Budget != nil {
		budget.left = min(budget.left, opts.Budget.left)
		start := budget.left
		defer func() { opts.Budget.left -= start - budget.left }()
	}
	var rerr error
	r := func(s string) string {
		if rerr != nil {
			return ""
		}
		out, err := ResolveVariablesBudget(s, scope, budget)
		if err != nil {
			rerr = err
			return ""
		}
		return out
	}

	// --- prepare_with_boundary ---
	method := RustTrim(req.Method)
	if !methodRe.MatchString(method) {
		return "", &ExportError{fmt.Sprintf("Invalid method \"%s\": use A–Z, 0–9, _ or -, starting with a letter", req.Method)}
	}
	if RustTrim(req.URL) == "" {
		return "", &ExportError{"URL is empty"}
	}

	var headers []header
	for _, h := range req.Headers {
		if h.Enabled && RustTrim(h.Key) != "" {
			headers = append(headers, header{r(RustTrim(h.Key)), r(h.Value)})
		}
	}
	hasHeader := func(name string) bool {
		for _, h := range headers {
			if eqIgnoreASCIICase(h.k, name) {
				return true
			}
		}
		return false
	}

	auth := effectiveAuth(&req.Auth, opts.InheritedAuth)
	var extraQuery []header
	if auth != nil {
		switch auth.Type {
		case "basic":
			if !hasHeader("Authorization") {
				creds := base64.StdEncoding.EncodeToString([]byte(r(auth.Username) + ":" + r(auth.Password)))
				headers = append(headers, header{"Authorization", "Basic " + creds})
			}
		case "bearer":
			if !hasHeader("Authorization") {
				headers = append(headers, header{"Authorization", "Bearer " + r(auth.Token)})
			}
		case "apikey":
			if RustTrim(auth.APIKey.Key) != "" {
				key := r(RustTrim(auth.APIKey.Key))
				value := r(auth.APIKey.Value)
				if auth.APIKey.In == "query" {
					extraQuery = append(extraQuery, header{key, value})
				} else if !hasHeader(key) {
					headers = append(headers, header{key, value})
				}
			}
		}
	}

	// URL (AC-REQ-2, AC-REQ-10)
	raw := RustTrim(req.URL)
	var url string
	if len(req.Params) == 0 {
		url = r(raw)
	} else {
		base, _, fragment := splitURL(raw)
		var rows []KeyValue
		for _, p := range req.Params {
			if p.Enabled {
				rows = append(rows, KeyValue{Key: r(p.Key), Value: r(p.Value), Enabled: true})
			}
		}
		url = r(base)
		if q := buildQuery(rows); q != "" {
			url += "?" + q
		}
		if fragment != nil {
			url += "#" + *fragment
		}
	}
	if len(extraQuery) > 0 {
		base, query, fragment := splitURL(url)
		q := ""
		if query != nil {
			q = *query
		}
		for _, kv := range extraQuery {
			if q != "" {
				q += "&"
			}
			q += EncodeQueryKey(kv.k)
			if kv.v != "" {
				q += "=" + EncodeQueryValue(kv.v)
			}
		}
		url = base + "?" + q
		if fragment != nil {
			url += "#" + *fragment
		}
	}
	url = WithDefaultScheme(url)

	// Body (AC-REQ-4, AC-REQ-5)
	b := &req.Body
	contentType := defaultContentType[b.Type]
	kind := bodyNone
	var text, path string
	var parts []part
	switch b.Type {
	case "json", "xml", "text":
		kind, text = bodyBytes, r(b.Text)
	case "graphql":
		query := r(b.GraphQL.Query)
		varsText := r(GraphQLVariablesText(b.GraphQL.Variables))
		variables := "{}"
		if RustTrim(varsText) != "" {
			v, err := jsonx.SerdeReserialize(varsText)
			if err != nil {
				line, column := 1, 1
				if se, ok := err.(*jsonx.SerdeError); ok {
					line, column = se.Line, se.Column
				}
				return "", &ExportError{fmt.Sprintf("Invalid GraphQL variables: line %d, column %d: %s", line, column, err.Error())}
			}
			variables = v
		}
		kind, text = bodyBytes, `{"query":`+jsonx.SerdeString(query)+`,"variables":`+variables+`}`
	case "formEncoded":
		kind, text = bodyBytes, "" // exported per field below
	case "form":
		kind = bodyMultipart
		for _, f := range b.Form {
			if f.Enabled && f.Key != "" {
				parts = append(parts, part{name: r(f.Key), kind: f.Kind, value: r(f.Value), contentType: f.ContentType})
			}
		}
		contentType = "multipart/form-data; boundary=boundary"
	case "binary":
		kind, path = bodyFile, r(b.BinaryPath)
	}
	if contentType != "" && kind != bodyNone && !hasHeader("Content-Type") {
		headers = append(headers, header{"Content-Type", contentType})
	}

	// --- export_curl ---
	explicit := func(name string) bool {
		for _, h := range req.Headers {
			if h.Enabled && eqIgnoreASCIICase(RustTrim(h.Key), name) {
				return true
			}
		}
		return false
	}
	without := func(name string) {
		kept := headers[:0]
		for _, h := range headers {
			if !eqIgnoreASCIICase(h.k, name) {
				kept = append(kept, h)
			}
		}
		headers = kept
	}
	basic, hasBasic := "", false
	if auth != nil && auth.Type == "basic" && !explicit("authorization") {
		without("authorization")
		basic, hasBasic = r(auth.Username)+":"+r(auth.Password), true
	}
	if (b.Type == "form" || b.Type == "formEncoded") && !explicit("content-type") {
		// cURL sets these itself (and generates its own multipart boundary).
		without("content-type")
	}

	var bodyParts []string
	switch {
	case kind == bodyFile:
		bodyParts = append(bodyParts, "--data-binary "+ShellQuote("@"+path))
	case kind == bodyMultipart:
		for _, p := range parts {
			typ := ""
			if p.contentType != "" {
				typ = ";type=" + p.contentType
			}
			switch {
			case p.kind == "file":
				bodyParts = append(bodyParts, "-F "+ShellQuote(p.name+"=@"+formPath(p.value)+typ))
			case needsFormString(p.value):
				bodyParts = append(bodyParts, "--form-string "+ShellQuote(p.name+"="+p.value))
			default:
				bodyParts = append(bodyParts, "-F "+ShellQuote(p.name+"="+p.value+typ))
			}
		}
	case kind == bodyBytes && b.Type == "formEncoded":
		for _, f := range b.FormEncoded {
			if !f.Enabled || (f.Key == "" && f.Value == "") {
				continue
			}
			key, value := r(f.Key), r(f.Value)
			// `--data-urlencode '=v'` would drop the `=`; send it verbatim.
			if key == "" {
				bodyParts = append(bodyParts, "--data-raw "+ShellQuote("="+FormURLEncode(value)))
			} else {
				bodyParts = append(bodyParts, "--data-urlencode "+ShellQuote(FormURLEncode(key)+"="+value))
			}
		}
	case kind == bodyBytes:
		bodyParts = append(bodyParts, "--data-raw "+ShellQuote(text))
	}
	hasBody := len(bodyParts) > 0

	// AC-CEXP-2: `curl`, the method flag, the URL, then headers and body flags, each on its own line.
	out := []string{"curl"}
	switch {
	case (method == "GET" && !hasBody) || (method == "POST" && hasBody):
		// implied by curl
	case method == "HEAD" && !hasBody:
		out = append(out, "--head")
	default:
		out = append(out, "-X "+method)
	}
	out = append(out, ShellQuote(url))
	if globRe.MatchString(url) {
		out = append(out, "--globoff")
	}
	for _, h := range headers {
		if h.v == "" {
			out = append(out, "-H "+ShellQuote(h.k+";"))
		} else {
			out = append(out, "-H "+ShellQuote(h.k+": "+h.v))
		}
	}
	if hasBasic {
		out = append(out, "-u "+ShellQuote(basic))
	}
	out = append(out, bodyParts...)
	if req.Settings != nil && !req.Settings.VerifyTLS {
		out = append(out, "-k")
	}
	if rerr != nil {
		return "", rerr
	}
	return strings.Join(out, LineJoin), nil
}
