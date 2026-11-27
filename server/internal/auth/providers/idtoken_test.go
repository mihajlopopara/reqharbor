package providers

import (
	"context"
	"crypto"
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/hmac"
	"crypto/rand"
	"crypto/rsa"
	"crypto/sha256"
	"encoding/asn1"
	"encoding/base64"
	"encoding/json"
	"math/big"
	"net/http"
	"net/http/httptest"
	"strconv"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/mihajlopopara/reqharbor/server/internal/clock"
)

// Test keys and a signer (this package cannot import testutil, which imports it).

var (
	keysOnce sync.Once
	rsaKey1  *rsa.PrivateKey
	rsaKey2  *rsa.PrivateKey
	ecKey1   *ecdsa.PrivateKey
)

func testKeys(t *testing.T) {
	t.Helper()
	keysOnce.Do(func() {
		var err error
		if rsaKey1, err = rsa.GenerateKey(rand.Reader, 2048); err != nil {
			panic(err)
		}
		if rsaKey2, err = rsa.GenerateKey(rand.Reader, 2048); err != nil {
			panic(err)
		}
		if ecKey1, err = ecdsa.GenerateKey(elliptic.P256(), rand.Reader); err != nil {
			panic(err)
		}
	})
}

var enc = base64.RawURLEncoding.EncodeToString

func rsaJWK(kid string, k *rsa.PrivateKey) map[string]any {
	return map[string]any{"kty": "RSA", "kid": kid, "use": "sig", "alg": "RS256", "n": enc(k.N.Bytes()), "e": enc(big.NewInt(int64(k.E)).Bytes())}
}

func ecJWK(kid string, k *ecdsa.PrivateKey) map[string]any {
	b, _ := k.PublicKey.Bytes()
	return map[string]any{"kty": "EC", "kid": kid, "use": "sig", "alg": "ES256", "crv": "P-256", "x": enc(b[1:33]), "y": enc(b[33:])}
}

// sign makes a compact JWS; key is *rsa.PrivateKey, *ecdsa.PrivateKey or []byte (HMAC), and
// the header's alg decides the algorithm actually used ("none": no signature).
func sign(t *testing.T, header, claims map[string]any, key any) string {
	t.Helper()
	hb, _ := json.Marshal(header)
	cb, _ := json.Marshal(claims)
	input := enc(hb) + "." + enc(cb)
	sum := sha256.Sum256([]byte(input))
	var sig []byte
	switch k := key.(type) {
	case nil:
	case []byte:
		m := hmac.New(sha256.New, k)
		m.Write([]byte(input))
		sig = m.Sum(nil)
	case *rsa.PrivateKey:
		var err error
		if sig, err = rsa.SignPKCS1v15(rand.Reader, k, crypto.SHA256, sum[:]); err != nil {
			t.Fatal(err)
		}
	case *ecdsa.PrivateKey:
		der, err := ecdsa.SignASN1(rand.Reader, k, sum[:])
		if err != nil {
			t.Fatal(err)
		}
		var rs struct{ R, S *big.Int }
		if _, err := asn1.Unmarshal(der, &rs); err != nil {
			t.Fatal(err)
		}
		sig = make([]byte, 64)
		rs.R.FillBytes(sig[:32])
		rs.S.FillBytes(sig[32:])
	}
	return input + "." + enc(sig)
}

var testNow = time.Date(2026, 11, 27, 12, 0, 0, 0, time.UTC)

func validClaims() map[string]any {
	return map[string]any{
		"iss": "https://idp.example.com", "sub": "user-1", "aud": "client-1", "nonce": "n-123",
		"exp": json.Number(strconv.FormatInt(testNow.Add(time.Hour).Unix(), 10)),
		"iat": json.Number(strconv.FormatInt(testNow.Unix(), 10)),
	}
}

func expect() Expect {
	return Expect{Issuers: []string{"https://idp.example.com", "idp.example.com"}, ClientID: "client-1", Nonce: "n-123", Now: testNow}
}

