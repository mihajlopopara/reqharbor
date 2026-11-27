package testutil

import (
	"context"
	"testing"
	"time"

	"github.com/mihajlopopara/reqharbor/server/internal/accounts"
	"github.com/mihajlopopara/reqharbor/server/internal/clock"
	"github.com/mihajlopopara/reqharbor/server/internal/ids"
	"github.com/mihajlopopara/reqharbor/server/internal/store"
)

// Team helpers (M2).

// TeamClientVersion is the X-ReqHarbor-Version the team helpers send (team spaces require one).
const TeamClientVersion = "0.1.0"

// QuickUser creates an account directly in the store (as the CLI does, no argon2 and no pages)
// and a live session for it, and returns its tokens. Name is the account's display name.
func (h *Harness) QuickUser(t testing.TB, email, name string) Tokens {
	t.Helper()
	now := h.Clock.Now()
	access := ids.Token("rha_")
	var tk Tokens
	err := h.Store.Write(context.Background(), func(tx store.WriteTx) error {
		created, err := accounts.Create(tx, accounts.CreateParams{Email: email, Name: name, EmailTrusted: true,
			PHC: "$argon2id$v=19$m=8192,t=3,p=2$unused$unused", CLI: true, Force: true, Now: now})
		if err != nil {
			return err
		}
		ms := clock.Millis(now)
		sess := store.Session{ID: ids.ULID(now), UserID: created.User.ID, ClientID: "reqharbor-desktop", DeviceName: "Test",
			AccessHash: ids.SHA256Hex(access), AccessExpiresAt: clock.Millis(now.Add(24 * 365 * time.Hour)),
			RefreshHash: ids.SHA256Hex(ids.Token("rhr_")), CreatedAt: ms, LastUsedDay: ms / 86_400_000,
			IdleExpiresAt: clock.Millis(now.Add(24 * 365 * time.Hour)), ExpiresAt: clock.Millis(now.Add(24 * 365 * time.Hour))}
		if err := tx.InsertSession(sess); err != nil {
			return err
		}
		tk.Access, tk.SessionID = access, sess.ID
		tk.User.ID, tk.User.Email, tk.User.Name, tk.User.IsAdmin = created.User.ID, email, name, created.User.IsAdmin
		return nil
	})
	if err != nil {
		t.Fatalf("quick user %s: %v", email, err)
	}
	return tk
}

// API sends a JSON call with an access token and the team client version header. body nil sends
// no body.
func (h *Harness) API(t testing.TB, method, path, access string, body any) *Resp {
	t.Helper()
	return h.Call(t, path, Req{Method: method, Token: access, JSON: body, Headers: map[string]string{"X-ReqHarbor-Version": TeamClientVersion}})
}
