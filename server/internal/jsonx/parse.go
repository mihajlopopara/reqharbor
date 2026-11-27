package jsonx

import (
	"errors"
	"fmt"
	"unicode/utf16"
	"unicode/utf8"
)

// Errors returned by Parse and Canonical.
var (
	// ErrSyntax: the text is not JSON.
	ErrSyntax = errors.New("jsonx: invalid JSON")
	// ErrNumberRange: a number overflows the finite f64 range ("number out of range" in serde_json).
	ErrNumberRange = errors.New("jsonx: number out of range")
	// ErrLoneSurrogate: a string holds an unpaired UTF-16 surrogate escape.
	ErrLoneSurrogate = errors.New("jsonx: lone surrogate in string escape")
	// ErrTooDeep: the value nests deeper than allowed.
	ErrTooDeep = errors.New("jsonx: value nests too deeply")
)

// MaxParseDepth bounds the recursion of Parse. Anything this deep is refused as ErrTooDeep for
// the whole text. Stored values have the much lower MaxDepth, checked by Canonical.
const MaxParseDepth = 10000

// SyntaxError describes where parsing failed. errors.Is(err, ErrSyntax) holds.
type SyntaxError struct {
	Offset int
	Msg    string
}

func (e *SyntaxError) Error() string { return fmt.Sprintf("jsonx: %s at offset %d", e.Msg, e.Offset) }

// Is reports ErrSyntax.
func (e *SyntaxError) Is(target error) bool { return target == ErrSyntax }

// Parse parses one JSON text (RFC 8259; whitespace is space, tab, LF and CR only, like
// JSON.parse). The input must be valid UTF-8; invalid UTF-8 is a syntax error. Lone surrogate
// escapes and out-of-range numbers are accepted here and judged by Canonical.
func Parse(data []byte) (*Value, error) {
	if !utf8.Valid(data) {
		return nil, &SyntaxError{Offset: 0, Msg: "invalid UTF-8"}
	}
	p := parser{data: data}
	p.ws()
	v, err := p.value(1)
	if err != nil {
		return nil, err
	}
	p.ws()
	if p.i != len(p.data) {
		return nil, p.fail("trailing characters")
	}
	return v, nil
}

type parser struct {
	data []byte
	i    int
}

func (p *parser) fail(msg string) error { return &SyntaxError{Offset: p.i, Msg: msg} }

func (p *parser) ws() {
	for p.i < len(p.data) {
		switch p.data[p.i] {
		case ' ', '\t', '\n', '\r':
			p.i++
		default:
			return
		}
	}
}

func (p *parser) value(depth int) (*Value, error) {
	if p.i >= len(p.data) {
		return nil, p.fail("unexpected end of input")
	}
	switch c := p.data[p.i]; c {
	case '{':
		return p.object(depth)
	case '[':
		return p.array(depth)
	case '"':
		s, lone, err := p.str()
		if err != nil {
			return nil, err
		}
		return &Value{Kind: String, S: s, Lone: lone}, nil
	case 't':
		return p.literal("true", &Value{Kind: Bool, B: true})
	case 'f':
		return p.literal("false", &Value{Kind: Bool})
	case 'n':
		return p.literal("null", &Value{Kind: Null})
	default:
		if c == '-' || (c >= '0' && c <= '9') {
			return p.number()
		}
		return nil, p.fail("unexpected character")
	}
}

func (p *parser) literal(lit string, v *Value) (*Value, error) {
	if len(p.data)-p.i < len(lit) || string(p.data[p.i:p.i+len(lit)]) != lit {
		return nil, p.fail("invalid literal")
	}
	p.i += len(lit)
	return v, nil
}

func (p *parser) object(depth int) (*Value, error) {
	if depth > MaxParseDepth {
		return nil, ErrTooDeep
	}
	p.i++ // {
	v := &Value{Kind: Object}
	p.ws()
	if p.i < len(p.data) && p.data[p.i] == '}' {
		p.i++
		return v, nil
	}
	for {
		p.ws()
		if p.i >= len(p.data) || p.data[p.i] != '"' {
			return nil, p.fail("object key must be a string")
		}
		key, lone, err := p.str()
		if err != nil {
			return nil, err
		}
		if lone {
			v.Lone = true // on an Object, Lone marks a key with an unpaired surrogate
		}
		p.ws()
		if p.i >= len(p.data) || p.data[p.i] != ':' {
			return nil, p.fail("expected ':'")
		}
		p.i++
		p.ws()
		val, err := p.value(depth + 1)
		if err != nil {
			return nil, err
		}
		// JSON.parse: a duplicate key keeps its first position and takes the last value.
		v.Set(key, val)
		p.ws()
		if p.i >= len(p.data) {
			return nil, p.fail("unexpected end of input")
		}
		switch p.data[p.i] {
		case ',':
			p.i++
		case '}':
			p.i++
			return v, nil
		default:
			return nil, p.fail("expected ',' or '}'")
		}
	}
}