// Every claim check of OIDC Core §3.1.3.7 (§5.4 audience rules, clock skew).
func TestValidateClaims(t *testing.T) {
	sec := func(d time.Duration) json.Number { return json.Number(strconv.FormatInt(testNow.Add(d).Unix(), 10)) }
	for _, tc := range []struct {
		name string
		set  map[string]any // nil value deletes
		want string         // "" means valid
	}{
		{"valid", nil, ""},
		{"second accepted issuer", map[string]any{"iss": "idp.example.com"}, ""},
		{"iss of another issuer", map[string]any{"iss": "https://evil.example.com"}, "iss_mismatch"},
		{"iss with a trailing slash", map[string]any{"iss": "https://idp.example.com/"}, "iss_mismatch"},
		{"iss missing", map[string]any{"iss": nil}, "iss_mismatch"},
		{"iss not a string", map[string]any{"iss": json.Number("1")}, "iss_mismatch"},
		{"aud string of another client", map[string]any{"aud": "client-2"}, "aud_mismatch"},
		{"aud array containing the client", map[string]any{"aud": []any{"client-1"}}, ""},
		{"aud array with several audiences and azp", map[string]any{"aud": []any{"client-1", "api"}, "azp": "client-1"}, ""},
		{"aud array with several audiences, no azp", map[string]any{"aud": []any{"client-1", "api"}}, "azp_missing"},
		{"aud array with several audiences, wrong azp", map[string]any{"aud": []any{"client-1", "api"}, "azp": "api"}, "azp_mismatch"},
		{"aud array without the client", map[string]any{"aud": []any{"client-2", "api"}, "azp": "client-1"}, "aud_mismatch"},
		{"aud empty array", map[string]any{"aud": []any{}}, "aud_mismatch"},
		{"aud array with a non-string", map[string]any{"aud": []any{"client-1", json.Number("3")}}, "aud_mismatch"},
		{"aud a number", map[string]any{"aud": json.Number("1")}, "aud_mismatch"},
		{"aud missing", map[string]any{"aud": nil}, "aud_mismatch"},
		{"azp equal with one audience", map[string]any{"azp": "client-1"}, ""},
		{"azp different with one audience", map[string]any{"azp": "client-2"}, "azp_mismatch"},
		{"azp not a string", map[string]any{"azp": json.Number("1")}, "azp_mismatch"},
		{"exp missing", map[string]any{"exp": nil}, "exp_missing"},
		{"exp a string", map[string]any{"exp": "2000000000"}, "exp_invalid"},
		{"exp negative", map[string]any{"exp": json.Number("-5")}, "exp_invalid"},
		{"exp past, beyond the skew", map[string]any{"exp": sec(-ClockSkew - time.Second)}, "expired"},
		{"exp exactly now minus the skew", map[string]any{"exp": sec(-ClockSkew)}, "expired"},
		{"exp past, within the skew", map[string]any{"exp": sec(-ClockSkew + time.Second)}, ""},
		{"exp fractional", map[string]any{"exp": json.Number(strconv.FormatInt(testNow.Unix()+60, 10) + ".5")}, ""},
		{"iat in the future, beyond the skew", map[string]any{"iat": sec(ClockSkew + time.Second)}, "iat_future"},
		{"iat in the future, within the skew", map[string]any{"iat": sec(ClockSkew)}, ""},
		{"iat missing is tolerated", map[string]any{"iat": nil}, ""},
		{"iat not a number", map[string]any{"iat": "now"}, "iat_invalid"},
		{"nbf in the future, beyond the skew", map[string]any{"nbf": sec(ClockSkew + time.Minute)}, "nbf_future"},
		{"nbf in the future, within the skew", map[string]any{"nbf": sec(time.Minute)}, ""},
		{"nonce missing", map[string]any{"nonce": nil}, "nonce_missing"},
		{"nonce empty", map[string]any{"nonce": ""}, "nonce_missing"},
		{"nonce of another round trip", map[string]any{"nonce": "n-456"}, "nonce_mismatch"},
		{"nonce not a string", map[string]any{"nonce": json.Number("123")}, "nonce_missing"},
		{"sub missing", map[string]any{"sub": nil}, "sub_missing"},
		{"sub empty", map[string]any{"sub": ""}, "sub_missing"},
		{"sub too long", map[string]any{"sub": strings.Repeat("s", 256)}, "sub_missing"},
		{"sub 255 bytes", map[string]any{"sub": strings.Repeat("s", 255)}, ""},
		{"sub a number", map[string]any{"sub": json.Number("42")}, "sub_missing"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			c := validClaims()
			for k, v := range tc.set {
				if v == nil {
					delete(c, k)
				} else {
					c[k] = v
				}
			}
			got, err := ValidateClaims(c, expect())
			if tc.want == "" {
				if err != nil {
					t.Fatalf("want valid, got %v", err)
				}
				if got.Subject != c["sub"] || got.Nonce != "n-123" {
					t.Fatalf("%+v", got)
				}
				return
			}
			if err == nil || ErrorCode(err) != tc.want {
				t.Fatalf("want %s, got %v", tc.want, err)
			}
		})
	}
	// The round trip must have a nonce at all.
	e := expect()
	e.Nonce = ""
	if _, err := ValidateClaims(validClaims(), e); ErrorCode(err) != "nonce_missing" {
		t.Fatalf("no expected nonce: %v", err)
	}
}

