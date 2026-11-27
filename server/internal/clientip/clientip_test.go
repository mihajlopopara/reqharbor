package clientip

import (
	"bytes"
	"fmt"
	"log/slog"
	"net/http/httptest"
	"net/netip"
	"strings"
	"testing"
)

func TestBucket_QA_R1_025(t *testing.T) {
	for in, want := range map[string]string{
		"203.0.113.7":     "203.0.113.7",
		"2001:db8:1:2::1": "2001:db8:1:2::/64",
		"2001:0db8:0001:0002:ffff:ffff:ffff:ffff": "2001:db8:1:2::/64",
		"[2001:DB8:1:2:3:4:5:6]":                  "2001:db8:1:2::/64",
		"2001:db8:1:3::1":                         "2001:db8:1:3::/64",
		"::ffff:198.51.100.9":                     "198.51.100.9",
		"::1":                                     "0:0:0:0::/64",
		"fe80::1%eth0":                            "fe80:0:0:0::/64",
		"not-an-ip:zz":                            "not-an-ip:zz",
		" 198.51.100.1 ":                          "198.51.100.1",
		"unknown":                                 "unknown",
	} {
		if got := Bucket(in); got != want {
			t.Errorf("Bucket(%q) = %q, want %q", in, got, want)
		}
	}
}

func resolver(t *testing.T, trusted, header string, logs *bytes.Buffer) *Resolver {
	t.Helper()
	var prefixes []netip.Prefix
	for _, p := range strings.Split(trusted, ",") {
		if p != "" {
			prefixes = append(prefixes, netip.MustParsePrefix(p))
		}
	}
	return New(prefixes, header, slog.New(slog.NewTextHandler(logs, nil)), false)
}

func TestClientIP_TrustedProxies(t *testing.T) {
	var logs bytes.Buffer
	xff := resolver(t, "10.0.0.0/24,2001:db8::/48", "", &logs)
	cases := []struct {
		remote  string
		headers []string
		want    string
	}{
		{"10.0.0.5:1234", []string{"X-Forwarded-For", "198.51.100.1"}, "198.51.100.1"},
		// Right-most address that is not itself trusted; earlier hops may be forged.
		{"10.0.0.5:1234", []string{"X-Forwarded-For", "6.6.6.6, 198.51.100.1, 10.0.0.9"}, "198.51.100.1"},
		{"10.0.0.5:1234", []string{"X-Forwarded-For", "6.6.6.6", "X-Forwarded-For", "198.51.100.2"}, "198.51.100.2"},
		{"10.0.0.5:1234", []string{"X-Forwarded-For", "10.0.0.7, 10.0.0.8"}, "10.0.0.7"},
		{"10.0.0.5:1234", []string{"X-Forwarded-For", "[2001:db9::1]:443"}, "2001:db9::1"},
		{"10.0.0.5:1234", nil, "10.0.0.5"},
		{"[2001:db8::5]:1", []string{"X-Forwarded-For", "::ffff:198.51.100.3"}, "198.51.100.3"},
		// Untrusted peer: the header is ignored.
		{"192.0.2.10:1234", []string{"X-Forwarded-For", "198.51.100.1"}, "192.0.2.10"},
		{"[::ffff:10.0.0.5]:1", []string{"X-Forwarded-For", "198.51.100.4"}, "198.51.100.4"},
	}
	for _, c := range cases {
		r := httptest.NewRequest("GET", "/", nil)
		r.RemoteAddr = c.remote
		for i := 0; i+1 < len(c.headers); i += 2 {
			r.Header.Add(c.headers[i], c.headers[i+1])
		}
		if got := xff.ClientIP(r); got != c.want {
			t.Errorf("%s %v → %q, want %q", c.remote, c.headers, got, c.want)
		}
	}

	cf := resolver(t, "127.0.0.1/32", "CF-Connecting-IP", &logs)
	r := httptest.NewRequest("GET", "/", nil)
	r.RemoteAddr = "127.0.0.1:5"
	r.Header.Set("CF-Connecting-IP", " 2001:db8:1:2::9 ")
	r.Header.Set("X-Forwarded-For", "6.6.6.6")
	if got := cf.ClientIP(r); got != "2001:db8:1:2::9" {
		t.Errorf("single-value header: %q", got)
	}
}

func TestClientIP_OneTimeWarnings(t *testing.T) {
	var logs bytes.Buffer
	res := resolver(t, "", "", &logs)
	for i := 0; i < 3; i++ {
		r := httptest.NewRequest("GET", "/", nil)
		r.RemoteAddr = "192.0.2.10:1"
		r.Header.Set("X-Forwarded-For", "1.2.3.4")
		res.ClientIP(r)
	}
	if n := strings.Count(logs.String(), "untrusted peer carried X-Forwarded-For"); n != 1 {
		t.Fatalf("header warning %d times: %s", n, logs.String())
	}
	if strings.Contains(logs.String(), "192.0.2.10") {
		t.Fatal("peer logged without LOG_CLIENT_IP")
	}
	// 200 requests from one untrusted, non-loopback peer: warn once.
	for i := 0; i < 400; i++ {
		r := httptest.NewRequest("GET", "/", nil)
		r.RemoteAddr = "192.0.2.10:1"
		res.ClientIP(r)
	}
	if n := strings.Count(logs.String(), "every request so far came from one address"); n != 1 {
		t.Fatalf("one-peer warning %d times", n)
	}

	// Mixed peers, or loopback traffic, never warn.
	for _, peers := range [][]string{{"192.0.2.10:1", "192.0.2.11:1"}, {"127.0.0.1:1"}} {
		var l bytes.Buffer
		r2 := resolver(t, "", "", &l)
		for i := 0; i < 300; i++ {
			r := httptest.NewRequest("GET", "/", nil)
			r.RemoteAddr = peers[i%len(peers)]
			r2.ClientIP(r)
		}
		if strings.Contains(l.String(), "every request so far") {
			t.Fatalf("%v: warned", peers)
		}
	}
	// With LOG_CLIENT_IP the peer appears.
	var l bytes.Buffer
	withIP := New(nil, "", slog.New(slog.NewTextHandler(&l, nil)), true)
	r := httptest.NewRequest("GET", "/", nil)
	r.RemoteAddr = "192.0.2.77:1"
	r.Header.Set("X-Forwarded-For", "1.2.3.4")
	withIP.ClientIP(r)
	if !strings.Contains(l.String(), "192.0.2.77") {
		t.Fatal(fmt.Sprint("peer missing: ", l.String()))
	}
}
