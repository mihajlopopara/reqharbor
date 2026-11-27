package providers

// ID tokens of Google and generic OIDC (06-go-server.md §5.4, S10). An ID token is read only from
// the token-endpoint response over TLS, and its signature is checked as well: compact JWS, RS256
// or ES256 only (never "none", never HS*), with a key from the issuer's JWKS. Then the claims of
// OIDC Core §3.1.3.7: iss, aud (a string or an array) with azp, exp, iat and nbf with a clock-skew
// tolerance, nonce and sub. No JOSE library: the stdlib verifies both algorithms.

import (
	"bytes"
	"context"
	"crypto"
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rsa"
	"crypto/sha256"
	"crypto/subtle"
	"encoding/asn1"
	"encoding/base64"
	"encoding/json"
	"io"
	"math"
	"math/big"
	"strings"
	"time"
)

// ClockSkew is the tolerance for exp, iat and nbf: the server's and the IdP's clocks may differ by
// this much.
const ClockSkew = 5 * time.Minute

// maxIDTokenBytes bounds a compact ID token.
const maxIDTokenBytes = 64 << 10

// Signing algorithms accepted for ID tokens.
const (
	AlgRS256 = "RS256"
	AlgES256 = "ES256"
)

// jwsHeader is the protected header of a compact JWS. Crit is kept raw: its mere presence makes the
// token unacceptable, because no extension is understood.
type jwsHeader struct {
	Alg  string          `json:"alg"`
	Kid  string          `json:"kid"`
	Crit json.RawMessage `json:"crit"`
}

// jws is a parsed compact JWS.
type jws struct {
	header       jwsHeader
	payload      []byte
	signingInput []byte
	signature    []byte
}

// b64urlDecode decodes unpadded base64url strictly: no padding, no standard-alphabet characters,
// no CR or LF (which the decoder would otherwise skip) and no non-zero trailing bits, so every
// token has exactly one encoding.
func b64urlDecode(s string) ([]byte, error) {
	if strings.ContainsAny(s, "=+/\r\n") {
		return nil, base64.CorruptInputError(0)
	}
	return base64.RawURLEncoding.Strict().DecodeString(s)
}

// parseJWS splits and decodes a compact JWS and checks that its algorithm is RS256 or ES256.
func parseJWS(raw string) (*jws, error) {
	if len(raw) > maxIDTokenBytes {
		return nil, &Error{Code: "id_token_malformed"}
	}
	parts := strings.Split(raw, ".")
	if len(parts) != 3 || parts[0] == "" || parts[1] == "" {
		return nil, &Error{Code: "id_token_malformed"}
	}
	hb, err1 := b64urlDecode(parts[0])
	payload, err2 := b64urlDecode(parts[1])
	sig, err3 := b64urlDecode(parts[2])
	if err1 != nil || err2 != nil || err3 != nil {
		return nil, &Error{Code: "id_token_malformed"}
	}
	var h jwsHeader
	if err := json.Unmarshal(hb, &h); err != nil || !bytes.HasPrefix(bytes.TrimSpace(hb), []byte("{")) {
		return nil, &Error{Code: "id_token_malformed"}
	}
	if h.Alg != AlgRS256 && h.Alg != AlgES256 {
		return nil, &Error{Code: "id_token_alg"}
	}
	if h.Crit != nil {
		return nil, &Error{Code: "id_token_crit"}
	}
	if len(sig) == 0 {
		return nil, &Error{Code: "id_token_signature"}
	}
	return &jws{header: h, payload: payload, signingInput: []byte(parts[0] + "." + parts[1]), signature: sig}, nil
}

// verifySignature checks sig over input with key for alg. The key's type must match the
// algorithm: an RSA key never verifies ES256, a P-256 key never verifies RS256.
func verifySignature(alg string, key crypto.PublicKey, input, sig []byte) bool {
	sum := sha256.Sum256(input)
	switch alg {
	case AlgRS256:
		k, ok := key.(*rsa.PublicKey)
		return ok && rsa.VerifyPKCS1v15(k, crypto.SHA256, sum[:], sig) == nil
	case AlgES256:
		k, ok := key.(*ecdsa.PublicKey)
		if !ok || k.Curve != elliptic.P256() || len(sig) != 64 {
			return false
		}
		der, err := asn1.Marshal(struct{ R, S *big.Int }{new(big.Int).SetBytes(sig[:32]), new(big.Int).SetBytes(sig[32:])})
		return err == nil && ecdsa.VerifyASN1(k, sum[:], der)
	}
	return false
}

// IDClaims are the validated claims of an ID token; Raw holds every claim.
type IDClaims struct {
	Issuer   string
	Subject  string
	Audience []string
	AZP      string
	Nonce    string
	Expiry   time.Time
	Raw      map[string]any
}

// Expect is what an ID token must say.
type Expect struct {
	Issuers  []string // accepted iss values (exact match)
	ClientID string
	Nonce    string
	Now      time.Time
}

// numericDate reads a JSON NumericDate (seconds since the epoch, possibly fractional).
func numericDate(v any) (time.Time, bool) {
	n, ok := v.(json.Number)
	if !ok {
		return time.Time{}, false
	}
	f, err := n.Float64()
	if err != nil || math.IsNaN(f) || math.IsInf(f, 0) || f < 0 || f > 1e11 {
		return time.Time{}, false
	}
	sec, frac := math.Modf(f)
	return time.Unix(int64(sec), int64(frac*1e9)), true
}

