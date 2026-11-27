package app_test

import (
	"context"
	"encoding/json"
	"errors"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"github.com/mihajlopopara/reqharbor/server/internal/app"
	"github.com/mihajlopopara/reqharbor/server/internal/db"
	"github.com/mihajlopopara/reqharbor/server/internal/store"
	tu "github.com/mihajlopopara/reqharbor/server/internal/testutil"
)

// blockingStore holds the next write transaction once armed, until release is closed.
type blockingStore struct {
	store.Store
	armed atomic.Bool
	// inside blocks inside the write transaction instead of before it, on a context that the
	// request's cancellation does not reach (a write that a closed connection does not stop).
	inside  bool
	entered chan struct{}
	release chan struct{}
}

func newBlockingStore() *blockingStore {
	return &blockingStore{entered: make(chan struct{}), release: make(chan struct{})}
}

func (b *blockingStore) Write(ctx context.Context, fn func(store.WriteTx) error) error {
	if b.inside && b.armed.CompareAndSwap(true, false) {
		return b.Store.Write(context.WithoutCancel(ctx), func(tx store.WriteTx) error {
			close(b.entered)
			<-b.release
			return fn(tx)
		})
	}
	if b.armed.CompareAndSwap(true, false) {
		close(b.entered)
		<-b.release
	}
	return b.Store.Write(ctx, fn)
}

func (b *blockingStore) wrap(s store.Store) store.Store { b.Store = s; return b }

// noKeepAlive makes every request a new connection, so a closed listener shows.
var noKeepAlive = &http.Client{Transport: &http.Transport{DisableKeepAlives: true}, Timeout: 30 * time.Second}

func call(c *http.Client, method, url, token, body string) (int, map[string]any, error) {
	req, err := http.NewRequest(method, url, strings.NewReader(body))
	if err != nil {
		return 0, nil, err
	}
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("CF-Connecting-IP", tu.RandomIP())
	if token != "" {
		req.Header.Set("Authorization", "Bearer "+token)
	}
	res, err := c.Do(req)
	if err != nil {
		return 0, nil, err
	}
	defer res.Body.Close()
	var v map[string]any
	b, _ := io.ReadAll(res.Body)
	_ = json.Unmarshal(b, &v)
	return res.StatusCode, v, nil
}

type pushResult struct {
	code int
	body map[string]any
	err  error
}

// startBlockedPush serves dataDir, creates a workspace, then starts a push whose write
// transaction blocks; it returns once the push is inside the store.
func startBlockedPush(t *testing.T, dataDir string, env []string) (*running, *blockingStore, chan pushResult) {
	return startBlockedPushWith(t, dataDir, env, newBlockingStore())
}

func startBlockedPushWith(t *testing.T, dataDir string, env []string, bs *blockingStore) (*running, *blockingStore, chan pushResult) {
	t.Helper()
	r := startRunWith(t, dataDir, env, app.RunOptions{WrapStore: bs.wrap})
	t.Cleanup(func() {
		select {
		case <-bs.release:
		default:
			close(bs.release)
		}
	})
	waitFor(t, "readyz", func() bool { c, _, _ := get(t, r.base+"/readyz"); return c == 200 })
	code, ws, err := call(noKeepAlive, "POST", r.base+"/v1/workspaces", "", `{"name":"shutdown"}`)
	if err != nil || code != 201 {
		t.Fatalf("create workspace: %d %v", code, err)
	}
	token := ws["token"].(string)
	bs.armed.Store(true)
	pushed := make(chan pushResult, 1)
	go func() {
		c, v, err := call(noKeepAlive, "POST", r.base+"/v1/sync", token,
			`{"changes":[{"type":"request","id":"inflight","updatedAt":1795435200000,"deleted":false,"deviceId":"d","data":{"id":"inflight","name":"n"}}]}`)
		pushed <- pushResult{c, v, err}
	}()
	select {
	case <-bs.entered:
	case <-time.After(10 * time.Second):
		t.Fatal("the push never reached the store")
	}
	return r, bs, pushed
}

