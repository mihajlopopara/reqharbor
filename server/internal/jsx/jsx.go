// Package jsx reproduces the JavaScript semantics the Worker relied on (String.prototype.trim,
// string length in UTF-16 code units, Number.isSafeInteger).
package jsx

import (
	"errors"
	"math"
	"strconv"
	"strings"
	"unicode/utf16"
)

// MaxSafeInteger is Number.MAX_SAFE_INTEGER (2^53 - 1).
const MaxSafeInteger = 1<<53 - 1

// IsJSSpace reports whether r is removed by String.prototype.trim: ECMAScript WhiteSpace
// (TAB, VT, FF, SP, NBSP, ZWNBSP/BOM and category Zs) plus LineTerminator (LF, CR, LS, PS).
// That is Unicode White_Space without U+0085 (NEL), plus U+FEFF.
func IsJSSpace(r rune) bool {
	switch r {
	case '\t', '\n', '\v', '\f', '\r', ' ', 0x00A0, 0x1680, 0x2028, 0x2029, 0x202F, 0x205F, 0x3000, 0xFEFF:
		return true
	}
	return r >= 0x2000 && r <= 0x200A
}

// Trim is String.prototype.trim.
func Trim(s string) string { return strings.TrimFunc(s, IsJSSpace) }

// UTF16Len is a JavaScript string's .length: the number of UTF-16 code units.
func UTF16Len(s string) int {
	n := 0
	for _, r := range s {
		n += utf16.RuneLen(r)
	}
	return n
}

// SafeInteger parses a JSON number lexeme the way JSON.parse does and reports whether
// Number.isSafeInteger accepts the result. It returns the integer value when it does.
func SafeInteger(lexeme string) (int64, bool) {
	f, err := strconv.ParseFloat(lexeme, 64)
	if errors.Is(err, strconv.ErrSyntax) {
		return 0, false
	}
	// strconv.ErrRange: overflow gives ±Inf (refused below); underflow gives 0, which JSON.parse
	// also yields.
	if math.IsInf(f, 0) || math.IsNaN(f) || f != math.Trunc(f) || math.Abs(f) > MaxSafeInteger {
		return 0, false
	}
	return int64(f), true
}

// NumberString is String(Number(lexeme)) for a JSON number lexeme: JavaScript's Number::toString
// (shortest round-trip digits; decimal notation for 1e-7 <= |x| < 1e21, exponent notation
// otherwise). A lexeme that overflows the f64 range gives "Infinity" or "-Infinity", as
// JSON.parse would.
func NumberString(lexeme string) string {
	f, err := strconv.ParseFloat(lexeme, 64)
	if errors.Is(err, strconv.ErrSyntax) {
		return "NaN"
	}
	return FormatNumber(f)
}

// FormatNumber is JavaScript's Number::toString(f) in base 10.
func FormatNumber(f float64) string {
	switch {
	case math.IsNaN(f):
		return "NaN"
	case math.IsInf(f, 1):
		return "Infinity"
	case math.IsInf(f, -1):
		return "-Infinity"
	case f == 0:
		return "0" // also -0
	}
	sign := ""
	if f < 0 {
		sign, f = "-", -f
	}
	mant, expStr, _ := strings.Cut(strconv.FormatFloat(f, 'e', -1, 64), "e")
	digits := strings.Replace(mant, ".", "", 1)
	e, _ := strconv.Atoi(expStr)
	k, n := len(digits), e+1
	var out string
	switch {
	case k <= n && n <= 21:
		out = digits + strings.Repeat("0", n-k)
	case 0 < n && n <= 21:
		out = digits[:n] + "." + digits[n:]
	case -6 < n && n <= 0:
		out = "0." + strings.Repeat("0", -n) + digits
	default:
		exp := "e+" + strconv.Itoa(n-1)
		if n-1 < 0 {
			exp = "e-" + strconv.Itoa(1-n)
		}
		if k == 1 {
			out = digits + exp
		} else {
			out = digits[:1] + "." + digits[1:] + exp
		}
	}
	return sign + out
}