// jwksServer serves keys (a function, so tests can rotate) and counts requests.
type jwksServer struct {
	*httptest.Server
	mu     sync.Mutex
	keys   []map[string]any
	status int
	hits   int
}

func newJWKSServer(t *testing.T, keys ...map[string]any) *jwksServer {
	s := &jwksServer{keys: keys}
	s.Server = httptest.NewTLSServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		s.mu.Lock()
		defer s.mu.Unlock()
		s.hits++
		if s.status != 0 {
			w.WriteHeader(s.status)
			return
		}
		w.Header().Set("Cache-Control", "max-age=600")
		_ = json.NewEncoder(w).Encode(map[string]any{"keys": s.keys})
	}))
	t.Cleanup(s.Close)
	return s
}

func (s *jwksServer) set(status int, keys ...map[string]any) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.status, s.keys = status, keys
}

func (s *jwksServer) count() int {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.hits
}

func newVerifier(srv *jwksServer, clk clock.Clock) *idTokenVerifier {
	return &idTokenVerifier{keys: NewJWKS(srv.URL, SecureClient(srv.Client()), clk), issuers: []string{"https://idp.example.com"},
		clientID: "client-1", now: clk.Now}
}

// Only RS256 and ES256 verify; none, HS* (including the RSA-public-key-as-HMAC-secret confusion)
// and every other algorithm are refused before any key is looked up.
func TestIDTokenAlgorithms(t *testing.T) {
	testKeys(t)
	srv := newJWKSServer(t, rsaJWK("rsa-1", rsaKey1), ecJWK("ec-1", ecKey1))
	v := newVerifier(srv, clock.NewFake(testNow))
	pubPEMish := rsaKey1.N.Bytes() // what an attacker would use as the HMAC secret
	for _, tc := range []struct {
		name   string
		header map[string]any
		key    any
		want   string
	}{
		{"RS256", map[string]any{"alg": "RS256", "kid": "rsa-1"}, rsaKey1, ""},
		{"ES256", map[string]any{"alg": "ES256", "kid": "ec-1"}, ecKey1, ""},
		{"RS256 without a kid", map[string]any{"alg": "RS256"}, rsaKey1, ""},
		{"none", map[string]any{"alg": "none"}, nil, "id_token_alg"},
		{"None", map[string]any{"alg": "None", "kid": "rsa-1"}, nil, "id_token_alg"},
		{"HS256 with the RSA modulus as secret", map[string]any{"alg": "HS256", "kid": "rsa-1"}, pubPEMish, "id_token_alg"},
		{"HS256 with the client secret", map[string]any{"alg": "HS256"}, []byte("client-secret"), "id_token_alg"},
		{"HS512", map[string]any{"alg": "HS512"}, []byte("k"), "id_token_alg"},
		{"RS384", map[string]any{"alg": "RS384", "kid": "rsa-1"}, rsaKey1, "id_token_alg"},
		{"PS256", map[string]any{"alg": "PS256", "kid": "rsa-1"}, rsaKey1, "id_token_alg"},
		{"ES384", map[string]any{"alg": "ES384", "kid": "ec-1"}, ecKey1, "id_token_alg"},
		{"lowercase rs256", map[string]any{"alg": "rs256", "kid": "rsa-1"}, rsaKey1, "id_token_alg"},
		{"no alg", map[string]any{"kid": "rsa-1"}, rsaKey1, "id_token_alg"},
		{"crit header", map[string]any{"alg": "RS256", "kid": "rsa-1", "crit": []string{"exp"}}, rsaKey1, "id_token_crit"},
		{"RS256 header, ES256 signature", map[string]any{"alg": "RS256", "kid": "rsa-1"}, ecKey1, "id_token_signature"},
		{"ES256 header naming the RSA key", map[string]any{"alg": "ES256", "kid": "rsa-1"}, ecKey1, "jwks_unknown_kid"},
		{"signed by another RSA key under a published kid", map[string]any{"alg": "RS256", "kid": "rsa-1"}, rsaKey2, "id_token_signature"},
		{"unknown kid", map[string]any{"alg": "RS256", "kid": "rsa-9"}, rsaKey1, "jwks_unknown_kid"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			tok := sign(t, tc.header, validClaims(), tc.key)
			c, err := v.verify(context.Background(), tok, "n-123")
			if tc.want == "" {
				if err != nil || c.Subject != "user-1" {
					t.Fatalf("want valid: %v", err)
				}
				return
			}
			if ErrorCode(err) != tc.want {
				t.Fatalf("want %s, got %v", tc.want, err)
			}
		})
	}
}

