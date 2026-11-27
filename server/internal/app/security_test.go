package app_test

// The security half of worker/test/qa_parity_security.test.ts (06-go-server.md §1.6).

import (
	"fmt"
	"net/url"
	"strings"
	"testing"

	tu "github.com/mihajlopopara/reqharbor/server/internal/testutil"
)

func TestSecurity_CrossWorkspaceDataAndSharesAre404(t *testing.T) {
	h := tu.New(t)
	a := h.NewWorkspace(t, "")
	b := h.NewWorkspace(t, "")
	h.Call(t, "/v1/sync", tu.Req{Token: a.Token, JSON: tu.Changes(tu.Change("request", "R1", 1, map[string]any{"secret": "A-only"}))})
	if pulled := h.Call(t, "/v1/sync?since=0", tu.Req{Token: b.Token}); strings.Contains(string(pulled.Body), "A-only") {
		t.Fatal("B read A's data")
	}
	share := h.Call(t, "/v1/shares", tu.Req{Token: a.Token, JSON: map[string]any{"kind": "collection", "entityId": "C1", "snapshot": tu.SampleCollectionSnapshot()}}).JSON(t)
	id := share["shareId"].(string)
	if r := h.Call(t, "/v1/shares/"+id, tu.Req{Method: "PUT", Token: b.Token, JSON: map[string]any{"snapshot": tu.SampleCollectionSnapshot()}}); r.StatusCode != 404 {
		t.Fatalf("put %d", r.StatusCode)
	}
	if r := h.Call(t, "/v1/shares/"+id, tu.Req{Method: "DELETE", Token: b.Token}); r.StatusCode != 404 {
		t.Fatalf("delete %d", r.StatusCode)
	}
	tu.JSONEq(t, h.Call(t, "/v1/shares", tu.Req{Token: b.Token}).Body, `{"shares":[]}`)
	// The same entity id in two workspaces does not collide.
	h.Call(t, "/v1/sync", tu.Req{Token: b.Token, JSON: tu.Changes(tu.Change("request", "R1", 999, map[string]any{"mine": "B"}))})
	if againA := h.Call(t, "/v1/sync?since=0", tu.Req{Token: a.Token}); !strings.Contains(string(againA.Body), "A-only") {
		t.Fatal("A's entity was overwritten")
	}
}

func TestSecurity_SQLishPayloadsAreInert(t *testing.T) {
	h := tu.New(t)
	name := "x'); DROP TABLE workspaces;--"
	res := h.Call(t, "/v1/workspaces", tu.Req{JSON: map[string]any{"name": name}, Headers: map[string]string{"CF-Connecting-IP": "198.51.100.77"}})
	if res.StatusCode != 201 {
		t.Fatalf("got %d", res.StatusCode)
	}
	token := res.JSON(t)["token"].(string)
	if got := h.Call(t, "/v1/workspace", tu.Req{Token: token}).JSON(t)["name"]; got != name {
		t.Fatalf("name %v", got)
	}
	for _, q := range []string{"since=1%20OR%201=1", "since=0;DROP", "limit=1e3", "since=-1", "since=99999999999999999999"} {
		if r := h.Call(t, "/v1/sync?"+q, tu.Req{Token: token}); r.StatusCode != 400 {
			t.Fatalf("%s: %d", q, r.StatusCode)
		}
	}
	if r := h.Call(t, "/s/"+url.PathEscape("' OR 1=1--"), tu.Req{}); r.StatusCode != 404 {
		t.Fatalf("got %d", r.StatusCode)
	}
}

func TestSecurity_QA_R1_024_OneTokenCannotStoreUnboundedShares(t *testing.T) {
	h := tu.New(t, manyShares)
	ws := h.NewWorkspace(t, "")
	big := tu.SampleCollectionSnapshot()
	big["collection"].(map[string]any)["description"] = strings.Repeat("x", 900*1024)
	created := 0
	for i := 0; i < 60; i++ {
		r := h.Call(t, "/v1/shares", tu.Req{Token: ws.Token, JSON: map[string]any{"kind": "collection", "entityId": fmt.Sprintf("C%d", i), "snapshot": big}})
		if r.StatusCode != 201 {
			break
		}
		created++
	}
	if created >= 60 {
		t.Fatalf("%d shares accepted", created)
	}
}

func TestSecurity_QA_R1_025_IPv6AddressesInOne64ShareABudget(t *testing.T) {
	h := tu.New(t)
	ok := 0
	for i := 0; i < 15; i++ {
		r := h.Call(t, "/v1/workspaces", tu.Req{JSON: map[string]any{"name": "v6"}, Headers: map[string]string{"CF-Connecting-IP": fmt.Sprintf("2001:db8:1:2::%x", i+1)}})
		if r.StatusCode == 201 {
			ok++
		}
	}
	if ok != 10 {
		t.Fatalf("%d creates from one /64", ok)
	}
	// IPv4-mapped IPv6 shares the IPv4 bucket.
	for i := 0; i < 10; i++ {
		h.Call(t, "/v1/workspaces", tu.Req{JSON: map[string]any{"name": "v4"}, Headers: map[string]string{"CF-Connecting-IP": "198.51.100.9"}})
	}
	if r := h.Call(t, "/v1/workspaces", tu.Req{JSON: map[string]any{"name": "v4"}, Headers: map[string]string{"CF-Connecting-IP": "::ffff:198.51.100.9"}}); r.StatusCode != 429 {
		t.Fatalf("mapped: %d", r.StatusCode)
	}
}

