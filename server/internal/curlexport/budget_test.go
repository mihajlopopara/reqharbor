package curlexport

// W11 (06-go-server.md §1.3): variable expansion is budgeted, and findReference is linear, so a
// public share page view can never stall or OOM the server.

import (
	"errors"
	"math/rand"
	"strings"
	"testing"
	"time"
)

// cubicVars is the QA reproduction: a={{b}}xN, b={{c}}xN, c={{d}}xN, d=leaf. "{{a}}" expands to
// N^3 copies of leaf (N=2000: 8e9 bytes for leaf "x").
func cubicVars(n int, leaf string) Vars {
	return Vars{
		"a": strings.Repeat("{{b}}", n),
		"b": strings.Repeat("{{c}}", n),
		"c": strings.Repeat("{{d}}", n),
		"d": leaf,
	}
}

// quick fails when fn takes longer than limit; the unbudgeted resolver needed minutes for these.
func quick(t *testing.T, limit time.Duration, fn func()) {
	t.Helper()
	start := time.Now()
	fn()
	if d := time.Since(start); d > limit {
		t.Fatalf("took %v, want under %v", d, limit)
	}
}

func isTooLarge(err error) bool {
	var ee *ExportError
	return errors.As(err, &ee) && ee.Msg == ErrTooLarge
}

func TestVariables_W11_NestedExpansionStopsAtTheBudget(t *testing.T) {
	for _, leaf := range []string{"x", ""} { // "" expands to nothing: the work is still charged
		vars := cubicVars(2000, leaf)
		quick(t, 2*time.Second, func() {
			b := NewBudget(MaxExpansionBytes)
			out, err := ResolveVariablesBudget("{{a}}", vars, b)
			if !isTooLarge(err) || out != "" {
				t.Fatalf("leaf %q: got %d bytes, %v", leaf, len(out), err)
			}
			// The convenience wrapper treats an overflow like a cycle: the text stays literal.
			eq(t, ResolveVariables("{{a}}", vars), "{{a}}")
		})
	}
}

func TestCurl_W11_OversizedExpansionIsAnExportError(t *testing.T) {
	vars := cubicVars(2000, "x")
	hs := []KeyValue{kv("X-A", "{{a}}"), kv("X-B", "{{a}}"), kv("X-C", "{{a}}")}
	quick(t, 2*time.Second, func() {
		got, err := ToCurl(base(url("https://h/{{a}}"), headers(hs...)), Options{Variables: vars})
		if !isTooLarge(err) || got != "" {
			t.Fatalf("got %d bytes, %v", len(got), err)
		}
		// Keep {{variables}} never expands, so it still exports.
		raw := mustCurl(t, base(url("https://h/{{a}}"), headers(hs...)), Options{Variables: vars, KeepVariables: true})
		contains(t, raw, "https://h/{{a}}")
	})
}

func TestCurl_W11_OverflowInAnyFieldFailsTheExport(t *testing.T) {
	vars := cubicVars(400, "x")
	for name, r := range map[string]*Request{
		"basic auth":   base(auth(Auth{Type: "basic", Username: "{{a}}"})),
		"form-encoded": base(method("POST"), body(Body{Type: "formEncoded", FormEncoded: []KeyValue{kv("k", "{{a}}")}})),
		"graphql":      base(method("POST"), body(Body{Type: "graphql", GraphQL: GraphQL{Query: "{{a}}", Variables: jsonText("{{a}}")}})),
		"body":         base(method("POST"), body(text("json", "{{a}}"))),
	} {
		if _, err := ToCurl(r, Options{Variables: vars}); !isTooLarge(err) {
			t.Errorf("%s: %v", name, err)
		}
	}
}

func TestVariables_W11_LegitimateLargeValuesStillResolve(t *testing.T) {
	big := strings.Repeat("v", 300_000)
	vars := Vars{"big": big, "alias": "{{big}}", "k": "1"}
	// 600 KB of literal text plus 300 KB substituted twice: 1.2 MB of output, 600 KB charged.
	got, err := ResolveVariablesBudget(strings.Repeat("L", 600_000)+"{{big}}{{big}}{{k}}", vars, NewBudget(MaxExpansionBytes))
	if err != nil || len(got) != 1_200_001 {
		t.Fatalf("%d %v", len(got), err)
	}
	// Literal text costs nothing; only substitutions do.
	b := NewBudget(MaxExpansionBytes)
	if _, err := ResolveVariablesBudget(strings.Repeat("{{nope}}", 200_000), vars, b); err != nil || b.Remaining() != MaxExpansionBytes {
		t.Fatalf("%v %d", err, b.Remaining())
	}
	// The alias costs its reference plus its value at each level.
	b = NewBudget(MaxExpansionBytes)
	if _, err := ResolveVariablesBudget("{{alias}}", vars, b); err != nil || MaxExpansionBytes-b.Remaining() != len("{{alias}}{{big}}")+len("{{big}}")+len(big) {
		t.Fatalf("%v spent %d", err, MaxExpansionBytes-b.Remaining())
	}
	// Four substitutions of 300 KB exceed 1 MiB.
	if _, err := ResolveVariablesBudget("{{big}}{{big}}{{big}}{{alias}}", vars, NewBudget(MaxExpansionBytes)); !isTooLarge(err) {
		t.Fatalf("%v", err)
	}
}