func (p *parser) array(depth int) (*Value, error) {
	if depth > MaxParseDepth {
		return nil, ErrTooDeep
	}
	p.i++ // [
	v := &Value{Kind: Array, Arr: []*Value{}}
	p.ws()
	if p.i < len(p.data) && p.data[p.i] == ']' {
		p.i++
		return v, nil
	}
	for {
		p.ws()
		item, err := p.value(depth + 1)
		if err != nil {
			return nil, err
		}
		v.Arr = append(v.Arr, item)
		p.ws()
		if p.i >= len(p.data) {
			return nil, p.fail("unexpected end of input")
		}
		switch p.data[p.i] {
		case ',':
			p.i++
		case ']':
			p.i++
			return v, nil
		default:
			return nil, p.fail("expected ',' or ']'")
		}
	}
}

func isDigit(c byte) bool { return c >= '0' && c <= '9' }

// number scans -?(0|[1-9]\d*)(\.\d+)?([eE][+-]?\d+)? and keeps the lexeme.
func (p *parser) number() (*Value, error) {
	start := p.i
	d := p.data
	if d[p.i] == '-' {
		p.i++
	}
	switch {
	case p.i < len(d) && d[p.i] == '0':
		p.i++
	case p.i < len(d) && d[p.i] >= '1' && d[p.i] <= '9':
		for p.i < len(d) && isDigit(d[p.i]) {
			p.i++
		}
	default:
		return nil, p.fail("invalid number")
	}
	if p.i < len(d) && d[p.i] == '.' {
		p.i++
		if p.i >= len(d) || !isDigit(d[p.i]) {
			return nil, p.fail("invalid number")
		}
		for p.i < len(d) && isDigit(d[p.i]) {
			p.i++
		}
	}
	if p.i < len(d) && (d[p.i] == 'e' || d[p.i] == 'E') {
		p.i++
		if p.i < len(d) && (d[p.i] == '+' || d[p.i] == '-') {
			p.i++
		}
		if p.i >= len(d) || !isDigit(d[p.i]) {
			return nil, p.fail("invalid number")
		}
		for p.i < len(d) && isDigit(d[p.i]) {
			p.i++
		}
	}
	return &Value{Kind: Number, S: string(d[start:p.i])}, nil
}

func hexVal(c byte) (rune, bool) {
	switch {
	case c >= '0' && c <= '9':
		return rune(c - '0'), true
	case c >= 'a' && c <= 'f':
		return rune(c-'a') + 10, true
	case c >= 'A' && c <= 'F':
		return rune(c-'A') + 10, true
	}
	return 0, false
}

func (p *parser) hex4() (rune, error) {
	if len(p.data)-p.i < 4 {
		return 0, p.fail("invalid \\u escape")
	}
	var r rune
	for k := 0; k < 4; k++ {
		h, ok := hexVal(p.data[p.i+k])
		if !ok {
			return 0, p.fail("invalid \\u escape")
		}
		r = r<<4 | h
	}
	p.i += 4
	return r, nil
}

// str decodes a string starting at the opening quote. lone reports an unpaired surrogate escape,
// which is replaced by U+FFFD in the result.
func (p *parser) str() (s string, lone bool, err error) {
	p.i++ // opening quote
	d := p.data
	start := p.i
	// Fast path: no escapes.
	for p.i < len(d) {
		c := d[p.i]
		if c == '"' {
			s = string(d[start:p.i])
			p.i++
			return s, false, nil
		}
		if c == '\\' {
			break
		}
		if c < 0x20 {
			return "", false, p.fail("control character in string")
		}
		p.i++
	}
	buf := make([]byte, 0, p.i-start+16)
	buf = append(buf, d[start:p.i]...)
	for {
		if p.i >= len(d) {
			return "", false, p.fail("unterminated string")
		}
		c := d[p.i]
		switch {
		case c == '"':
			p.i++
			return string(buf), lone, nil
		case c < 0x20:
			return "", false, p.fail("control character in string")
		case c != '\\':
			buf = append(buf, c)
			p.i++
			continue
		}
		p.i++ // backslash
		if p.i >= len(d) {
			return "", false, p.fail("unterminated string")
		}
		e := d[p.i]
		p.i++
		switch e {
		case '"', '\\', '/':
			buf = append(buf, e)
		case 'b':
			buf = append(buf, '\b')
		case 'f':
			buf = append(buf, '\f')
		case 'n':
			buf = append(buf, '\n')
		case 'r':
			buf = append(buf, '\r')
		case 't':
			buf = append(buf, '\t')
		case 'u':
			r, err := p.hex4()
			if err != nil {
				return "", false, err
			}
			switch {
			case r >= 0xD800 && r <= 0xDBFF:
				// A high surrogate must be followed by an escaped low surrogate.
				if len(d)-p.i >= 6 && d[p.i] == '\\' && d[p.i+1] == 'u' {
					save := p.i
					p.i += 2
					r2, err := p.hex4()
					if err != nil {
						return "", false, err
					}
					if r2 >= 0xDC00 && r2 <= 0xDFFF {
						buf = utf8.AppendRune(buf, utf16.DecodeRune(r, r2))
						continue
					}
					p.i = save // the next escape is decoded on its own
				}
				lone = true
				buf = utf8.AppendRune(buf, utf8.RuneError)
			case r >= 0xDC00 && r <= 0xDFFF:
				lone = true
				buf = utf8.AppendRune(buf, utf8.RuneError)
			default:
				buf = utf8.AppendRune(buf, r)
			}
		default:
			p.i--
			return "", false, p.fail("invalid escape")
		}
	}
}