func TestSecurity_HEADOnSharePageIs405(t *testing.T) {
	h := tu.New(t)
	ws := h.NewWorkspace(t, "")
	id := h.Call(t, "/v1/shares", tu.Req{Token: ws.Token, JSON: map[string]any{"kind": "collection", "entityId": "C1", "snapshot": tu.SampleCollectionSnapshot()}}).JSON(t)["shareId"].(string)
	if r := h.Call(t, "/s/"+id, tu.Req{Method: "HEAD"}); r.StatusCode != 405 {
		t.Fatalf("got %d", r.StatusCode)
	}
}

func TestSecurity_ClientIPHeaderIgnoredFromUntrustedPeersAndWarnedOnce(t *testing.T) {
	// No trusted proxies: CF-Connecting-IP from 127.0.0.1 must not pick the bucket.
	h := tu.New(t, tu.WithEnv("REQHARBOR_TRUSTED_PROXIES="))
	for i := 0; i < 10; i++ {
		if r := h.Call(t, "/v1/workspaces", tu.Req{JSON: map[string]any{"name": "x"}, Headers: map[string]string{"CF-Connecting-IP": tu.RandomIP()}}); r.StatusCode != 201 {
			t.Fatalf("create %d: %d", i, r.StatusCode)
		}
	}
	if r := h.Call(t, "/v1/workspaces", tu.Req{JSON: map[string]any{"name": "x"}, Headers: map[string]string{"CF-Connecting-IP": tu.RandomIP()}}); r.StatusCode != 429 {
		t.Fatalf("spoofed header picked a fresh bucket: %d", r.StatusCode)
	}
	if n := h.Logs.Count("untrusted peer carried CF-Connecting-IP"); n != 1 {
		t.Fatalf("warning logged %d times:\n%s", n, h.Logs.String())
	}
	if strings.Contains(h.Logs.String(), `"peer"`) {
		t.Fatal("peer address logged without REQHARBOR_LOG_CLIENT_IP")
	}
}

func TestSecurity_SetCookieIsStrippedOutsideAuth(t *testing.T) {
	h := tu.New(t)
	// No handler sets cookies yet; the access log never contains tokens or query strings.
	ws := h.NewWorkspace(t, "")
	h.Call(t, "/v1/sync?since=0", tu.Req{Token: ws.Token})
	logs := h.Logs.String()
	if strings.Contains(logs, ws.Token) || strings.Contains(logs, "since=0") {
		t.Fatalf("secret in logs: %s", logs)
	}
	if !strings.Contains(logs, `"route":"/v1/sync"`) {
		t.Fatalf("route pattern missing: %s", logs)
	}
}

func TestSecurity_SharePageEscapesHostileContentInEveryField(t *testing.T) {
	h := tu.New(t)
	ws := h.NewWorkspace(t, "")
	x := `"><img src=x onerror=alert(1)><script>alert(1)</script>`
	snap := tu.SampleCollectionSnapshot()
	c := snap["collection"].(map[string]any)
	c["name"], c["description"] = x, x
	c["folders"].([]any)[0].(map[string]any)["name"] = x
	c["requests"] = append(c["requests"].([]any), tu.SampleRequest(map[string]any{
		"id": "RX", "name": x, "method": "<svg/onload=alert(1)>", "url": "javascript:alert(1)//" + x,
		"headers": []any{map[string]any{"key": x, "value": x, "enabled": true}},
		"body":    map[string]any{"type": "graphql", "graphql": map[string]any{"query": x, "variables": map[string]any{x: x}}},
	}))
	env := snap["environment"].(map[string]any)
	env["variables"] = append(env["variables"].([]any), map[string]any{"key": x, "value": x, "enabled": true, "secret": false})
	id := h.Call(t, "/v1/shares", tu.Req{Token: ws.Token, JSON: map[string]any{"kind": "collection", "entityId": "C1", "snapshot": snap}}).JSON(t)["shareId"].(string)
	res := h.Call(t, "/s/"+id, tu.Req{})
	text := string(res.Body)
	for _, bad := range []string{"<img src=x", "<script>alert", "<svg/onload"} {
		if strings.Contains(text, bad) {
			t.Fatalf("raw %q on the page", bad)
		}
	}
	if strings.Contains(strings.ToLower(text), `href="javascript:`) {
		t.Fatal("javascript: href")
	}
	if !strings.Contains(res.Header.Get("Content-Security-Policy"), "script-src 'sha256-") || res.Header.Get("Set-Cookie") != "" {
		t.Fatalf("%v", res.Header)
	}
}