func TestCurl_W11_SharedBudgetSpansExports(t *testing.T) {
	vars := Vars{"v": strings.Repeat("v", 400_000)}
	shared := NewBudget(1_000_000)
	r := base(url("https://h/{{v}}"))
	mustCurl(t, r, Options{Variables: vars, Budget: shared})
	mustCurl(t, r, Options{Variables: vars, Budget: shared})
	eq(t, shared.Remaining(), 1_000_000-2*(400_000+len("{{v}}")))
	if _, err := ToCurl(r, Options{Variables: vars, Budget: shared}); !isTooLarge(err) {
		t.Fatalf("third export: %v", err)
	}
	// A failed export charges only what it spent before stopping.
	eq(t, shared.Remaining(), 1_000_000-2*(400_000+len("{{v}}")))
	// Exports that resolve nothing never touch the budget.
	mustCurl(t, base(url("https://h/{{v}}")), Options{Variables: vars, Budget: shared, KeepVariables: true})
	eq(t, shared.Remaining(), 1_000_000-2*(400_000+len("{{v}}")))
}

// findReferenceCore is the core's candidate-by-candidate scan (vars::find_reference), kept to
// prove the linear findReference returns the same reference.
func findReferenceCore(text string) (found, bool) {
	from := 0
	for {
		i := strings.Index(text[from:], "{{")
		if i < 0 {
			return found{}, false
		}
		start := from + i
		innerStart := start + 2
		c := strings.Index(text[innerStart:], "}}")
		if c < 0 {
			return found{}, false
		}
		closeAt := innerStart + c
		name := RustTrim(text[innerStart:closeAt])
		if name != "" && nameRe.MatchString(name) {
			return found{start, closeAt + 2, name}, true
		}
		from = start + 1
	}
}

func TestVariables_W11_FindReferenceMatchesTheCoreScan(t *testing.T) {
	pieces := []string{"{", "}", "{{", "}}", " ", "\u00a0", "\ufeff", "a", "b.c", "-", "_", "x y", "{{a}}", "{{ b }}", "}{"}
	rng := rand.New(rand.NewSource(11))
	for i := 0; i < 200_000; i++ {
		var sb strings.Builder
		for n := rng.Intn(12); n > 0; n-- {
			sb.WriteString(pieces[rng.Intn(len(pieces))])
		}
		s := sb.String()
		gf, gok := findReference(s)
		wf, wok := findReferenceCore(s)
		if gf != wf || gok != wok {
			t.Fatalf("%q: got %+v %v, core %+v %v", s, gf, gok, wf, wok)
		}
	}
}

func TestVariables_W11_FindReferenceIsLinear(t *testing.T) {
	vars := Vars{"k": "1"}
	for name, text := range map[string]string{
		"open braces":         strings.Repeat("{{ ", 300_000) + "}}{{k}}",
		"braces then spaces":  strings.Repeat("{", 500_000) + strings.Repeat(" ", 500_000) + "}}{{k}}",
		"many closes, braces": strings.Repeat("{{{ }}", 150_000) + "{{k}}",
	} {
		quick(t, 2*time.Second, func() {
			got := ResolveVariables(text, vars)
			if !strings.HasSuffix(got, "1") || len(got) != len(text)-len("{{k}}")+1 {
				t.Fatalf("%s: wrong result (%d bytes)", name, len(got))
			}
		})
	}
}

func TestVariables_W11_BudgetKeepsCycleAndDepthRules(t *testing.T) {
	// Overflow is checked before a cycle or depth rule would discard the work, so a budget
	// large enough never changes the result.
	m := Vars{"a": "x{{b}}", "b": "y{{a}}", "c": "{{d}}", "d": "{{e}}", "e": "{{f}}", "f": "{{g}}", "g": "deep"}
	for _, in := range []string{"{{a}}", "{{c}}", "{{b}}-{{c}}-{{zz}}"} {
		got, err := ResolveVariablesBudget(in, m, NewBudget(MaxExpansionBytes))
		if err != nil || got != ResolveVariables(in, m) {
			t.Fatalf("%q: %q %v", in, got, err)
		}
	}
	eq(t, ResolveVariables("{{c}}", m), "{{g}}")
}