func TestIDTokenTamperingAndMalformed(t *testing.T) {
	testKeys(t)
	srv := newJWKSServer(t, rsaJWK("rsa-1", rsaKey1))
	v := newVerifier(srv, clock.NewFake(testNow))
	good := sign(t, map[string]any{"alg": "RS256", "kid": "rsa-1"}, validClaims(), rsaKey1)
	parts := strings.Split(good, ".")
	evil := validClaims()
	evil["sub"] = "admin"
	evilPayload, _ := json.Marshal(evil)
	for _, tc := range []struct{ name, token, want string }{
		{"payload swapped", parts[0] + "." + enc(evilPayload) + "." + parts[2], "id_token_signature"},
		{"signature stripped", parts[0] + "." + parts[1] + ".", "id_token_signature"},
		{"two parts", parts[0] + "." + parts[1], "id_token_malformed"},
		{"four parts", good + ".x", "id_token_malformed"},
		{"padded base64", parts[0] + "=." + parts[1] + "." + parts[2], "id_token_malformed"},
		{"CR LF inside the signature", parts[0] + "." + parts[1] + "." + parts[2][:10] + "\r\n" + parts[2][10:], "id_token_malformed"},
		{"LF inside the payload", parts[0] + "." + parts[1][:5] + "\n" + parts[1][5:] + "." + parts[2], "id_token_malformed"},
		{"non-zero trailing bits", parts[0] + "." + parts[1] + "." + flipLastBits(parts[2]), "id_token_malformed"},
		{"std base64 alphabet", parts[0] + "." + parts[1] + "." + strings.ReplaceAll(strings.ReplaceAll(parts[2], "-", "+"), "_", "/"), "id_token_malformed"},
		{"header not an object", enc([]byte(`"RS256"`)) + "." + parts[1] + "." + parts[2], "id_token_malformed"},
		{"too long", strings.Repeat("a", maxIDTokenBytes+1), "id_token_malformed"},
		{"empty", "", "id_token_malformed"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			if _, err := v.verify(context.Background(), tc.token, "n-123"); ErrorCode(err) != tc.want {
				t.Fatalf("want %s, got %v", tc.want, err)
			}
		})
	}
	// A valid signature over claims that are not exactly one JSON object.
	cb, _ := json.Marshal(validClaims())
	for _, payload := range []string{`[1,2]`, string(cb) + `}`, string(cb) + `]`, string(cb) + ` {}`, string(cb) + `x`} {
		if _, err := v.verify(context.Background(), signRaw(t, payload), "n-123"); ErrorCode(err) != "id_token_malformed" {
			t.Fatalf("payload %q: %v", payload[max(0, len(payload)-6):], err)
		}
	}
	if _, err := v.verify(context.Background(), signRaw(t, string(cb)+"\n"), "n-123"); err != nil {
		t.Fatalf("trailing whitespace: %v", err)
	}
	// Claims are checked after the signature: the right key, a wrong nonce.
	if _, err := v.verify(context.Background(), good, "other"); ErrorCode(err) != "nonce_mismatch" {
		t.Fatalf("nonce: %v", err)
	}
}

// flipLastBits changes the unused low bits of the last base64url character (a second encoding of
// the same bytes, which strict decoding refuses). The signature is 256 bytes, so its last
// character carries 4 unused bits.
func flipLastBits(s string) string {
	const alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_"
	last := strings.IndexByte(alphabet, s[len(s)-1])
	return s[:len(s)-1] + string(alphabet[last|1])
}

// ES256 accepts only the 64-byte r||s form: a DER signature and zero r or s are refused.
func TestES256SignatureForms(t *testing.T) {
	testKeys(t)
	srv := newJWKSServer(t, ecJWK("ec-1", ecKey1))
	v := newVerifier(srv, clock.NewFake(testNow))
	good := sign(t, map[string]any{"alg": "ES256", "kid": "ec-1"}, validClaims(), ecKey1)
	if _, err := v.verify(context.Background(), good, "n-123"); err != nil {
		t.Fatal(err)
	}
	parts := strings.Split(good, ".")
	raw, _ := base64.RawURLEncoding.DecodeString(parts[2])
	der, err := asn1.Marshal(struct{ R, S *big.Int }{new(big.Int).SetBytes(raw[:32]), new(big.Int).SetBytes(raw[32:])})
	if err != nil {
		t.Fatal(err)
	}
	zeroR := append(make([]byte, 32), raw[32:]...)
	zeroS := append(append([]byte{}, raw[:32]...), make([]byte, 32)...)
	for name, sig := range map[string][]byte{"DER": der, "zeros": make([]byte, 64), "zero r": zeroR, "zero s": zeroS, "63 bytes": raw[:63]} {
		tok := parts[0] + "." + parts[1] + "." + enc(sig)
		if _, err := v.verify(context.Background(), tok, "n-123"); ErrorCode(err) != "id_token_signature" {
			t.Errorf("%s: %v", name, err)
		}
	}
}

