package jsonx

import (
	"fmt"
	"math"
	"strconv"
	"strings"
)

// SerdeReserialize is serde_json::to_string(&serde_json::from_str::<Value>(text)?) with
// serde_json's `preserve_order` feature, reproduced byte for byte. It is a line-by-line port of
// worker/src/serde-json.ts and is used only by the cURL exporter, to re-serialise GraphQL
// variables exactly as the Rust exporter does (QA-R1-006). Differences from encoding/json that
// matter here:
//   - object keys keep insertion order; a duplicate key replaces the value but keeps the first
//     key's position (IndexMap::insert);
//   - integers that fit u64/i64 are printed exactly; other numbers are parsed with serde_json's
//     default (not correctly rounded) algorithm and printed like serde_json 1.0.154 (`1.0`,
//     `1e+21`, `1e-7`, `-0.0`); out-of-range numbers are an error;
//   - strings escape only `"`, `\\` and control characters (`\u001f` style, lowercase hex);
//   - nesting deeper than 128 is an error (serde_json's recursion limit).
//
// Error positions count bytes, as serde_json does (the TypeScript port counted UTF-16 units).
func SerdeReserialize(text string) (string, error) {
	p := &serdeParser{text: text}
	v, err := p.value(0)
	if err != nil {
		return "", err
	}
	p.ws()
	if p.i < len(text) {
		return "", p.fail("trailing characters")
	}
	var b strings.Builder
	if err := serdeWrite(&b, v); err != nil {
		return "", err
	}
	return b.String(), nil
}

// SerdeError is a serde_json parse error with its 1-based position.
type SerdeError struct {
	Msg          string
	Line, Column int
}

func (e *SerdeError) Error() string { return e.Msg }

// SerdeString is serde_json's string serialisation (for building `{"query":…,"variables":…}`).
func SerdeString(s string) string {
	var b strings.Builder
	serdeStr(&b, s)
	return b.String()
}

type serdeKind uint8

const (
	sNull serdeKind = iota
	sBool
	sNum
	sStr
	sArr
	sObj
)

type serdeValue struct {
	kind serdeKind
	b    bool
	s    string // string text, or number lexeme
	arr  []*serdeValue
	keys []string
	vals []*serdeValue
}

const serdeMaxDepth = 128

type serdeParser struct {
	text string
	i    int
}

func (p *serdeParser) fail(msg string) error {
	i := min(p.i, len(p.text)) // an escape at the very end steps past it
	before := p.text[:i]
	line := strings.Count(before, "\n") + 1
	column := i - (strings.LastIndexByte(before, '\n') + 1) + 1
	return &SerdeError{Msg: fmt.Sprintf("%s at line %d column %d", msg, line, column), Line: line, Column: column}
}

func (p *serdeParser) ws() {
	for p.i < len(p.text) {
		switch p.text[p.i] {
		case ' ', '\t', '\n', '\r':
			p.i++
		default:
			return
		}
	}
}

