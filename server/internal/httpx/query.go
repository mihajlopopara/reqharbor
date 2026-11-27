package httpx

import (
	"net/http"
	"strings"
)

// QueryGet returns the first value of name in the query string, parsed like the Worker's
// URLSearchParams: pairs split on '&' only (Go's url.ParseQuery drops pairs containing ';'),
// '+' is a space and percent-escapes decode leniently (an invalid escape stays literal).
func QueryGet(r *http.Request, name string) string {
	for _, pair := range strings.Split(r.URL.RawQuery, "&") {
		if pair == "" {
			continue
		}
		k, v, _ := strings.Cut(pair, "=")
		if formDecode(k) == name {
			return formDecode(v)
		}
	}
	return ""
}

func unhex(c byte) (byte, bool) {
	switch {
	case c >= '0' && c <= '9':
		return c - '0', true
	case c >= 'a' && c <= 'f':
		return c - 'a' + 10, true
	case c >= 'A' && c <= 'F':
		return c - 'A' + 10, true
	}
	return 0, false
}

func formDecode(s string) string {
	if !strings.ContainsAny(s, "+%") {
		return s
	}
	b := make([]byte, 0, len(s))
	for i := 0; i < len(s); i++ {
		c := s[i]
		switch {
		case c == '+':
			b = append(b, ' ')
		case c == '%' && i+2 < len(s):
			hi, ok1 := unhex(s[i+1])
			lo, ok2 := unhex(s[i+2])
			if ok1 && ok2 {
				b = append(b, hi<<4|lo)
				i += 2
			} else {
				b = append(b, c)
			}
		default:
			b = append(b, c)
		}
	}
	return strings.ToValidUTF8(string(b), "�")
}
