package jsx

import "testing"

func TestTrim_MatchesStringPrototypeTrim(t *testing.T) {
	for in, want := range map[string]string{
		"  a  ":                           "a",
		"\t\n\v\f\r a \r\n":               "a",
		string([]rune{0xA0, 'a', 0x3000}): "a",
		string([]rune{0xFEFF, 'a', 0x2028, 0x2029}):                 "a",
		string([]rune{0x2000, 0x200A, 'a', 0x202F, 0x205F, 0x1680}): "a",
		// U+0085 (NEL) is Unicode White_Space but not trimmed by JavaScript.
		string([]rune{0x85, 'a'}):   string([]rune{0x85, 'a'}),
		string([]rune{0x180E, 'a'}): string([]rune{0x180E, 'a'}),
		"":                          "",
	} {
		if got := Trim(in); got != want {
			t.Errorf("Trim(%q) = %q, want %q", in, got, want)
		}
	}
}

func TestUTF16Len(t *testing.T) {
	for in, want := range map[string]int{"": 0, "abc": 3, "é": 1, "€": 1, string(rune(0x1F600)): 2, "a" + string(rune(0x10FFFF)): 3} {
		if got := UTF16Len(in); got != want {
			t.Errorf("UTF16Len(%q) = %d, want %d", in, got, want)
		}
	}
}

func TestSafeInteger(t *testing.T) {
	for lex, want := range map[string]int64{"0": 0, "-0": 0, "1e3": 1000, "1.0": 1, "9007199254740991": MaxSafeInteger, "-9007199254740991": -MaxSafeInteger, "1E2": 100, "1e-400": 0} {
		if got, ok := SafeInteger(lex); !ok || got != want {
			t.Errorf("SafeInteger(%q) = %d %v", lex, got, ok)
		}
	}
	for _, lex := range []string{"1.5", "9007199254740992", "9007199254740993", "1e400", "-1e400", "x", "", "1e16"} {
		if _, ok := SafeInteger(lex); ok {
			t.Errorf("SafeInteger(%q) accepted", lex)
		}
	}
}

func TestNumberString_MatchesJavaScript(t *testing.T) {
	cases := map[string]string{
		"42": "42", "-0": "0", "1.50": "1.5", "1e21": "1e+21", "1e20": "100000000000000000000",
		"123456789012345680000": "123456789012345680000", "0.000001": "0.000001", "1e-7": "1e-7",
		"1.5e-7": "1.5e-7", "-2.5e300": "-2.5e+300", "9007199254740993": "9007199254740992",
		"0.1": "0.1", "1e400": "Infinity", "-1e400": "-Infinity", "1e-400": "0", "5e-324": "5e-324",
	}
	for in, want := range cases {
		if got := NumberString(in); got != want {
			t.Errorf("NumberString(%q) = %q, want %q", in, got, want)
		}
	}
}
