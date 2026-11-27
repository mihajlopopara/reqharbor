package curlexport_test

// Port of worker/test/curl_parity.test.ts (QA-R1-006): the share page's cURL must match the Rust
// exporter byte for byte. Every case in curl_parity.json carries `expected` produced by the real
// Rust exporter. Requests take the share page's own path: snapshot normalisation, then ToCurl.

import (
	"errors"
	"os"
	"strings"
	"testing"

	"github.com/mihajlopopara/reqharbor/server/internal/curlexport"
	"github.com/mihajlopopara/reqharbor/server/internal/jsonx"
	"github.com/mihajlopopara/reqharbor/server/internal/sharepage"
)

// fixturePath is the shared fixture; S8 moves it to testdata/ (06-go-server.md §1.6, §10).
const fixturePath = "../../../worker/test/fixtures/curl_parity.json"

func loadCases(t *testing.T) []*jsonx.Value {
	t.Helper()
	raw, err := os.ReadFile(fixturePath)
	if err != nil {
		t.Fatalf("read fixture: %v", err)
	}
	doc, err := jsonx.Parse(raw)
	if err != nil {
		t.Fatalf("parse fixture: %v", err)
	}
	return doc.Get("cases").Arr
}

func TestCurlParity_FixtureHasExpectationsForEveryCase(t *testing.T) {
	cases := loadCases(t)
	if len(cases) != 39 {
		t.Fatalf("fixture has %d cases, want 39", len(cases))
	}
	for _, c := range cases {
		if !c.Get("expected").IsObject() {
			t.Fatalf("case %s has no expected", c.Get("name").S)
		}
	}
}

func TestCurlParity_QA_R1_006(t *testing.T) {
	// Every case also goes through a shared page budget, as on the share page (W11): budgeting
	// never changes a legitimate export.
	page := curlexport.NewBudget(sharepage.MaxPageExpansionBytes)
	t.Cleanup(func() {
		if page.Remaining() <= 0 || page.Remaining() == sharepage.MaxPageExpansionBytes {
			t.Errorf("page budget left: %d", page.Remaining())
		}
	})
	for _, c := range loadCases(t) {
		name, _ := c.Get("name").Str()
		t.Run(name, func(t *testing.T) {
			snap := &jsonx.Value{Kind: jsonx.Object}
			snap.Set("schemaVersion", &jsonx.Value{Kind: jsonx.Number, S: "1"})
			snap.Set("request", c.Get("request"))
			req := sharepage.NormalizeSnapshot("request", snap).Requests[0]

			var vars []curlexport.Variable
			var rows []*jsonx.Value
			if vs := c.Get("variables"); vs.IsArray() {
				rows = vs.Arr
			}
			for _, v := range rows {
				k, _ := v.Get("key").Str()
				val, _ := v.Get("value").Str()
				vars = append(vars, curlexport.Variable{Key: k, Value: val, Enabled: v.Get("enabled").IsTrue()})
			}
			opts := curlexport.Options{Variables: curlexport.VariableMap(vars), KeepVariables: c.Get("keepVariables").IsTrue(), Budget: page}
			if ca := c.Get("collectionAuth"); ca != nil {
				a := sharepage.NormalizeAuth(ca)
				opts.InheritedAuth = &a
			}
			got, err := curlexport.ToCurl(&req.Request, opts)
			exp := c.Get("expected")
			if want, ok := exp.Get("curl").Str(); ok {
				if err != nil {
					t.Fatalf("unexpected error: %v", err)
				}
				if got != want {
					t.Fatalf("mismatch\n got: %q\nwant: %q", got, want)
				}
				return
			}
			want, _ := exp.Get("error").Str()
			var ee *curlexport.ExportError
			if !errors.As(err, &ee) || !strings.Contains(ee.Msg, want) {
				t.Fatalf("want ExportError containing %q, got %v (output %q)", want, err, got)
			}
		})
	}
}