// audience reads aud: a string, or an array of strings.
func audience(v any) ([]string, bool) {
	switch a := v.(type) {
	case string:
		return []string{a}, true
	case []any:
		out := make([]string, 0, len(a))
		for _, e := range a {
			s, ok := e.(string)
			if !ok {
				return nil, false
			}
			out = append(out, s)
		}
		return out, len(out) > 0
	}
	return nil, false
}

// decodeClaims parses the payload as a JSON object, keeping numbers exact.
func decodeClaims(payload []byte) (map[string]any, error) {
	dec := json.NewDecoder(bytes.NewReader(payload))
	dec.UseNumber()
	var raw map[string]any
	if err := dec.Decode(&raw); err != nil || raw == nil {
		return nil, &Error{Code: "id_token_malformed"}
	}
	var extra any
	if err := dec.Decode(&extra); err != io.EOF {
		return nil, &Error{Code: "id_token_malformed"} // anything after the object, even a stray } or ]
	}
	return raw, nil
}

// ValidateClaims applies OIDC Core §3.1.3.7 to the claims of a signature-checked ID token:
//   - iss equals one of want.Issuers;
//   - aud is a string or an array of strings and contains the client id; with more than one
//     audience azp is required, and whenever azp is present it must equal the client id;
//   - exp is present and now < exp + ClockSkew; iat and nbf, when present, are not later than
//     now + ClockSkew;
//   - nonce equals the round trip's nonce (constant-time);
//   - sub is a non-empty string of at most 255 bytes.
func ValidateClaims(raw map[string]any, want Expect) (*IDClaims, error) {
	c := &IDClaims{Raw: raw}
	str := func(name string) (string, bool) {
		v, present := raw[name]
		if !present {
			return "", true
		}
		s, ok := v.(string)
		return s, ok
	}
	var ok bool
	if c.Issuer, ok = str("iss"); !ok || c.Issuer == "" || !contains(want.Issuers, c.Issuer) {
		return nil, &Error{Code: "iss_mismatch"}
	}
	if c.Audience, ok = audience(raw["aud"]); !ok || !contains(c.Audience, want.ClientID) {
		return nil, &Error{Code: "aud_mismatch"}
	}
	_, hasAZP := raw["azp"]
	if c.AZP, ok = str("azp"); !ok {
		return nil, &Error{Code: "azp_mismatch"}
	}
	switch {
	case len(c.Audience) > 1 && !hasAZP:
		return nil, &Error{Code: "azp_missing"}
	case hasAZP && c.AZP != want.ClientID:
		return nil, &Error{Code: "azp_mismatch"}
	}
	exp, present := raw["exp"]
	if !present {
		return nil, &Error{Code: "exp_missing"}
	}
	if c.Expiry, ok = numericDate(exp); !ok {
		return nil, &Error{Code: "exp_invalid"}
	}
	if !want.Now.Before(c.Expiry.Add(ClockSkew)) {
		return nil, &Error{Code: "expired"}
	}
	for _, name := range []string{"iat", "nbf"} {
		v, present := raw[name]
		if !present {
			continue
		}
		t, ok := numericDate(v)
		if !ok {
			return nil, &Error{Code: name + "_invalid"}
		}
		if t.After(want.Now.Add(ClockSkew)) {
			return nil, &Error{Code: name + "_future"}
		}
	}
	if c.Nonce, ok = str("nonce"); !ok || c.Nonce == "" || want.Nonce == "" {
		return nil, &Error{Code: "nonce_missing"}
	}
	if subtle.ConstantTimeCompare([]byte(c.Nonce), []byte(want.Nonce)) != 1 {
		return nil, &Error{Code: "nonce_mismatch"}
	}
	if c.Subject, ok = str("sub"); !ok || c.Subject == "" || len(c.Subject) > 255 {
		return nil, &Error{Code: "sub_missing"}
	}
	return c, nil
}

func contains(list []string, s string) bool {
	for _, e := range list {
		if e == s {
			return true
		}
	}
	return false
}

// idTokenVerifier checks ID tokens of one issuer.
type idTokenVerifier struct {
	keys     *JWKS
	issuers  []string
	clientID string
	now      func() time.Time
}

// verify parses raw, checks its signature with a key of the issuer's JWKS and validates its
// claims for nonce.
func (v *idTokenVerifier) verify(ctx context.Context, raw, nonce string) (*IDClaims, error) {
	t, err := parseJWS(raw)
	if err != nil {
		return nil, err
	}
	keys, err := v.keys.Keys(ctx, t.header.Kid, t.header.Alg)
	if err != nil {
		return nil, err
	}
	verified := false
	for _, k := range keys {
		if verifySignature(t.header.Alg, k, t.signingInput, t.signature) {
			verified = true
			break
		}
	}
	if !verified {
		return nil, &Error{Code: "id_token_signature"}
	}
	claims, err := decodeClaims(t.payload)
	if err != nil {
		return nil, err
	}
	return ValidateClaims(claims, Expect{Issuers: v.issuers, ClientID: v.clientID, Nonce: nonce, Now: v.now()})
}