func (p *serdeParser) value(depth int) (*serdeValue, error) {
	p.ws()
	if p.i >= len(p.text) {
		return nil, p.fail("EOF while parsing a value")
	}
	c := p.text[p.i]
	switch c {
	case '[':
		if depth >= serdeMaxDepth {
			return nil, p.fail("recursion limit exceeded")
		}
		p.i++
		p.ws()
		v := &serdeValue{kind: sArr}
		if p.peek() == ']' {
			p.i++
			return v, nil
		}
		for {
			item, err := p.value(depth + 1)
			if err != nil {
				return nil, err
			}
			v.arr = append(v.arr, item)
			p.ws()
			switch p.peek() {
			case ',':
				p.i++
			case ']':
				p.i++
				return v, nil
			default:
				return nil, p.fail("expected `,` or `]`")
			}
		}
	case '{':
		if depth >= serdeMaxDepth {
			return nil, p.fail("recursion limit exceeded")
		}
		p.i++
		p.ws()
		v := &serdeValue{kind: sObj}
		if p.peek() == '}' {
			p.i++
			return v, nil
		}
		for {
			p.ws()
			if p.peek() != '"' {
				return nil, p.fail("key must be a string")
			}
			key, err := p.str()
			if err != nil {
				return nil, err
			}
			p.ws()
			if p.peek() != ':' {
				return nil, p.fail("expected `:`")
			}
			p.i++
			item, err := p.value(depth + 1)
			if err != nil {
				return nil, err
			}
			at := -1
			for k, existing := range v.keys {
				if existing == key {
					at = k
					break
				}
			}
			if at < 0 {
				v.keys = append(v.keys, key)
				v.vals = append(v.vals, item)
			} else {
				v.vals[at] = item
			}
			p.ws()
			switch p.peek() {
			case ',':
				p.i++
			case '}':
				p.i++
				return v, nil
			default:
				return nil, p.fail("expected `,` or `}`")
			}
		}
	case '"':
		s, err := p.str()
		if err != nil {
			return nil, err
		}
		return &serdeValue{kind: sStr, s: s}, nil
	}
	for _, lit := range []struct {
		text string
		v    serdeValue
	}{{"true", serdeValue{kind: sBool, b: true}}, {"false", serdeValue{kind: sBool}}, {"null", serdeValue{kind: sNull}}} {
		if strings.HasPrefix(p.text[p.i:], lit.text) {
			p.i += len(lit.text)
			v := lit.v
			return &v, nil
		}
	}
	n := numberPrefix(p.text[p.i:])
	if n == 0 {
		return nil, p.fail("expected value")
	}
	v := &serdeValue{kind: sNum, s: p.text[p.i : p.i+n]}
	p.i += n
	return v, nil
}

func (p *serdeParser) peek() byte {
	if p.i < len(p.text) {
		return p.text[p.i]
	}
	return 0
}

// numberPrefix is the length of the match of /^-?(0|[1-9]\d*)(\.\d+)?([eE][+-]?\d+)?/, or 0.
func numberPrefix(s string) int {
	i := 0
	if i < len(s) && s[i] == '-' {
		i++
	}
	switch {
	case i < len(s) && s[i] == '0':
		i++
	case i < len(s) && s[i] >= '1' && s[i] <= '9':
		for i < len(s) && isDigit(s[i]) {
			i++
		}
	default:
		return 0
	}
	if i+1 < len(s) && s[i] == '.' && isDigit(s[i+1]) {
		i++
		for i < len(s) && isDigit(s[i]) {
			i++
		}
	}
	if i < len(s) && (s[i] == 'e' || s[i] == 'E') {
		j := i + 1
		if j < len(s) && (s[j] == '+' || s[j] == '-') {
			j++
		}
		if j < len(s) && isDigit(s[j]) {
			for j < len(s) && isDigit(s[j]) {
				j++
			}
			i = j
		}
	}
	return i
}

var serdeSimpleEscapes = map[byte]string{'"': "\"", '\\': "\\", '/': "/", 'b': "\b", 'f': "\f", 'n': "\n", 'r': "\r", 't': "\t"}

func (p *serdeParser) hex() (rune, error) {
	if p.i+4 > len(p.text) {
		return 0, p.fail("invalid escape")
	}
	var r rune
	for k := 0; k < 4; k++ {
		d, ok := hexVal(p.text[p.i+k])
		if !ok {
			return 0, p.fail("invalid escape")
		}
		r = r<<4 | d
	}
	p.i += 4
	return r, nil
}