// A published key without kid does not match a token that names a kid (and a token without kid
// may use it).
func TestJWKSKeyWithoutKid(t *testing.T) {
	testKeys(t)
	k := rsaJWK("", rsaKey1)
	delete(k, "kid")
	srv := newJWKSServer(t, k)
	v := newVerifier(srv, clock.NewFake(testNow))
	if _, err := v.verify(context.Background(), sign(t, map[string]any{"alg": "RS256", "kid": "rsa-1"}, validClaims(), rsaKey1), "n-123"); ErrorCode(err) != "jwks_unknown_kid" {
		t.Fatalf("token with kid: %v", err)
	}
	if _, err := v.verify(context.Background(), sign(t, map[string]any{"alg": "RS256"}, validClaims(), rsaKey1), "n-123"); err != nil {
		t.Fatalf("token without kid: %v", err)
	}
}

func signRaw(t *testing.T, payload string) string {
	hb, _ := json.Marshal(map[string]any{"alg": "RS256", "kid": "rsa-1"})
	input := enc(hb) + "." + enc([]byte(payload))
	sum := sha256.Sum256([]byte(input))
	sig, err := rsa.SignPKCS1v15(rand.Reader, rsaKey1, crypto.SHA256, sum[:])
	if err != nil {
		t.Fatal(err)
	}
	return input + "." + enc(sig)
}

func TestEmailFromClaims(t *testing.T) {
	for _, tc := range []struct {
		name         string
		claims       map[string]any
		claim        string
		email        string
		wantVerified bool
	}{
		{"verified", map[string]any{"email": "A@Example.com", "email_verified": true}, "email", "a@example.com", true},
		{"unverified", map[string]any{"email": "a@example.com", "email_verified": false}, "email", "a@example.com", false},
		{"verified as a string", map[string]any{"email": "a@example.com", "email_verified": "true"}, "email", "a@example.com", true},
		{"unverified as a string", map[string]any{"email": "a@example.com", "email_verified": "false"}, "email", "a@example.com", false},
		{"email_verified missing", map[string]any{"email": "a@example.com"}, "email", "a@example.com", false},
		{"email_verified a number", map[string]any{"email": "a@example.com", "email_verified": json.Number("1")}, "email", "a@example.com", false},
		{"no email", map[string]any{"email_verified": true}, "email", "", false},
		{"not an address", map[string]any{"email": "nope", "email_verified": true}, "email", "", false},
		{"email not a string", map[string]any{"email": json.Number("1"), "email_verified": true}, "email", "", false},
		{"custom claim equal to the verified email", map[string]any{"upn": "A@corp.example", "email": "a@corp.example", "email_verified": true}, "upn", "a@corp.example", true},
		{"custom claim differing from the verified email", map[string]any{"upn": "boss@corp.example", "email": "a@corp.example", "email_verified": true}, "upn", "boss@corp.example", false},
		{"custom claim without an email claim", map[string]any{"preferred_username": "a@corp.example", "email_verified": true}, "preferred_username", "a@corp.example", false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			email, verified := EmailFromClaims(tc.claims, tc.claim)
			if email != tc.email || verified != tc.wantVerified {
				t.Fatalf("got %q %v, want %q %v", email, verified, tc.email, tc.wantVerified)
			}
		})
	}
}

func TestNameFromClaims(t *testing.T) {
	for _, tc := range []struct {
		claims map[string]any
		want   string
	}{
		{map[string]any{"name": " Ada Lovelace ", "given_name": "X"}, "Ada Lovelace"},
		{map[string]any{"given_name": "Ada", "family_name": "Lovelace"}, "Ada Lovelace"},
		{map[string]any{"given_name": "Ada"}, "Ada"},
		{map[string]any{"preferred_username": "ada"}, "ada"},
		{map[string]any{}, ""},
		{map[string]any{"name": strings.Repeat("é", 150)}, strings.Repeat("é", 100)},
	} {
		if got := nameFromClaims(tc.claims); got != tc.want {
			t.Errorf("%v: %q, want %q", tc.claims, got, tc.want)
		}
	}
}
