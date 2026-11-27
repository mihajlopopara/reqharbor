package curlexport

// Port of worker/test/curl.test.ts (06-go-server.md §1.6).

import (
	"errors"
	"reflect"
	"regexp"
	"strings"
	"testing"

	"github.com/mihajlopopara/reqharbor/server/internal/jsonx"
)

// shellWords is a minimal POSIX sh word splitter (single quotes, double quotes, backslash and
// backslash-newline continuation), enough to prove the output parses back to the intended argv.
func shellWords(t *testing.T, cmd string) []string {
	t.Helper()
	var words []string
	var cur strings.Builder
	inWord := false
	for i := 0; i < len(cmd); i++ {
		c := cmd[i]
		switch {
		case c == '\'':
			end := strings.IndexByte(cmd[i+1:], '\'')
			if end < 0 {
				t.Fatalf("unterminated single quote in %q", cmd)
			}
			cur.WriteString(cmd[i+1 : i+1+end])
			inWord = true
			i += end + 1
		case c == '\\':
			i++
			if i < len(cmd) && cmd[i] == '\n' {
				continue // line continuation
			}
			if i < len(cmd) {
				cur.WriteByte(cmd[i])
			}
			inWord = true
		case c == ' ' || c == '\n' || c == '\t':
			if inWord {
				words = append(words, cur.String())
			}
			cur.Reset()
			inWord = false
		default:
			cur.WriteByte(c)
			inWord = true
		}
	}
	if inWord {
		words = append(words, cur.String())
	}
	return words
}

type reqOpt func(*Request)

func base(opts ...reqOpt) *Request {
	r := &Request{Method: "GET", URL: "https://api.example.com/users", Auth: Auth{Type: "none"}, Body: Body{Type: "none"}}
	for _, o := range opts {
		o(r)
	}
	return r
}

func method(m string) reqOpt            { return func(r *Request) { r.Method = m } }
func url(u string) reqOpt               { return func(r *Request) { r.URL = u } }
func headers(h ...KeyValue) reqOpt      { return func(r *Request) { r.Headers = h } }
func body(b Body) reqOpt                { return func(r *Request) { r.Body = b } }
func auth(a Auth) reqOpt                { return func(r *Request) { r.Auth = a } }
func kv(k, v string) KeyValue           { return KeyValue{Key: k, Value: v, Enabled: true} }
func off(k, v string) KeyValue          { return KeyValue{Key: k, Value: v} }
func text(typ, s string) Body           { return Body{Type: typ, Text: s} }
func jsonText(s string) *jsonx.Value    { return jsonx.NewString(s) }
func noTLS() reqOpt                     { return func(r *Request) { r.Settings = &Settings{VerifyTLS: false} } }
func withTLS() reqOpt                   { return func(r *Request) { r.Settings = &Settings{VerifyTLS: true} } }
func opts(vars Vars, keep bool) Options { return Options{Variables: vars, KeepVariables: keep} }

func mustCurl(t *testing.T, r *Request, o ...Options) string {
	t.Helper()
	var op Options
	if len(o) > 0 {
		op = o[0]
	}
	out, err := ToCurl(r, op)
	if err != nil {
		t.Fatalf("ToCurl: %v", err)
	}
	return out
}

func lines(parts ...string) string { return strings.Join(parts, LineJoin) }

func eq(t *testing.T, got, want any) {
	t.Helper()
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("\n got: %#v\nwant: %#v", got, want)
	}
}

func contains(t *testing.T, s, sub string) {
	t.Helper()
	if !strings.Contains(s, sub) {
		t.Fatalf("%q does not contain %q", s, sub)
	}
}

func notContains(t *testing.T, s, sub string) {
	t.Helper()
	if strings.Contains(s, sub) {
		t.Fatalf("%q contains %q", s, sub)
	}
}

func TestCurl_AC_CEXP_2_GETOmitsXAndJoinsLines(t *testing.T) {
	out := mustCurl(t, base(headers(kv("Accept", "application/json"), off("X-Disabled", "no"), kv("X-Trace", "1"))))
	eq(t, out, lines("curl", "'https://api.example.com/users'", "-H 'Accept: application/json'", "-H 'X-Trace: 1'"))
	eq(t, LineJoin, " \\\n  ")
}

