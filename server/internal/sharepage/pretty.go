package sharepage

import (
	"bytes"
	"math"
	"strconv"

	"github.com/mihajlopopara/reqharbor/server/internal/jsonx"
	"github.com/mihajlopopara/reqharbor/server/internal/jsx"
)

// PrettyJSON is share-page.ts prettyJson: JSON.stringify(JSON.parse(text), null, 2), or the text
// unchanged when it is not JSON.
func PrettyJSON(text string) string {
	v, err := jsonx.Parse([]byte(text))
	if err != nil {
		return text
	}
	return prettyValue(v)
}

// prettyValue is JSON.stringify(v, null, 2): numbers as JavaScript prints them (non-finite ones
// as null), integer-like keys first in ascending order, as JavaScript objects order them.
func prettyValue(v *jsonx.Value) string {
	var b bytes.Buffer
	writePretty(&b, v, "")
	return b.String()
}

// arrayIndex reports whether k is a canonical array index ("0", "42"; not "01" or "4294967295"),
// which JavaScript objects enumerate first in ascending order.
func arrayIndex(k string) (uint64, bool) {
	if k == "" || len(k) > 10 || (len(k) > 1 && k[0] == '0') {
		return 0, false
	}
	n, err := strconv.ParseUint(k, 10, 64)
	if err != nil || n >= math.MaxUint32 {
		return 0, false
	}
	return n, true
}

func jsKeyOrder(v *jsonx.Value) []int {
	order := make([]int, 0, len(v.Keys))
	type idx struct {
		n uint64
		i int
	}
	var ints []idx
	for i, k := range v.Keys {
		if n, ok := arrayIndex(k); ok {
			ints = append(ints, idx{n, i})
		}
	}
	for a := 1; a < len(ints); a++ { // insertion sort: small lists, stable
		for c := a; c > 0 && ints[c].n < ints[c-1].n; c-- {
			ints[c], ints[c-1] = ints[c-1], ints[c]
		}
	}
	for _, x := range ints {
		order = append(order, x.i)
	}
	for i, k := range v.Keys {
		if _, ok := arrayIndex(k); !ok {
			order = append(order, i)
		}
	}
	return order
}

func writePretty(b *bytes.Buffer, v *jsonx.Value, indent string) {
	switch v.Kind {
	case jsonx.Null:
		b.WriteString("null")
	case jsonx.Bool:
		b.WriteString(strconv.FormatBool(v.B))
	case jsonx.Number:
		f, _ := strconv.ParseFloat(v.S, 64)
		if math.IsInf(f, 0) || math.IsNaN(f) {
			b.WriteString("null")
		} else {
			b.WriteString(jsx.FormatNumber(f))
		}
	case jsonx.String:
		jsonx.WriteString(b, v.S)
	case jsonx.Array:
		if len(v.Arr) == 0 {
			b.WriteString("[]")
			return
		}
		inner := indent + "  "
		b.WriteString("[\n")
		for i, it := range v.Arr {
			if i > 0 {
				b.WriteString(",\n")
			}
			b.WriteString(inner)
			writePretty(b, it, inner)
		}
		b.WriteString("\n" + indent + "]")
	case jsonx.Object:
		if len(v.Keys) == 0 {
			b.WriteString("{}")
			return
		}
		inner := indent + "  "
		b.WriteString("{\n")
		for n, i := range jsKeyOrder(v) {
			if n > 0 {
				b.WriteString(",\n")
			}
			b.WriteString(inner)
			jsonx.WriteString(b, v.Keys[i])
			b.WriteString(": ")
			writePretty(b, v.Vals[i], inner)
		}
		b.WriteString("\n" + indent + "}")
	}
}
