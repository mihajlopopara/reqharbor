package ids

import (
	"regexp"
	"strings"
	"testing"
	"time"
)

func TestULID(t *testing.T) {
	now := time.UnixMilli(1_800_000_000_123)
	a, b := ULID(now), ULID(now)
	re := regexp.MustCompile(`^[0-9A-HJKMNP-TV-Z]{26}$`)
	if !re.MatchString(a) || a == b || a[:10] != b[:10] {
		t.Fatalf("%s %s", a, b)
	}
	if later := ULID(now.Add(time.Millisecond)); later[:10] <= a[:10] {
		t.Fatal("timestamp prefix not increasing")
	}
	if ULID(time.UnixMilli(0))[:10] != "0000000000" {
		t.Fatal("epoch prefix")
	}
}

func TestShareIDAndTokens(t *testing.T) {
	seen := map[string]bool{}
	for i := 0; i < 200; i++ {
		id := ShareID()
		if !ShareIDRe.MatchString(id) || seen[id] {
			t.Fatalf("%q", id)
		}
		seen[id] = true
	}
	tok := Token("rh_")
	if !LegacyTokenRe.MatchString(tok) {
		t.Fatalf("%q", tok)
	}
	if strings.HasPrefix(Token("rha_"), "rh_") && LegacyTokenRe.MatchString(Token("rha_")) {
		t.Fatal("an access token matches the legacy token pattern")
	}
	if SHA256Hex("abc") != "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad" {
		t.Fatal("sha256")
	}
	if !Equal("a", "a") || Equal("a", "b") || Equal("a", "ab") {
		t.Fatal("Equal")
	}
	if c := Code(4); !regexp.MustCompile(`^[0-9A-HJKMNP-TV-Z]{4}(-[0-9A-HJKMNP-TV-Z]{4}){3}$`).MatchString(c) {
		t.Fatalf("code %q", c)
	}
}
