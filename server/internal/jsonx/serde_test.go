package jsonx

import (
	"errors"
	"math"
	"strings"
	"testing"
)

func TestSerdeReserialize_MatchesSerdeJSON(t *testing.T) {
	for in, want := range map[string]string{
		`{"z": 1, "a": 2, "z": 3}`:         `{"z":3,"a":2}`,
		`[1.0, -0, 100.50, 1e21, 1e-7]`:    `[1.0,-0.0,100.5,1e+21,1e-7]`,
		`18446744073709551615`:             `18446744073709551615`,
		`18446744073709551616`:             `1.8446744073709552e+19`,
		`-9223372036854775808`:             `-9223372036854775808`,
		`-9223372036854775809`:             `-9.223372036854776e+18`,
		`1e-400`:                           `0.0`,
		`-1e-99999999999`:                  `-0.0`,
		`"\u00e9\u0001/\"\ud83d\ude00\t"`:  "\"é\\u0001/\\\"\U0001F600\\t\"",
		" \n\t{ } ":                        `{}`,
		`[true,false,null,"\/"]`:           `[true,false,null,"/"]`,
		`0.000001234`:                      `1.234e-6`,
		`123456789012345678901234567890.5`: `1.2345678901234568e+29`,
	} {
		got, err := SerdeReserialize(in)
		if err != nil || got != want {
			t.Errorf("SerdeReserialize(%q) = %q, %v; want %q", in, got, err, want)
		}
	}
}

func TestSerdeReserialize_Errors(t *testing.T) {
	for in, want := range map[string]string{
		`{nope`:                  "key must be a string at line 1 column 2",
		"[1,\n 2,]":              "expected value at line 2 column 4",
		`1e400`:                  "number out of range",
		`1e99999999999`:          "number out of range",
		`"\udc00"`:               "lone leading surrogate in hex escape at line 1 column 8",
		`"\ud800x"`:              "unexpected end of hex escape at line 1 column 8",
		`"\ud800\u0041"`:         "lone leading surrogate in hex escape at line 1 column 14",
		`"a`:                     "EOF while parsing a string at line 1 column 3",
		`"\`:                     "invalid escape at line 1 column 3",
		`[1] x`:                  "trailing characters at line 1 column 5",
		"\"\x01\"":               "control character (\\u0000-\\u001F) found while parsing a string at line 1 column 2",
		``:                       "EOF while parsing a value at line 1 column 1",
		strings.Repeat("[", 129): "recursion limit exceeded at line 1 column 129",
	} {
		_, err := SerdeReserialize(in)
		var se *SerdeError
		if !errors.As(err, &se) || se.Msg != want {
			t.Errorf("SerdeReserialize(%q) error = %v; want %q", in, err, want)
		}
	}
	if _, err := SerdeReserialize(strings.Repeat("[", 128) + strings.Repeat("]", 128)); err != nil {
		t.Errorf("depth 128 must parse: %v", err)
	}
}

func TestFormatF64(t *testing.T) {
	for f, want := range map[float64]string{
		0: "0.0", math.Copysign(0, -1): "-0.0", 1e15: "1000000000000000.0", 1e16: "1e+16", 1.5e16: "1.5e+16",
		1e-5: "0.00001", 1e-6: "1e-6", 0.1: "0.1", -1.5e22: "-1.5e+22", 5e-324: "5e-324", 123.456: "123.456",
	} {
		if got := FormatF64(f); got != want {
			t.Errorf("FormatF64(%v) = %q, want %q", f, got, want)
		}
	}
}

func FuzzSerdeReserialize(f *testing.F) {
	for _, s := range []string{`{"a":[1,2.5,-0,1e400,"x"]}`, `"😀"`, `[[[]]]`, `{"a":1,"a":2}`} {
		f.Add(s)
	}
	f.Fuzz(func(t *testing.T, s string) {
		out, err := SerdeReserialize(s)
		if err != nil {
			return
		}
		// Output is JSON that parses again. (It need not re-serialise to itself: serde_json's
		// default float parsing is not correctly rounded, so a float may move by one ulp.)
		if _, err := SerdeReserialize(out); err != nil {
			t.Fatalf("output does not parse: %q -> %q: %v", s, out, err)
		}
	})
}
