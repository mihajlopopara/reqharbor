package jsonx

import (
	"bytes"
	"encoding/json"
	"errors"
	"reflect"
	"strings"
	"testing"
)

func TestCanonicalize_Table(t *testing.T) {
	cases := []struct {
		in, out string
		err     error
	}{
		{`{ "b" : 1 , "a" : [ true, false, null ] }`, `{"b":1,"a":[true,false,null]}`, nil},
		{`{"a":1,"b":2,"a":3}`, `{"a":3,"b":2}`, nil}, // first position, last value
		{`{"n":1e308,"m":-0,"f":1.50,"e":1E+2}`, `{"n":1e308,"m":-0,"f":1.50,"e":1E+2}`, nil},
		{`18446744073709551616`, `18446744073709551616`, nil},
		{`1e-400`, `1e-400`, nil},
		{`1.8e308`, ``, ErrNumberRange},
		{`1e400`, ``, ErrNumberRange},
		{`-1e400`, ``, ErrNumberRange},
		{`"` + esc("0041", "00e9", "20ac", "d83d", "de00") + `"`, `"Aé€😀"`, nil},
		{`"\"\\\/\b\f\n\r\t\u0001\u001f\u007f"`, `"\"\\/\b\f\n\r\t\u0001\u001f` + "\x7f" + `"`, nil},
		{`" <>&"`, "\" <>&\"", nil},
		{`"\uD800"`, ``, ErrLoneSurrogate},
		{`"\uDE00"`, ``, ErrLoneSurrogate},
		{`"` + esc("D83D", "0041") + `"`, ``, ErrLoneSurrogate},
		{`"` + esc("D83D", "D83D", "DE00") + `"`, ``, ErrLoneSurrogate},
		{`"` + esc("D83D", "DE00") + `"`, "\"😀\"", nil},
		{`{"\uD800":1}`, ``, ErrLoneSurrogate},
		{`{`, ``, ErrSyntax},
		{`[1,]`, ``, ErrSyntax},
		{`01`, ``, ErrSyntax},
		{`1.`, ``, ErrSyntax},
		{`.5`, ``, ErrSyntax},
		{`+1`, ``, ErrSyntax},
		{`"a` + "\n" + `"`, ``, ErrSyntax},
		{`"\x"`, ``, ErrSyntax},
		{`"\u12"`, ``, ErrSyntax},
		{`tru`, ``, ErrSyntax},
		{`{} {}`, ``, ErrSyntax},
		{"\ufeff{}", ``, ErrSyntax},
		{"{\"a\":\"\xff\"}", ``, ErrSyntax},
		{" {}", ``, ErrSyntax},
	}
	for _, c := range cases {
		got, err := Canonicalize([]byte(c.in))
		if c.err != nil {
			if !errors.Is(err, c.err) {
				t.Errorf("%q: err %v, want %v", c.in, err, c.err)
			}
			continue
		}
		if err != nil || string(got) != c.out {
			t.Errorf("%q → %q (%v), want %q", c.in, got, err, c.out)
		}
	}
}

