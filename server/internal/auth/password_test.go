package auth

import (
	"strings"
	"testing"
)

func TestHasherPHCAndRehash(t *testing.T) {
	h := NewHasher(8192)
	phc, err := h.Hash("correct horse battery")
	if err != nil {
		t.Fatal(err)
	}
	if !strings.HasPrefix(phc, "$argon2id$v=19$m=8192,t=3,p=2$") || strings.Count(phc, "$") != 5 {
		t.Fatalf("phc %q", phc)
	}
	if ok, rehash := h.Verify(phc, "correct horse battery"); !ok || rehash {
		t.Fatalf("verify %v %v", ok, rehash)
	}
	if ok, _ := h.Verify(phc, "correct horse batterz"); ok {
		t.Fatal("wrong password accepted")
	}
	// Different parameters: still verifies, and asks for a rehash.
	other := NewHasher(16384)
	if ok, rehash := other.Verify(phc, "correct horse battery"); !ok || !rehash {
		t.Fatalf("other params %v %v", ok, rehash)
	}
	// Malformed hashes never verify (and still cost a hash).
	before := h.Verifies.Load()
	for _, bad := range []string{"", "plain", "$argon2i$v=19$m=8192,t=3,p=2$AA$AA", "$argon2id$v=18$m=8192,t=3,p=2$AA$AA", "$argon2id$v=19$m=0,t=3,p=2$AA$AA"} {
		if ok, _ := h.Verify(bad, "x"); ok {
			t.Fatalf("%q verified", bad)
		}
	}
	h.VerifyDummy("x")
	if h.Verifies.Load() != before+6 {
		t.Fatal("verifications not counted")
	}
}

func TestPKCEAndGraceSeal(t *testing.T) {
	// RFC 7636 appendix B.
	if got := pkceChallenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"); got != "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM" {
		t.Fatal(got)
	}
	ct, err := sealPair("rhr_old", "S1", pendingPair{AccessToken: "a", RefreshToken: "r", AccessExpiresAt: 5})
	if err != nil {
		t.Fatal(err)
	}
	p, err := openPair("rhr_old", "S1", ct)
	if err != nil || p.AccessToken != "a" || p.RefreshToken != "r" || p.AccessExpiresAt != 5 {
		t.Fatalf("open %+v %v", p, err)
	}
	if _, err := openPair("rhr_other", "S1", ct); err == nil {
		t.Fatal("opened with another refresh token")
	}
	if _, err := openPair("rhr_old", "S2", ct); err == nil {
		t.Fatal("opened for another session")
	}
}
