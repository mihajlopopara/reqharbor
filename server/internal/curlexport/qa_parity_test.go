package curlexport

// Port of the parity half of worker/test/qa_parity_security.test.ts. The BUG QA-R1-xxx cases
// assert the fixed behaviour, as the Worker tests do today.

import (
	"slices"
	"testing"
)

func qaBase(opts ...reqOpt) *Request {
	r := base(opts...)
	r.URL = "https://api.example.com/items"
	for _, o := range opts {
		o(r)
	}
	return r
}

func TestQA_R1_006a_BracketsAndBracesNeedGloboff(t *testing.T) {
	withBrackets := mustCurl(t, qaBase(url("https://api.example.com/items?ids[]=1&ids[]=2")))
	raw := mustCurl(t, qaBase(url("{{baseUrl}}/items")), Options{KeepVariables: true})
	if !slices.Contains(shellWords(t, withBrackets), "--globoff") || !slices.Contains(shellWords(t, raw), "--globoff") {
		t.Fatalf("missing --globoff:\n%s\n%s", withBrackets, raw)
	}
}

func TestQA_R1_006b_HEADExportsAsHeadNotXHEAD(t *testing.T) {
	notContains(t, mustCurl(t, qaBase(method("HEAD"))), "-X HEAD")
	contains(t, mustCurl(t, qaBase(method("HEAD"))), "--head")
}

func TestQA_R1_006c_FormFilePathsWithSemicolonOrCommaAreQuoted(t *testing.T) {
	cmd := mustCurl(t, qaBase(method("POST"), body(Body{Type: "form", Form: []FormField{{Key: "doc", Value: "/tmp/a;b,c.txt", Enabled: true, Kind: "file"}}})))
	if !slices.Contains(shellWords(t, cmd), `doc=@"/tmp/a;b,c.txt"`) {
		t.Fatal(cmd)
	}
}

func TestQA_R1_006d_FormTextWithSemicolonOrLeadingQuoteUsesFormString(t *testing.T) {
	cmd := mustCurl(t, qaBase(method("POST"), body(Body{Type: "form", Form: []FormField{
		{Key: "a", Value: "x;type=text/html", Enabled: true, Kind: "text"},
		{Key: "b", Value: `"quoted"`, Enabled: true, Kind: "text"},
	}})))
	w := shellWords(t, cmd)
	eq(t, w[slices.Index(w, "a=x;type=text/html")-1], "--form-string")
	eq(t, w[slices.Index(w, `b="quoted"`)-1], "--form-string")
}

func TestQA_R1_006e_FormEncodeKeysArePercentEncoded(t *testing.T) {
	cmd := mustCurl(t, qaBase(method("POST"), body(Body{Type: "formEncoded", FormEncoded: []KeyValue{kv("a=b&c", "v")}})))
	if !slices.Contains(shellWords(t, cmd), "a%3Db%26c=v") {
		t.Fatal(cmd)
	}
}

func TestQA_R1_009_VariableDepthMatchesTheCore(t *testing.T) {
	eq(t, ResolveVariables("{{a}}", Vars{"a": "{{b}}", "b": "{{c}}", "c": "{{d}}", "d": "x"}), "x")
}

func TestQA_R1_009b_ACycleLeavesTheTopLevelReferenceLiteral(t *testing.T) {
	eq(t, ResolveVariables("{{a}}", Vars{"a": "x{{b}}", "b": "y{{a}}"}), "{{a}}")
}

func TestQA_ParityOK_QueryCustomMethodsBasicInsecure(t *testing.T) {
	cmd := mustCurl(t, qaBase(method("QUERY"), auth(Auth{Type: "basic", Username: "u", Password: "p"}), body(text("json", `{"q":1}`)), noTLS()))
	w := shellWords(t, cmd)
	for _, want := range []string{"-X", "QUERY", "-u", "u:p", "-k", "--data-raw", `{"q":1}`} {
		if !slices.Contains(w, want) {
			t.Fatalf("%q missing from %q", want, w)
		}
	}
}
