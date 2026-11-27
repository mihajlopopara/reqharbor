package httpx

import (
	"net/http/httptest"
	"testing"
)

func TestCompareVersions(t *testing.T) {
	for _, c := range []struct {
		a, b string
		want int
	}{
		{"0.3.0", "0.3.0", 0}, {"0.3.1", "0.3.0", 1}, {"0.2.9", "0.3.0", -1}, {"1.0.0", "0.99.99", 1},
		{"0.3.0-rc1", "0.3.0", -1}, {"0.3.0", "0.3.0-rc1", 1}, {"0.3.0-rc.2", "0.3.0-rc.10", -1},
		{"0.3.0-alpha", "0.3.0-beta", -1}, {"0.3.0-1", "0.3.0-alpha", -1}, {"v0.3.0", "0.3.0", 0},
		{"0.3.0+build", "0.3.0", 0}, {"garbage", "0.0.1", -1}, {"0.10.0", "0.9.0", 1},
	} {
		if got := CompareVersions(c.a, c.b); got != c.want {
			t.Errorf("CompareVersions(%q, %q) = %d, want %d", c.a, c.b, got, c.want)
		}
	}
}

func TestCheckClientVersion(t *testing.T) {
	r := httptest.NewRequest("POST", "/v1/auth/token", nil)
	if CheckClientVersion(r, "") != nil {
		t.Fatal("empty min gates")
	}
	e, ok := AsAPIError(CheckClientVersion(r, "0.3.0"))
	if !ok || e.Status != 426 || e.Code != "client_too_old" {
		t.Fatalf("missing header: %v", e)
	}
	r.Header.Set(VersionHeader, "0.3.0")
	if CheckClientVersion(r, "0.3.0") != nil {
		t.Fatal("equal version gated")
	}
}