func TestRun_GracefulShutdownFinishesAnInFlightPush(t *testing.T) {
	dir := t.TempDir()
	r, bs, pushed := startBlockedPush(t, dir, nil)

	r.stop() // SIGTERM
	select {
	case err := <-r.done:
		t.Fatalf("Run returned with a push in flight: %v", err)
	case <-time.After(300 * time.Millisecond):
	}
	// The listener is closed at once: new connections are refused while the push finishes.
	if _, _, err := call(noKeepAlive, "GET", r.base+"/healthz", "", ""); err == nil {
		t.Fatal("a new connection was accepted during shutdown")
	}
	close(bs.release)
	res := <-pushed
	if res.err != nil || res.code != 200 {
		t.Fatalf("in-flight push: %d %v %v", res.code, res.body, res.err)
	}
	results, _ := res.body["results"].([]any)
	if len(results) != 1 || results[0].(map[string]any)["status"] != "applied" {
		t.Fatalf("push results %v", res.body)
	}
	select {
	case err := <-r.done:
		if err != nil {
			t.Fatalf("Run: %v", err)
		}
	case <-time.After(10 * time.Second):
		t.Fatal("Run did not return after the push finished")
	}
	r.done <- nil // for the cleanup

	// The WAL was folded into the database file and the lock released; the push is durable.
	if fi, err := os.Stat(filepath.Join(dir, db.FileName+"-wal")); err == nil && fi.Size() != 0 {
		t.Fatalf("WAL not checkpointed: %d bytes", fi.Size())
	}
	d, err := db.Open(dir, db.Options{Lock: true})
	if err != nil {
		t.Fatalf("lock not released: %v", err)
	}
	defer d.Close()
	var n int
	if err := d.Reader.QueryRow("SELECT COUNT(*) FROM entities WHERE id = 'inflight'").Scan(&n); err != nil || n != 1 {
		t.Fatalf("pushed entity: %d %v", n, err)
	}
}

func TestRun_ShutdownTimeoutClosesAStuckRequest(t *testing.T) {
	dir := t.TempDir()
	r, bs, pushed := startBlockedPush(t, dir, []string{"REQHARBOR_SHUTDOWN_TIMEOUT=200ms"})
	start := time.Now()
	r.stop()
	select {
	case err := <-r.done:
		if err != nil {
			t.Fatalf("Run: %v", err)
		}
	case <-time.After(10 * time.Second):
		t.Fatal("Run ignored REQHARBOR_SHUTDOWN_TIMEOUT")
	}
	r.done <- nil
	if el := time.Since(start); el < 200*time.Millisecond {
		t.Fatalf("Run returned after %v, before the timeout", el)
	}
	// The stuck request's connection was closed rather than answered.
	res := <-pushed
	if res.err == nil {
		t.Fatalf("stuck push answered %d", res.code)
	}
	close(bs.release)
	// The lock is released even so.
	d, err := db.Open(dir, db.Options{Lock: true})
	if errors.Is(err, db.ErrLocked) {
		t.Fatal("lock still held")
	}
	if err != nil {
		t.Fatal(err)
	}
	d.Close()
}

// A handler cut off by the shutdown timeout may still be inside a write transaction (its
// connection is closed, but that does not stop the goroutine). Run waits, bounded, for the
// writer to go idle before it checkpoints and releases the lock, so a restore or a second serve
// can never start while this process still writes.
func TestRun_ShutdownWaitsForAWriteThatOutlivesTheTimeout(t *testing.T) {
	dir := t.TempDir()
	bs := newBlockingStore()
	bs.inside = true
	r, bs, _ := startBlockedPushWith(t, dir, []string{"REQHARBOR_SHUTDOWN_TIMEOUT=100ms"}, bs)
	r.stop()
	select {
	case err := <-r.done:
		t.Fatalf("Run returned while a write transaction was open: %v", err)
	case <-time.After(600 * time.Millisecond):
	}
	if l, err := db.AcquireLock(dir); err == nil {
		l.Release()
		t.Fatal("the lock was released while a write transaction was open")
	}
	close(bs.release)
	select {
	case err := <-r.done:
		if err != nil {
			t.Fatalf("Run: %v", err)
		}
	case <-time.After(10 * time.Second):
		t.Fatal("Run did not return once the write finished")
	}
	r.done <- nil
	d, err := db.Open(dir, db.Options{Lock: true})
	if err != nil {
		t.Fatalf("lock not released: %v", err)
	}
	defer d.Close()
	var n int
	if err := d.Reader.QueryRow("SELECT COUNT(*) FROM entities WHERE id = 'inflight'").Scan(&n); err != nil || n != 1 {
		t.Fatalf("the write that outlived the timeout: %d %v", n, err)
	}
}