func TestCurl_AC_CEXP_2_POSTWithBodyOmitsX(t *testing.T) {
	withBody := mustCurl(t, base(method("POST"), body(text("text", "hi"))))
	notContains(t, withBody, "-X")
	eq(t, strings.Split(withBody, LineJoin), []string{"curl", "'https://api.example.com/users'", "-H 'Content-Type: text/plain'", "--data-raw 'hi'"})
	eq(t, mustCurl(t, base(method("POST"))), lines("curl", "-X POST", "'https://api.example.com/users'"))
	eq(t, strings.Split(mustCurl(t, base(method("PUT"), body(text("json", "{}")))), LineJoin)[1], "-X PUT")
	// A GET that carries a body must keep -X GET, or curl would send POST.
	contains(t, mustCurl(t, base(body(text("text", "x")))), "-X GET")
}

func TestCurl_AC_QRY_5_QUERYEmitsXQueryAndTheBody(t *testing.T) {
	out := mustCurl(t, base(method("QUERY"), body(text("json", `{"q":"ada"}`))))
	eq(t, strings.Split(out, LineJoin), []string{"curl", "-X QUERY", "'https://api.example.com/users'", "-H 'Content-Type: application/json'", `--data-raw '{"q":"ada"}'`})
}

func TestCurl_AC_CEXP_3_EverythingSingleQuotedAndParsesBack(t *testing.T) {
	eq(t, ShellQuote("it's"), `'it'\''s'`)
	nasty := "O'Brien \"quoted\" $HOME `whoami` $(rm -rf /) \\ ; | & > < * ? ~ !\nnew line\ttab"
	out := mustCurl(t, base(method("POST"), url("https://x.test/?q="+nasty), headers(kv("X-Nasty", nasty)), body(text("text", nasty))))
	eq(t, shellWords(t, out), []string{"curl", "https://x.test/?q=" + nasty, "-H", "X-Nasty: " + nasty, "-H", "Content-Type: text/plain", "--data-raw", nasty})
	// Shell metacharacters only ever appear inside single quotes.
	outside := regexp.MustCompile(`'[^']*'`).ReplaceAllString(strings.ReplaceAll(out, `'\''`, "Q"), "")
	if regexp.MustCompile("[$`;|&<>()\"]").MatchString(outside) {
		t.Fatalf("metacharacter outside quotes: %q", outside)
	}
}

func TestCurl_AC_CEXP_4_TextBodiesUseDataRaw(t *testing.T) {
	xml := mustCurl(t, base(method("POST"), body(text("xml", "<a/>"))))
	contains(t, xml, "--data-raw '<a/>'")
	contains(t, xml, "-H 'Content-Type: application/xml'")
	gql := mustCurl(t, base(method("POST"), body(Body{Type: "graphql", GraphQL: GraphQL{Query: "{ me { id } }", Variables: jsonText(`{"a": 1}`)}})))
	words := shellWords(t, gql)
	for i, w := range words {
		if w == "--data-raw" {
			eq(t, words[i+1], `{"query":"{ me { id } }","variables":{"a":1}}`)
		}
	}
	contains(t, gql, "-H 'Content-Type: application/json'")
}

func TestCurl_AC_CEXP_4_ExplicitContentTypeIsNotDuplicated(t *testing.T) {
	out := mustCurl(t, base(method("POST"), headers(kv("content-type", "application/vnd.api+json")), body(text("json", "{}"))))
	eq(t, len(regexp.MustCompile(`(?i)content-type`).FindAllString(out, -1)), 1)
}

func TestCurl_AC_CEXP_4_FormEncodeUsesDataURLEncodePerField(t *testing.T) {
	out := mustCurl(t, base(method("POST"), body(Body{Type: "formEncoded", FormEncoded: []KeyValue{kv("a", "1 2"), kv("b", "x&y"), off("c", "off")}})))
	parts := strings.Split(out, LineJoin)
	eq(t, parts[len(parts)-2:], []string{"--data-urlencode 'a=1 2'", "--data-urlencode 'b=x&y'"})
	notContains(t, out, "Content-Type")
}

