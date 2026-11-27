//go:build !devauth

package app_test

// S5, default builds (06-go-server.md §5.4 guards (1)–(2)): the dev provider's code and routes
// are absent.

import (
	"net/url"
	"runtime/debug"
	"strings"
	"testing"

	"github.com/mihajlopopara/reqharbor/server/internal/auth/providers"
	"github.com/mihajlopopara/reqharbor/server/internal/config"

	tu "github.com/mihajlopopara/reqharbor/server/internal/testutil"
)

func TestDefaultBuild_HasNoDevProvider(t *testing.T) {
	if config.DevAuthCompiled || providers.DevCompiled {
		t.Fatal("the default build contains the dev provider")
	}
	if bi, ok := debug.ReadBuildInfo(); ok {
		for _, s := range bi.Settings {
			if s.Key == "-tags" && strings.Contains(s.Value, "devauth") {
				t.Fatalf("built with -tags %s", s.Value)
			}
		}
	}
	h := tu.New(t)
	h.CreateAdmin(t, "admin@example.test")
	b := h.Browser()
	ar := b.Authorize(t, tu.NewPKCE())
	for _, r := range []*tu.Resp{
		b.Get(t, "/auth/start/dev?ar="+ar),
		b.Get(t, "/auth/callback/dev?state=x"),
		b.Post(t, "/auth/dev", url.Values{"ar": {ar}, "email": {"dev@example.test"}}),
	} {
		if r.StatusCode != 404 {
			t.Fatalf("dev route answered %d", r.StatusCode)
		}
	}
	if strings.Contains(string(b.Get(t, "/auth/authorize?"+tu.NewPKCE().Query().Encode()).Body), "/auth/start/dev") {
		t.Fatal("the authorize page offers the dev provider")
	}
}
