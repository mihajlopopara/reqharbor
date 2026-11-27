package curlexport

import (
	"regexp"
	"slices"
	"strings"
	"unicode"
)

// `{{variable}}` resolution (AC-ENV-1, AC-ENV-2): port of worker/src/variables.ts, itself an
// exact port of crates/core/src/vars.rs (QA-R1-009). Shared by the exporter and the share page.

// jsSpace is JavaScript's \s, which Go's regexp \s (ASCII only) is not.
const jsSpace = `[\t\n\v\f\r \x{a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}]`

// VarRe is `{{ name }}`, for highlighting only; resolution uses findReference.
var VarRe = regexp.MustCompile(`\{\{` + jsSpace + `*([A-Za-z0-9_.-]+)` + jsSpace + `*\}\}`)

// maxDepth is the nesting depth for variables referencing variables (AC-ENV-2), as in the core.
const maxDepth = 3

// Vars maps variable names to values.
type Vars map[string]string

// RustTrim is Rust's str::trim: Unicode White_Space, which is exactly unicode.IsSpace (unlike
// JavaScript's trim, it keeps U+FEFF).
func RustTrim(s string) string { return strings.TrimFunc(s, unicode.IsSpace) }

var nameRe = regexp.MustCompile(`^[A-Za-z0-9_.-]+$`)

type found struct {
	start, end int
	name       string
}

// findReference is vars::find_reference: the first `{{ name }}` with a valid name; it stops at
// the first `{{` that has no `}}` after it.
//
// It returns what the core's candidate-by-candidate scan returns, in linear time: a candidate
// whose name would contain a `{` can never be valid (names are [A-Za-z0-9_.-] after the trim),
// so the scan jumps straight to the last `{{` before the same `}}`. The core's scan is quadratic
// on text such as "{{ {{ {{ … }}", which only matters on the public share page (W11).
func findReference(text string) (found, bool) {
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
		inner := text[innerStart:closeAt]
		if lb := strings.LastIndexByte(inner, '{'); lb >= 0 {
			// Every `{{` that starts before innerStart+lb-1 shares this `}}` and has a `{` in its
			// name. The one at innerStart+lb-1 (if any) does not contain that `{`.
			from = innerStart + lb - 1
			continue
		}
		name := RustTrim(inner)
		if name != "" && nameRe.MatchString(name) {
			return found{start, closeAt + 2, name}, true
		}
		from = start + 1
	}
}

// MaxExpansionBytes bounds the variable expansion of one exported request (W11): the bytes of
// every variable value substituted, at every nesting level, plus the references they replace.
// A legitimate request is far below it (a share snapshot is at most 1 MB); nested references can
// otherwise grow as N^3 (a={{b}}xN, b={{c}}xN, …) and stall the whole server on one page view.
const MaxExpansionBytes = 1 << 20

// ErrTooLarge is the export error for a request whose expansion exceeds its Budget.
const ErrTooLarge = "Resolved request is too large to export"

// Budget is the expansion work left, shared by every resolution it is passed to.
type Budget struct{ left int }

// NewBudget returns a budget of n bytes.
func NewBudget(n int) *Budget { return &Budget{left: n} }

// Remaining is the budget left, in bytes.
func (b *Budget) Remaining() int { return b.left }

func (b *Budget) spend(n int) bool {
	if n > b.left {
		return false
	}
	b.left -= n
	return true
}

// ResolveVariables replaces known variables like the core: nested references resolve while
// depth <= 3 (deeper ones stay literal); a cycle anywhere below a top-level reference leaves that
// top-level reference literal. Unknown names stay literal. It spends at most MaxExpansionBytes;
// past that, like a cycle, it returns text unchanged. Exports use ResolveVariablesBudget.
func ResolveVariables(text string, vars Vars) string {
	out, err := ResolveVariablesBudget(text, vars, NewBudget(MaxExpansionBytes))
	if err != nil {
		return text
	}
	return out
}

// ResolveVariablesBudget is ResolveVariables charged to b: each substituted value costs its
// length plus the reference's, at every depth, before it is expanded. The output is therefore at
// most len(text) plus what was spent, and the work is linear in both. Once b runs out it stops
// and returns an *ExportError (ErrTooLarge).
func ResolveVariablesBudget(text string, vars Vars, b *Budget) (string, error) {
	errBudget := &ExportError{ErrTooLarge}
	var inner func(s string, stack []string, depth int) (string, bool, error)
	inner = func(s string, stack []string, depth int) (string, bool, error) {
		if !strings.Contains(s, "{{") {
			return s, true, nil
		}
		var out strings.Builder
		rest := s
		for {
			f, ok := findReference(rest)
			if !ok {
				break
			}
			out.WriteString(rest[:f.start])
			literal := rest[f.start:f.end]
			value, known := vars[f.name]
			switch {
			case !known:
				out.WriteString(literal)
			case slices.Contains(stack, f.name):
				return "", false, nil
			case depth > maxDepth:
				out.WriteString(literal)
			default:
				if !b.spend(len(literal) + len(value)) {
					return "", false, errBudget
				}
				resolved, ok, err := inner(value, append(stack, f.name), depth+1)
				switch {
				case err != nil:
					return "", false, err
				case ok:
					out.WriteString(resolved)
				case depth == 0:
					out.WriteString(literal)
				default:
					return "", false, nil
				}
			}
			rest = rest[f.end:]
		}
		out.WriteString(rest)
		return out.String(), true, nil
	}
	r, ok, err := inner(text, nil, 0)
	switch {
	case err != nil:
		return "", err
	case ok:
		return r, nil
	}
	return text, nil
}

// Variable is an environment variable as the share snapshot carries it.
type Variable struct {
	Key, Value      string
	Enabled, Secret bool
}

// VariableMap builds a lookup from environment variables: disabled and secret ones are skipped;
// the first of a duplicated key wins.
func VariableMap(vars []Variable) Vars {
	m := Vars{}
	for _, v := range vars {
		if !v.Enabled || v.Secret {
			continue
		}
		if _, dup := m[v.Key]; dup {
			continue
		}
		m[v.Key] = v.Value
	}
	return m
}
