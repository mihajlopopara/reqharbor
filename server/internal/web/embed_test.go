package web

import (
	"strings"
	"testing"
)

// pow.js must not depend on WebCrypto: crypto.subtle is undefined outside secure contexts, so on an
// http PUBLIC_URL that is not loopback (a LAN address, which §5.9 supports) the proof of work could
// never be produced and the slow-down would become a lockout. qa/server_pow_check.js runs the
// script itself in node without WebCrypto and checks the proof.
func TestPowJSWorksWithoutWebCrypto(t *testing.T) {
	for _, bad := range []string{"crypto.subtle", "subtle.digest", "TextEncoder"} {
		if strings.Contains(PowJS, bad) {
			t.Errorf("pow.js uses %s, which is missing on http origins that are not loopback", bad)
		}
	}
	for _, want := range []string{
		"0x428a2f98", "0xc67178f2", // the SHA-256 round constants (first and last)
		"0x6a09e667", "0x5be0cd19", // and initial hash values
		"btn.disabled=true",
		"could not finish the check", // the visible message when no hash can be computed
	} {
		if !strings.Contains(PowJS, want) {
			t.Errorf("pow.js lacks %q", want)
		}
	}
	if strings.Contains(strings.ToLower(PowJS), "</script") {
		t.Error("pow.js would end its inline <script> element")
	}
	if !strings.Contains(AuthCSP, "'sha256-"+HashBase64(PowJS)+"'") {
		t.Error("AuthCSP does not carry the hash of pow.js")
	}
}