func (p *serdeParser) str() (string, error) {
	p.i++ // opening quote
	var out strings.Builder
	for {
		if p.i >= len(p.text) {
			return "", p.fail("EOF while parsing a string")
		}
		c := p.text[p.i]
		if c == '"' {
			p.i++
			return out.String(), nil
		}
		if c < ' ' {
			return "", p.fail("control character (\\u0000-\\u001F) found while parsing a string")
		}
		if c != '\\' {
			out.WriteByte(c)
			p.i++
			continue
		}
		var e byte
		if p.i+1 < len(p.text) {
			e = p.text[p.i+1]
		}
		p.i += 2
		if s, ok := serdeSimpleEscapes[e]; ok {
			out.WriteString(s)
			continue
		}
		if e != 'u' {
			return "", p.fail("invalid escape")
		}
		cu, err := p.hex()
		if err != nil {
			return "", err
		}
		if cu >= 0xdc00 && cu <= 0xdfff {
			return "", p.fail("lone leading surrogate in hex escape")
		}
		if cu >= 0xd800 && cu <= 0xdbff {
			if p.peek() != '\\' || p.i+1 >= len(p.text) || p.text[p.i+1] != 'u' {
				return "", p.fail("unexpected end of hex escape")
			}
			p.i += 2
			lo, err := p.hex()
			if err != nil {
				return "", err
			}
			if lo < 0xdc00 || lo > 0xdfff {
				return "", p.fail("lone leading surrogate in hex escape")
			}
			out.WriteRune(0x10000 + (cu-0xd800)<<10 + (lo - 0xdc00))
		} else {
			out.WriteRune(cu)
		}
	}
}

// FormatF64 is serde_json 1.0.154's float formatting (shortest round-trip digits): plain decimal
// with a trailing `.0` while -5 < kk <= 16 (10^(kk-1) <= |f| < 10^kk), otherwise `d.ddde+X` /
// `de-X`. Verified against the Rust exporter by worker/test/fixtures/curl_parity.json.
func FormatF64(f float64) string {
	sign := ""
	if math.Signbit(f) {
		sign = "-"
	}
	if f == 0 {
		return sign + "0.0"
	}
	e := strconv.FormatFloat(math.Abs(f), 'e', -1, 64) // d.ddde±XX
	mant, expStr, _ := strings.Cut(e, "e")
	digits := strings.Replace(mant, ".", "", 1)
	exp10, _ := strconv.Atoi(expStr)
	length := len(digits)
	kk := exp10 + 1
	k := kk - length
	ee := kk - 1
	expPart := "e+" + strconv.Itoa(ee)
	if ee < 0 {
		expPart = "e-" + strconv.Itoa(-ee)
	}
	var out string
	switch {
	case k >= 0 && kk <= 16:
		out = digits + strings.Repeat("0", k) + ".0"
	case kk > 0 && kk <= 16:
		out = digits[:kk] + "." + digits[kk:]
	case kk > -5 && kk <= 0:
		out = "0." + strings.Repeat("0", -kk) + digits
	case length == 1:
		out = digits + expPart
	default:
		out = digits[:1] + "." + digits[1:] + expPart
	}
	return sign + out
}

var pow10 = func() [309]float64 {
	var t [309]float64
	for i := range t {
		t[i], _ = strconv.ParseFloat("1e"+strconv.Itoa(i), 64)
	}
	return t
}()

const (
	u64Max = math.MaxUint64
	i32Max = math.MaxInt32
	i32Min = math.MinInt32
)

func outOfRange() error { return &SerdeError{Msg: "number out of range", Line: 1, Column: 1} }

func overflowU64(sig, d uint64) bool {
	return sig >= u64Max/10 && (sig > u64Max/10 || d > u64Max%10)
}

// f64FromParts is serde_json's default (non-`float_roundtrip`) f64_from_parts.
func f64FromParts(significand uint64, exponent int64) (float64, error) {
	f := float64(significand)
	for {
		idx := exponent
		if idx < 0 {
			idx = -idx
		}
		if idx <= 308 {
			if exponent >= 0 {
				f *= pow10[idx]
				if math.IsInf(f, 0) {
					return 0, outOfRange()
				}
			} else {
				f /= pow10[idx]
			}
			return f, nil
		}
		if f == 0 {
			return f, nil
		}
		if exponent >= 0 {
			return 0, outOfRange()
		}
		f /= 1e308
		exponent += 308
	}
}