func TestCurl_AC_CEXP_4_FormUsesFAndFormString(t *testing.T) {
	out := mustCurl(t, base(method("POST"), body(Body{Type: "form", Form: []FormField{
		{Key: "name", Value: "Ada", Enabled: true, Kind: "text"},
		{Key: "avatar", Value: "/tmp/me.png", Enabled: true, Kind: "file", ContentType: "image/png"},
		{Key: "handle", Value: "@ada", Enabled: true, Kind: "text"},
		{Key: "skip", Value: "x", Kind: "text"},
	}})))
	eq(t, strings.Split(out, LineJoin)[2:], []string{"-F 'name=Ada'", "-F 'avatar=@/tmp/me.png;type=image/png'", "--form-string 'handle=@ada'"})
}

func TestCurl_AC_CEXP_4_BinaryUsesDataBinary(t *testing.T) {
	out := mustCurl(t, base(method("POST"), body(Body{Type: "binary", BinaryPath: "/data/it's.bin"})))
	contains(t, out, `--data-binary '@/data/it'\''s.bin'`)
	contains(t, out, "-H 'Content-Type: application/octet-stream'")
}

func TestCurl_AC_CEXP_4_AuthSchemesAndInsecureTLS(t *testing.T) {
	contains(t, mustCurl(t, base(auth(Auth{Type: "basic", Username: "ada", Password: "p:w'd"}))), `-u 'ada:p:w'\''d'`)
	contains(t, mustCurl(t, base(auth(Auth{Type: "bearer", Token: "abc"}))), "-H 'Authorization: Bearer abc'")
	contains(t, mustCurl(t, base(auth(Auth{Type: "apikey", APIKey: APIKey{Key: "X-Key", Value: "k1", In: "header"}}))), "-H 'X-Key: k1'")
	q := mustCurl(t, base(url("https://a.test/x?y=1#frag"), auth(Auth{Type: "apikey", APIKey: APIKey{Key: "api key", Value: "a&b", In: "query"}})))
	contains(t, q, "'https://a.test/x?y=1&api%20key=a%26b#frag'")
	if !strings.Contains(strings.Join(strings.Split(mustCurl(t, base(noTLS())), LineJoin), "|"), "|-k") {
		t.Fatal("verifyTls=false must add -k")
	}
	notContains(t, mustCurl(t, base()), "-k")
	notContains(t, mustCurl(t, base(withTLS())), "-k")
}

func TestCurl_AC_REQ_7_ExplicitAuthorizationWins(t *testing.T) {
	out := mustCurl(t, base(headers(kv("Authorization", "Token xyz")), auth(Auth{Type: "bearer", Token: "abc"})))
	contains(t, out, "-H 'Authorization: Token xyz'")
	notContains(t, out, "Bearer abc")
	basic := mustCurl(t, base(headers(kv("authorization", "x")), auth(Auth{Type: "basic", Username: "u"})))
	notContains(t, basic, "-u")
}

func TestCurl_AuthInheritUsesTheCollectionAuth(t *testing.T) {
	out := mustCurl(t, base(auth(Auth{Type: "inherit"})), Options{InheritedAuth: &Auth{Type: "bearer", Token: "{{token}}"}})
	contains(t, out, "-H 'Authorization: Bearer {{token}}'")
	notContains(t, mustCurl(t, base(auth(Auth{Type: "inherit"}))), "Authorization")
}

func TestCurl_AC_CURL_4_EmptyHeaderValueExportsAsNameSemicolon(t *testing.T) {
	contains(t, mustCurl(t, base(headers(kv("X-Empty", "")))), "-H 'X-Empty;'")
}