// esc builds JSON escapes such as esc("D83D", "DE00"); they are assembled at run time so the
// source holds no literal escape sequences that editors or tools might decode.
func esc(hex ...string) string {
	var b strings.Builder
	for _, h := range hex {
		b.WriteString(`\` + "u" + h)
	}
	return b.String()
}

func nest(n int) string { return strings.Repeat("[", n) + strings.Repeat("]", n) }

func TestCanonicalize_Depth(t *testing.T) {
	if _, err := Canonicalize([]byte(nest(MaxDepth))); err != nil {
		t.Fatalf("depth %d: %v", MaxDepth, err)
	}
	if _, err := Canonicalize([]byte(nest(MaxDepth + 1))); !errors.Is(err, ErrTooDeep) {
		t.Fatalf("depth %d: %v", MaxDepth+1, err)
	}
	// Scalars do not count as a level.
	if _, err := Canonicalize([]byte(strings.Repeat("[", MaxDepth) + "1" + strings.Repeat("]", MaxDepth))); err != nil {
		t.Fatal(err)
	}
	// Absurd nesting is refused by the parser without exhausting the stack.
	if _, err := Parse([]byte(nest(MaxParseDepth + 1))); !errors.Is(err, ErrTooDeep) {
		t.Fatalf("parse depth: %v", err)
	}
	if _, err := Parse([]byte(strings.Repeat(`{"a":`, 200000))); !errors.Is(err, ErrTooDeep) {
		t.Fatalf("parse depth: %v", err)
	}
}

func TestParse_LenientValuesAreJudgedLater(t *testing.T) {
	v, err := Parse([]byte(`{"ok":1,"bad":"\uD800","inf":1e999}`))
	if err != nil {
		t.Fatal(err)
	}
	if err := Validate(v.Get("ok")); err != nil {
		t.Fatal(err)
	}
	if err := Validate(v.Get("bad")); !errors.Is(err, ErrLoneSurrogate) {
		t.Fatal(err)
	}
	if s, _ := v.Get("bad").Str(); s != "�" {
		t.Fatalf("%q", s)
	}
	if err := Validate(v.Get("inf")); !errors.Is(err, ErrNumberRange) {
		t.Fatal(err)
	}
	if v.Get("missing") != nil || v.Get("ok").Get("x") != nil {
		t.Fatal("Get on missing")
	}
}

func TestValue_SetAndShallowCopy(t *testing.T) {
	v, _ := Parse([]byte(`{"a":1,"b":2}`))
	c := v.ShallowCopy()
	c.Set("a", NewString("x"))
	c.Set("z", NewString("y"))
	got, _ := Canonical(c)
	orig, _ := Canonical(v)
	if string(got) != `{"a":"x","b":2,"z":"y"}` || string(orig) != `{"a":1,"b":2}` {
		t.Fatalf("%s %s", got, orig)
	}
	// Large objects switch to an index; behaviour is the same.
	var b strings.Builder
	b.WriteString("{")
	for i := 0; i < 50; i++ {
		if i > 0 {
			b.WriteString(",")
		}
		b.WriteString(`"k` + string(rune('a'+i%26)) + strings.Repeat("x", i/26) + `":` + "1")
	}
	b.WriteString(`,"ka":2}`)
	big, err := Parse([]byte(b.String()))
	if err != nil {
		t.Fatal(err)
	}
	if len(big.Keys) != 50 || big.Get("ka").S != "2" || big.Keys[0] != "ka" {
		t.Fatalf("keys %d first %q", len(big.Keys), big.Keys[0])
	}
}

// TestCanonicalize_ManyDuplicateKeysIsLinear guards against quadratic dedupe.
func TestCanonicalize_ManyKeys(t *testing.T) {
	var b bytes.Buffer
	b.WriteString("{")
	for i := 0; i < 200000; i++ {
		if i > 0 {
			b.WriteByte(',')
		}
		b.WriteString(`"`)
		b.WriteString(strings.Repeat("k", 1+i%7))
		b.WriteByte(byte('a' + i%26))
		b.WriteString(`":1`)
	}
	b.WriteString("}")
	v, err := Parse(b.Bytes())
	if err != nil {
		t.Fatal(err)
	}
	if len(v.Keys) != 182 { // lcm(7, 26) distinct keys
		t.Fatalf("%d keys", len(v.Keys))
	}
}

func FuzzCanonicalize(f *testing.F) {
	for _, s := range []string{`{}`, `{"a":[1,2,{"b":null}]}`, `"é😀"`, `1e308`, `-0.0e-10`, `{"a":1,"a":2}`, `"\u0000"`, `[[[[]]]]`} {
		f.Add([]byte(s))
	}
	f.Fuzz(func(t *testing.T, data []byte) {
		out, err := Canonicalize(data)
		if err != nil {
			return
		}
		// Accepted output is valid JSON that encoding/json reads as an equal value...
		var a, b any
		if err := json.Unmarshal(out, &b); err != nil {
			t.Fatalf("output %q is not JSON: %v", out, err)
		}
		if json.Unmarshal(data, &a) == nil && !reflect.DeepEqual(a, b) {
			// encoding/json keeps the last duplicate too, so values must agree.
			t.Fatalf("value changed: %q → %q", data, out)
		}
		// ...and canonicalisation is idempotent.
		again, err := Canonicalize(out)
		if err != nil || !bytes.Equal(again, out) {
			t.Fatalf("not idempotent: %q → %q → %q (%v)", data, out, again, err)
		}
	})
}