// serdeNumber turns a number lexeme into serde_json's Value and serialises it (the
// parse_integer, parse_decimal and parse_exponent paths).
func serdeNumber(lexeme string) (string, error) {
	i := 0
	positive := lexeme[0] != '-'
	if !positive {
		i++
	}
	digit := func(k int) bool { return k < len(lexeme) && isDigit(lexeme[k]) }
	var sig uint64
	var exponent int64
	isFloat := false
	if lexeme[i] == '0' {
		i++
	} else {
		for digit(i) {
			d := uint64(lexeme[i] - '0')
			if overflowU64(sig, d) {
				isFloat = true // parse_long_integer: remaining integer digits only scale the exponent
				for digit(i) {
					i++
					exponent++
				}
				break
			}
			sig = sig*10 + d
			i++
		}
	}
	if i < len(lexeme) && lexeme[i] == '.' {
		isFloat = true
		i++
		for digit(i) {
			d := uint64(lexeme[i] - '0')
			if overflowU64(sig, d) {
				for digit(i) { // parse_decimal_overflow: ignore further digits
					i++
				}
				break
			}
			sig = sig*10 + d
			exponent--
			i++
		}
	}
	if i < len(lexeme) && (lexeme[i] == 'e' || lexeme[i] == 'E') {
		isFloat = true
		i++
		positiveExp := true
		if i < len(lexeme) && (lexeme[i] == '+' || lexeme[i] == '-') {
			positiveExp = lexeme[i] == '+'
			i++
		}
		var exp int64
		for digit(i) {
			d := int64(lexeme[i] - '0')
			if exp >= i32Max/10 && (exp > i32Max/10 || d > i32Max%10) {
				// parse_exponent_overflow: error instead of infinity, else +/-0.0
				if sig != 0 && positiveExp {
					return "", outOfRange()
				}
				if positive {
					return FormatF64(0), nil
				}
				return FormatF64(math.Copysign(0, -1)), nil
			}
			exp = exp*10 + d
			i++
		}
		if positiveExp {
			exponent += exp
		} else {
			exponent -= exp
		}
		exponent = min(i32Max, max(i32Min, exponent))
	}
	if !isFloat {
		if positive {
			return strconv.FormatUint(sig, 10), nil
		}
		// `-0` and magnitudes beyond i64 become f64.
		if sig > 0 && sig <= 1<<63 {
			return "-" + strconv.FormatUint(sig, 10), nil
		}
		return FormatF64(-float64(sig)), nil
	}
	f, err := f64FromParts(sig, exponent)
	if err != nil {
		return "", err
	}
	if !positive {
		f = -f
	}
	return FormatF64(f), nil
}

func serdeStr(b *strings.Builder, s string) {
	b.WriteByte('"')
	for _, r := range s {
		switch {
		case r == '"':
			b.WriteString(`\"`)
		case r == '\\':
			b.WriteString(`\\`)
		case r < 0x20:
			switch r {
			case '\b':
				b.WriteString(`\b`)
			case '\t':
				b.WriteString(`\t`)
			case '\n':
				b.WriteString(`\n`)
			case '\f':
				b.WriteString(`\f`)
			case '\r':
				b.WriteString(`\r`)
			default:
				b.WriteString(`\u00`)
				b.WriteByte(hexDigits[r>>4])
				b.WriteByte(hexDigits[r&0xf])
			}
		default:
			b.WriteRune(r)
		}
	}
	b.WriteByte('"')
}

func serdeWrite(b *strings.Builder, v *serdeValue) error {
	switch v.kind {
	case sNull:
		b.WriteString("null")
	case sBool:
		if v.b {
			b.WriteString("true")
		} else {
			b.WriteString("false")
		}
	case sNum:
		s, err := serdeNumber(v.s)
		if err != nil {
			return err
		}
		b.WriteString(s)
	case sStr:
		serdeStr(b, v.s)
	case sArr:
		b.WriteByte('[')
		for i, item := range v.arr {
			if i > 0 {
				b.WriteByte(',')
			}
			if err := serdeWrite(b, item); err != nil {
				return err
			}
		}
		b.WriteByte(']')
	case sObj:
		b.WriteByte('{')
		for i, k := range v.keys {
			if i > 0 {
				b.WriteByte(',')
			}
			serdeStr(b, k)
			b.WriteByte(':')
			if err := serdeWrite(b, v.vals[i]); err != nil {
				return err
			}
		}
		b.WriteByte('}')
	}
	return nil
}