func TestCurl_AC_CEXP_5_VariablesResolveByDefaultKeepExportsRaw(t *testing.T) {
	vars := VariableMap([]Variable{
		{Key: "baseUrl", Value: "https://staging.example.com", Enabled: true},
		{Key: "token", Value: "t0k", Enabled: true},
		{Key: "secret", Value: "never", Enabled: true, Secret: true},
	})
	req := base(url("{{baseUrl}}/users"), headers(kv("X-S", "{{secret}}")), auth(Auth{Type: "bearer", Token: "{{ token }}"}))
	resolved := mustCurl(t, req, opts(vars, false))
	contains(t, resolved, "'https://staging.example.com/users'")
	contains(t, resolved, "Bearer t0k")
	contains(t, resolved, "-H 'X-S: {{secret}}'") // secret variables are never resolved into shares
	raw := mustCurl(t, req, opts(vars, true))
	contains(t, raw, "'{{baseUrl}}/users'")
	contains(t, raw, "Bearer {{ token }}")
}

func TestCurl_AC_REQ_10_DefaultScheme(t *testing.T) {
	for in, want := range map[string]string{
		"localhost:3000/x":  "http://localhost:3000/x",
		"127.0.0.1/x":       "http://127.0.0.1/x",
		"[::1]:8080":        "http://[::1]:8080",
		"api.example.com/x": "https://api.example.com/x",
		"http://a.test":     "http://a.test",
		"{{baseUrl}}/x":     "{{baseUrl}}/x",
		"//LOCALHOST/x":     "http://LOCALHOST/x",
	} {
		eq(t, WithDefaultScheme(in), want)
	}
}

func TestCurl_CustomMethodsVerbatimInvalidMethodsAndEmptyURLsAreErrors(t *testing.T) {
	contains(t, mustCurl(t, base(method("PURGE"))), "-X PURGE")
	var ee *ExportError
	if _, err := ToCurl(base(method("BAD;X")), Options{}); !errors.As(err, &ee) {
		t.Fatalf("BAD;X: %v", err)
	}
	if _, err := ToCurl(base(method("get")), Options{}); err == nil || !strings.Contains(err.Error(), `Invalid method "get"`) {
		t.Fatalf("get: %v", err)
	}
	if _, err := ToCurl(base(url("  ")), Options{}); err == nil || err.Error() != "URL is empty" {
		t.Fatalf("empty url: %v", err)
	}
}

func TestVariables_AC_ENV_1_TrimsNamesUnknownStayLiteral(t *testing.T) {
	eq(t, ResolveVariables("{{ a }}-{{a}}-{{missing}}", Vars{"a": "1"}), "1-1-{{missing}}")
}

func TestVariables_AC_ENV_2_NestedDepthCyclesAndDisabled(t *testing.T) {
	m := VariableMap([]Variable{
		{Key: "a", Value: "{{b}}", Enabled: true},
		{Key: "b", Value: "{{c}}", Enabled: true},
		{Key: "c", Value: "{{d}}", Enabled: true},
		{Key: "d", Value: "deep", Enabled: true},
		{Key: "x", Value: "{{y}}", Enabled: true},
		{Key: "y", Value: "{{x}}", Enabled: true},
		{Key: "off", Value: "nope"},
	})
	eq(t, ResolveVariables("{{c}}", m), "deep")
	eq(t, ResolveVariables("{{a}}", m), "deep")
	deeper := Vars{"e": "too deep"}
	for k, v := range m {
		deeper[k] = v
	}
	deeper["d"] = "{{e}}"
	eq(t, ResolveVariables("{{a}}", deeper), "{{e}}")
	eq(t, ResolveVariables("{{x}}", m), "{{x}}")
	eq(t, ResolveVariables("{{off}}", m), "{{off}}")
}

func TestVariables_VariableMapFirstWinsAndUnclosedBracesStop(t *testing.T) {
	m := VariableMap([]Variable{{Key: "k", Value: "1", Enabled: true}, {Key: "k", Value: "2", Enabled: true}})
	eq(t, m["k"], "1")
	eq(t, ResolveVariables("{{bad name}} {{k}}", m), "{{bad name}} 1")
	eq(t, ResolveVariables("{{k", m), "{{k")
	// Rust trim keeps U+FEFF, so it is part of the (then invalid) name.
	eq(t, ResolveVariables("{{\ufeffk}}", m), "{{\ufeffk}}")
	eq(t, ResolveVariables("{{ k　}}", m), "1")
}
