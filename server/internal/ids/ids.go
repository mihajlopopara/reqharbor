// Package ids generates identifiers and token primitives (port of worker/src/crypto.ts).
package ids

import (
	"crypto/rand"
	"crypto/sha256"
	"crypto/subtle"
	"encoding/base64"
	"encoding/hex"
	"math/big"
	"regexp"
	"strings"
	"time"
)

const (
	crockford = "0123456789ABCDEFGHJKMNPQRSTVWXYZ"
	base62    = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz"
)

var (
	// ShareIDRe matches a share id: 128 random bits as fixed-width (22 chars) base62.
	ShareIDRe = regexp.MustCompile(`^[0-9A-Za-z]{22}$`)
	// LegacyTokenRe matches a legacy workspace token: `rh_` + base64url(32 bytes).
	LegacyTokenRe = regexp.MustCompile(`^rh_[A-Za-z0-9_-]{43}$`)
)

func randomBytes(n int) []byte {
	b := make([]byte, n)
	if _, err := rand.Read(b); err != nil {
		panic("ids: crypto/rand failed: " + err.Error())
	}
	return b
}

// ULID returns a 26-character ULID: a 48-bit millisecond timestamp and 80 random bits in
// Crockford base32, the same construction as the Worker's ulid().
func ULID(now time.Time) string {
	var out [26]byte
	t := uint64(now.UnixMilli())
	for i := 9; i >= 0; i-- {
		out[i] = crockford[t%32]
		t /= 32
	}
	rnd := randomBytes(16)
	for i := 0; i < 16; i++ {
		out[10+i] = crockford[rnd[i]%32]
	}
	return string(out[:])
}

// ShareID returns 128 random bits as 22 base62 characters.
func ShareID() string {
	n := new(big.Int).SetBytes(randomBytes(16))
	base := big.NewInt(62)
	mod := new(big.Int)
	out := make([]byte, 22)
	for i := 21; i >= 0; i-- {
		n.DivMod(n, base, mod)
		out[i] = base62[mod.Int64()]
	}
	return string(out)
}

// Token returns prefix + base64url (no padding) of 32 random bytes; Token("rh_") is a legacy
// workspace token.
func Token(prefix string) string {
	return prefix + base64.RawURLEncoding.EncodeToString(randomBytes(32))
}

// SHA256Hex returns the lowercase hex SHA-256 digest of s.
func SHA256Hex(s string) string {
	sum := sha256.Sum256([]byte(s))
	return hex.EncodeToString(sum[:])
}

// Equal compares two strings in constant time (for equal lengths).
func Equal(a, b string) bool {
	return subtle.ConstantTimeCompare([]byte(a), []byte(b)) == 1
}

// Code returns `groups` groups of four Crockford base32 characters joined by '-', for example
// K7Q4-ZX2P-8M3D-1R6T (setup and invite codes).
func Code(groups int) string {
	parts := make([]string, groups)
	rnd := randomBytes(4 * groups)
	for g := 0; g < groups; g++ {
		var b [4]byte
		for i := 0; i < 4; i++ {
			b[i] = crockford[rnd[g*4+i]%32]
		}
		parts[g] = string(b[:])
	}
	return strings.Join(parts, "-")
}

// NormalizeCode canonicalises a setup or invite code as typed: spaces and dashes are dropped,
// letters are uppercased and the Crockford look-alikes I, L and O read as 1, 1 and 0.
func NormalizeCode(s string) string {
	var b strings.Builder
	for _, r := range strings.ToUpper(s) {
		switch {
		case r == ' ' || r == '-' || r == '\t':
		case r == 'I' || r == 'L':
			b.WriteByte('1')
		case r == 'O':
			b.WriteByte('0')
		default:
			b.WriteRune(r)
		}
	}
	return b.String()
}

// IsCrockford reports whether s is non-empty and only Crockford base32 characters (upper case,
// as NormalizeCode returns them).
func IsCrockford(s string) bool {
	return s != "" && strings.Trim(s, crockford) == ""
}
