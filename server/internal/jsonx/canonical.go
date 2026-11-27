package jsonx

import (
	"bytes"
	"math"
	"strconv"
	"unicode/utf8"
)

// MaxDepth is the deepest nesting a stored value may have, counting the value itself as level 1
// (W4). serde_json stops at 128 levels from the document root; a pull page puts `data` at level
// 4 and the client nests share snapshots inside its own structures, so 100 leaves headroom.
const MaxDepth = 100

// Canonicalize parses raw and returns its canonical form (see Canonical).
func Canonicalize(raw []byte) ([]byte, error) {
	v, err := Parse(raw)
	if err != nil {
		return nil, err
	}
	return Canonical(v)
}

// Canonical serialises v without insignificant whitespace, refusing anything the client's
// serde_json::Value parser would refuse:
//   - numbers that overflow the finite f64 range (ErrNumberRange); other lexemes are kept verbatim;
//   - strings that held an unpaired surrogate escape (ErrLoneSurrogate);
//   - nesting deeper than MaxDepth (ErrTooDeep).
//
// Strings are re-escaped minimally (\" \\ \b \f \n \r \t, other control characters as \u00xx,
// everything else raw UTF-8), which is also what serde_json writes.
func Canonical(v *Value) ([]byte, error) {
	var b bytes.Buffer
	if err := writeCanonical(&b, v, 1); err != nil {
		return nil, err
	}
	return b.Bytes(), nil
}

// Validate reports the error Canonical would return, without building the output.
func Validate(v *Value) error { return check(v, 1) }

func check(v *Value, depth int) error {
	switch v.Kind {
	case Number:
		return checkNumber(v.S)
	case String:
		if v.Lone {
			return ErrLoneSurrogate
		}
	case Array:
		if depth > MaxDepth {
			return ErrTooDeep
		}
		for _, it := range v.Arr {
			if err := check(it, depth+1); err != nil {
				return err
			}
		}
	case Object:
		if depth > MaxDepth {
			return ErrTooDeep
		}
		if v.Lone {
			return ErrLoneSurrogate
		}
		for _, it := range v.Vals {
			if err := check(it, depth+1); err != nil {
				return err
			}
		}
	}
	return nil
}

func checkNumber(lexeme string) error {
	// Underflow (1e-400) yields 0 or a subnormal, which serde_json accepts; only ±Inf is refused,
	// whatever err says.
	f, _ := strconv.ParseFloat(lexeme, 64)
	if math.IsInf(f, 0) {
		return ErrNumberRange
	}
	return nil
}

func writeCanonical(b *bytes.Buffer, v *Value, depth int) error {
	switch v.Kind {
	case Null:
		b.WriteString("null")
	case Bool:
		if v.B {
			b.WriteString("true")
		} else {
			b.WriteString("false")
		}
	case Number:
		if err := checkNumber(v.S); err != nil {
			return err
		}
		b.WriteString(v.S)
	case String:
		if v.Lone {
			return ErrLoneSurrogate
		}
		WriteString(b, v.S)
	case Array:
		if depth > MaxDepth {
			return ErrTooDeep
		}
		b.WriteByte('[')
		for i, it := range v.Arr {
			if i > 0 {
				b.WriteByte(',')
			}
			if err := writeCanonical(b, it, depth+1); err != nil {
				return err
			}
		}
		b.WriteByte(']')
	case Object:
		if depth > MaxDepth {
			return ErrTooDeep
		}
		if v.Lone {
			return ErrLoneSurrogate
		}
		b.WriteByte('{')
		for i, k := range v.Keys {
			if i > 0 {
				b.WriteByte(',')
			}
			WriteString(b, k)
			b.WriteByte(':')
			if err := writeCanonical(b, v.Vals[i], depth+1); err != nil {
				return err
			}
		}
		b.WriteByte('}')
	}
	return nil
}

const hexDigits = "0123456789abcdef"

// WriteString writes s as a JSON string literal with minimal escaping. s must be valid UTF-8.
func WriteString(b *bytes.Buffer, s string) {
	b.WriteByte('"')
	start := 0
	for i := 0; i < len(s); {
		c := s[i]
		if c >= utf8.RuneSelf {
			i++
			continue
		}
		var esc string
		switch c {
		case '"':
			esc = `\"`
		case '\\':
			esc = `\\`
		case '\b':
			esc = `\b`
		case '\f':
			esc = `\f`
		case '\n':
			esc = `\n`
		case '\r':
			esc = `\r`
		case '\t':
			esc = `\t`
		default:
			if c >= 0x20 {
				i++
				continue
			}
		}
		b.WriteString(s[start:i])
		if esc != "" {
			b.WriteString(esc)
		} else {
			b.WriteString(`\u00`)
			b.WriteByte(hexDigits[c>>4])
			b.WriteByte(hexDigits[c&0xF])
		}
		i++
		start = i
	}
	b.WriteString(s[start:])
	b.WriteByte('"')
}
