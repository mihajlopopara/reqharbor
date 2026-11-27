package mail_test

import (
	"context"
	"errors"
	"log/slog"
	"strings"
	"testing"
	"time"

	"github.com/mihajlopopara/reqharbor/server/internal/clock"
	"github.com/mihajlopopara/reqharbor/server/internal/mail"
	tu "github.com/mihajlopopara/reqharbor/server/internal/testutil"
)

func sender(s *tu.SMTPServer, mode string) mail.SMTPConfig {
	c := mail.SMTPConfig{Host: s.Host, Port: s.Port, TLS: mode, HeloName: "rh.acme.test", TLSConfig: s.ClientTLS,
		DialTimeout: 2 * time.Second, SessionTimeout: 5 * time.Second}
	return c
}

type outboxEnv struct {
	box  *mail.Outbox
	logs *tu.Buffer
	stop func()
}

func newOutbox(t *testing.T, s mail.Sender, perRcpt, perHour int64) outboxEnv {
	t.Helper()
	from, err := mail.ParseFrom(`"Acme, Inc. ReqHarbor" <rh@acme.test>`)
	if err != nil {
		t.Fatal(err)
	}
	logs := &tu.Buffer{}
	box := mail.NewOutbox(mail.OutboxConfig{Sender: s, From: from, Clock: clock.NewFake(time.Date(2026, 11, 27, 12, 0, 0, 0, time.UTC)),
		Logger: slog.New(slog.NewJSONHandler(logs, nil)), PerRecipientPerHour: perRcpt, PerHour: perHour})
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan struct{})
	go func() { defer close(done); box.Run(ctx) }()
	env := outboxEnv{box: box, logs: logs, stop: func() { cancel(); <-done }}
	t.Cleanup(env.stop)
	return env
}

func flush(t *testing.T, box *mail.Outbox) {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	if !box.Flush(ctx) {
		t.Fatal("the outbox did not drain")
	}
}

func TestSMTP_StartTLSThenAuthPlain(t *testing.T) {
	s := tu.NewSMTPServer(t, tu.SMTPOptions{Username: "apikey", Password: "pa55word"})
	c := sender(s, mail.TLSStartTLS)
	c.Username, c.Password = "apikey", "pa55word"
	env := newOutbox(t, mail.NewSMTP(c), 5, 200)
	env.box.Send("test", mail.Message{To: "Bea@Example.test", Subject: "Hello", Text: "Line one\n.dot line\n"})
	flush(t, env.box)
	msgs := s.Wait(t, 1)
	m := msgs[0]
	if !m.TLS || m.Auth != "apikey" || m.From != "rh@acme.test" || len(m.To) != 1 || m.To[0] != "bea@example.test" {
		t.Fatalf("message %+v", m)
	}
	if log := s.AuthLog(); len(log) != 1 || !strings.HasPrefix(log[0], "PLAIN user=apikey") || !strings.HasSuffix(log[0], "tls=true") {
		t.Fatalf("auth %v", log)
	}
	if m.Body != "Line one\n.dot line\n" {
		t.Fatalf("body %q", m.Body)
	}
	for k, want := range map[string]string{
		"Content-Type": "text/plain; charset=utf-8", "Content-Transfer-Encoding": "quoted-printable", "MIME-Version": "1.0",
		"Auto-Submitted": "auto-generated", "To": "bea@example.test",
	} {
		if got := m.Header.Get(k); got != want {
			t.Errorf("%s: %q, want %q", k, got, want)
		}
	}
	if from := m.Header.Get("From"); from != `"Acme, Inc. ReqHarbor" <rh@acme.test>` {
		t.Errorf("From %q", from)
	}
	if id := m.Header.Get("Message-Id"); !strings.HasSuffix(id, "@acme.test>") {
		t.Errorf("Message-ID %q", id)
	}
	if d, err := m.Header.Date(); err != nil || !d.Equal(time.Date(2026, 11, 27, 12, 0, 0, 0, time.UTC)) {
		t.Errorf("Date %v %v", d, err)
	}
	if strings.Contains(env.logs.String(), "pa55word") || strings.Contains(env.logs.String(), "bea@example.test") {
		t.Fatalf("logs carry the password or the address: %s", env.logs)
	}
}

func TestSMTP_AuthLoginWhenPlainIsNotOffered(t *testing.T) {
	s := tu.NewSMTPServer(t, tu.SMTPOptions{Username: "u@acme.test", Password: "secret pw", AuthMechs: "LOGIN"})
	c := sender(s, mail.TLSStartTLS)
	c.Username, c.Password = "u@acme.test", "secret pw"
	if err := mail.NewSMTP(c).Send(context.Background(), "rh@acme.test", "x@example.test", []byte("Subject: x\r\n\r\nhi\r\n")); err != nil {
		t.Fatal(err)
	}
	if log := s.AuthLog(); len(log) != 1 || log[0] != "LOGIN user=u@acme.test pass=secret pw tls=true" {
		t.Fatalf("auth %v", log)
	}
}

func TestSMTP_ImplicitTLS(t *testing.T) {
	s := tu.NewSMTPServer(t, tu.SMTPOptions{Mode: "tls", Username: "u", Password: "p"})
	c := sender(s, mail.TLSImplicit)
	c.Username, c.Password = "u", "p"
	if err := mail.NewSMTP(c).Send(context.Background(), "rh@acme.test", "x@example.test", []byte("Subject: x\r\n\r\nhi\r\n")); err != nil {
		t.Fatal(err)
	}
	if m := s.Wait(t, 1)[0]; !m.TLS || m.Auth != "u" {
		t.Fatalf("%+v", m)
	}
	// An untrusted certificate is refused.
	bad := sender(s, mail.TLSImplicit)
	bad.TLSConfig = nil
	if err := mail.NewSMTP(bad).Send(context.Background(), "rh@acme.test", "x@example.test", []byte("x")); err == nil {
		t.Fatal("an untrusted certificate was accepted")
	}
}

func TestSMTP_StartTLSIsRequired(t *testing.T) {
	// The relay offers no STARTTLS: nothing is sent, and the credentials never leave.
	s := tu.NewSMTPServer(t, tu.SMTPOptions{Mode: "none", Username: "u", Password: "p"})
	c := sender(s, mail.TLSStartTLS)
	c.Username, c.Password = "u", "p"
	err := mail.NewSMTP(c).Send(context.Background(), "rh@acme.test", "x@example.test", []byte("x"))
	if err == nil || !strings.Contains(err.Error(), "does not offer STARTTLS") {
		t.Fatalf("err %v", err)
	}
	if len(s.Messages()) != 0 || len(s.AuthLog()) != 0 {
		t.Fatal("sent without TLS")
	}
	// TLS=none (allowed by config only for a loopback host) sends in the clear.
	c = sender(s, mail.TLSNone)
	c.Username, c.Password = "u", "p"
	if err := mail.NewSMTP(c).Send(context.Background(), "rh@acme.test", "x@example.test", []byte("Subject: x\r\n\r\nhi\r\n")); err != nil {
		t.Fatal(err)
	}
	if m := s.Wait(t, 1)[0]; m.TLS || m.Auth != "u" {
		t.Fatalf("%+v", m)
	}
}

func TestSMTP_Timeouts(t *testing.T) {
	s := tu.NewSMTPServer(t, tu.SMTPOptions{Stall: true})
	c := sender(s, mail.TLSStartTLS)
	c.SessionTimeout = 300 * time.Millisecond
	start := time.Now()
	err := mail.NewSMTP(c).Send(context.Background(), "rh@acme.test", "x@example.test", []byte("x"))
	if err == nil || time.Since(start) > 3*time.Second {
		t.Fatalf("a stalled relay: %v after %v", err, time.Since(start))
	}
	// Cancelling the context aborts at once too.
	c.SessionTimeout = time.Minute
	ctx, cancel := context.WithCancel(context.Background())
	time.AfterFunc(200*time.Millisecond, cancel)
	start = time.Now()
	if err := mail.NewSMTP(c).Send(ctx, "rh@acme.test", "x@example.test", []byte("x")); err == nil || time.Since(start) > 3*time.Second {
		t.Fatalf("cancel: %v after %v", err, time.Since(start))
	}
}

func TestSMTP_RejectionsKeepTheAddressOutOfErrors(t *testing.T) {
	s := tu.NewSMTPServer(t, tu.SMTPOptions{RejectRcpt: true})
	err := mail.NewSMTP(sender(s, mail.TLSStartTLS)).Send(context.Background(), "rh@acme.test", "victim@example.test", []byte("x"))
	var se *mail.SMTPError
	if !errors.As(err, &se) || se.Code != 550 || se.Step != "RCPT TO" || strings.Contains(err.Error(), "victim") {
		t.Fatalf("err %v", err)
	}
	// Wrong credentials: 535, and neither the error nor the outbox log carries the password.
	s = tu.NewSMTPServer(t, tu.SMTPOptions{Username: "u", Password: "right"})
	c := sender(s, mail.TLSStartTLS)
	c.Username, c.Password = "u", "wr0ng-pa55"
	env := newOutbox(t, mail.NewSMTP(c), 5, 200)
	env.box.Send("signup_code", mail.Message{To: "victim@example.test", Subject: "x", Text: "x"})
	flush(t, env.box)
	logs := env.logs.String()
	if !strings.Contains(logs, `"msg":"email_failed"`) || !strings.Contains(logs, "code 535") || strings.Contains(logs, "wr0ng-pa55") || strings.Contains(logs, "victim") {
		t.Fatalf("logs %s", logs)
	}
}

func TestOutbox_HeaderInjection(t *testing.T) {
	s := tu.NewSMTPServer(t, tu.SMTPOptions{})
	env := newOutbox(t, mail.NewSMTP(sender(s, mail.TLSStartTLS)), 50, 200)
	// A subject built from REQHARBOR_SERVER_NAME with line breaks stays one header.
	env.box.Send("t", mail.Message{To: "a@example.test", Subject: "Hi\r\nBcc: evil@example.test\nX-Evil: 1", Text: "body\r\nBcc: no@example.test\n"})
	// A recipient with a line break, a display name or a list is never sent.
	for _, to := range []string{"a@example.test\r\nBcc: evil@example.test", "Bea <a@example.test>", "a@example.test, b@example.test", "<a@example.test>", ""} {
		if env.box.Send("t", mail.Message{To: to, Subject: "x", Text: "x"}) {
			t.Errorf("recipient %q was queued", to)
		}
	}
	// Non-ASCII subjects are RFC 2047 encoded.
	env.box.Send("t", mail.Message{To: "b@example.test", Subject: "Café — Zürich ReqHarbor", Text: "Grüße " + strings.Repeat("x", 200)})
	flush(t, env.box)
	msgs := s.Wait(t, 2)
	if len(msgs) != 2 {
		t.Fatalf("%d messages", len(msgs))
	}
	m := msgs[0]
	if len(m.To) != 1 || m.Header.Get("Bcc") != "" || m.Header.Get("X-Evil") != "" || m.Subject() != "Hi Bcc: evil@example.test X-Evil: 1" {
		t.Fatalf("injected: to=%v headers=%v", m.To, m.Header)
	}
	if !strings.Contains(m.Body, "Bcc: no@example.test") {
		t.Fatalf("body %q", m.Body)
	}
	m = msgs[1]
	if raw := m.Header.Get("Subject"); !strings.HasPrefix(raw, "=?utf-8?q?") || m.Subject() != "Café — Zürich ReqHarbor" {
		t.Fatalf("subject %q -> %q", raw, m.Subject())
	}
	if m.Body != "Grüße "+strings.Repeat("x", 200)+"\n" {
		t.Fatalf("body %q", m.Body)
	}
	for _, line := range strings.Split(m.Raw, "\r\n") {
		if len(line) > 78 {
			t.Fatalf("line of %d characters: %q", len(line), line)
		}
	}
	if !strings.Contains(env.logs.String(), `"msg":"email_invalid"`) {
		t.Fatal("no email_invalid log")
	}
}

func TestOutbox_CapsSuppressWithoutLoggingTheAddress(t *testing.T) {
	s := tu.NewSMTPServer(t, tu.SMTPOptions{})
	env := newOutbox(t, mail.NewSMTP(sender(s, mail.TLSStartTLS)), 2, 3)
	sent := 0
	for i := 0; i < 3; i++ {
		if env.box.Send("signup_code", mail.Message{To: "same@example.test", Subject: "x", Text: "x"}) {
			sent++
		}
	}
	if sent != 2 {
		t.Fatalf("per recipient: %d sent, want 2", sent)
	}
	// The address is compared normalised.
	if env.box.Send("reset_link", mail.Message{To: " SAME@example.test", Subject: "x", Text: "x"}) {
		t.Fatal("the per-recipient cap ignored a differently cased address")
	}
	if !env.box.Send("reset_link", mail.Message{To: "other@example.test", Subject: "x", Text: "x"}) {
		t.Fatal("another recipient was refused")
	}
	if env.box.Send("reset_link", mail.Message{To: "third@example.test", Subject: "x", Text: "x"}) {
		t.Fatal("the server cap (3 / h) let a fourth email through")
	}
	flush(t, env.box)
	if n := len(s.Messages()); n != 3 {
		t.Fatalf("%d delivered", n)
	}
	logs := env.logs.String()
	if strings.Count(logs, `"msg":"email_suppressed"`) != 3 || !strings.Contains(logs, `"cap":"per_recipient"`) || !strings.Contains(logs, `"cap":"server"`) ||
		!strings.Contains(logs, `"purpose":"reset_link"`) {
		t.Fatalf("logs %s", logs)
	}
	for _, addr := range []string{"same@", "other@", "third@"} {
		if strings.Contains(logs, addr) {
			t.Fatalf("logs carry %s: %s", addr, logs)
		}
	}
}

func TestOutbox_ShutdownDropsWhatIsQueued(t *testing.T) {
	s := tu.NewSMTPServer(t, tu.SMTPOptions{Stall: true})
	c := sender(s, mail.TLSStartTLS)
	c.SessionTimeout = time.Minute
	env := newOutbox(t, mail.NewSMTP(c), 10, 10)
	for i := 0; i < 3; i++ {
		env.box.Send("t", mail.Message{To: "a@example.test", Subject: "x", Text: "x"})
	}
	time.Sleep(100 * time.Millisecond)
	done := make(chan struct{})
	go func() { env.stop(); close(done) }()
	select {
	case <-done:
	case <-time.After(5 * time.Second):
		t.Fatal("Run did not stop while a delivery hung")
	}
	ctx, cancel := context.WithTimeout(context.Background(), time.Second)
	defer cancel()
	if !env.box.Flush(ctx) {
		t.Fatal("pending count is off after shutdown")
	}
	if !strings.Contains(env.logs.String(), "emails not sent at shutdown") {
		t.Fatalf("logs %s", env.logs)
	}
}

func TestParseFrom(t *testing.T) {
	for _, ok := range []string{"rh@acme.dev", "ReqHarbor <rh@acme.dev>", `"Acme, Inc." <rh@acme.dev>`} {
		if _, err := mail.ParseFrom(ok); err != nil {
			t.Errorf("%q: %v", ok, err)
		}
	}
	for _, bad := range []string{"", "ReqHarbor", "rh@acme.dev\r\nBcc: x@y", "a@b, c@d"} {
		if _, err := mail.ParseFrom(bad); err == nil {
			t.Errorf("%q accepted", bad)
		}
	}
}

func TestOutbox_AFullQueueSpendsNoCap(t *testing.T) {
	s := tu.NewSMTPServer(t, tu.SMTPOptions{})
	from, _ := mail.ParseFrom("rh@acme.test")
	logs := &tu.Buffer{}
	box := mail.NewOutbox(mail.OutboxConfig{Sender: mail.NewSMTP(sender(s, mail.TLSStartTLS)), From: from,
		Clock: clock.NewFake(time.Date(2026, 11, 27, 12, 0, 0, 0, time.UTC)), Logger: slog.New(slog.NewJSONHandler(logs, nil)),
		PerRecipientPerHour: 2, PerHour: 100, Capacity: 1})
	m := mail.Message{To: "a@example.test", Subject: "x", Text: "x"}
	if !box.Send("t", m) {
		t.Fatal("first not queued")
	}
	// The sender is not running: the queue (1) is full, so these are dropped before any cap.
	for i := 0; i < 3; i++ {
		if box.Send("t", m) || box.Later("t", func(context.Context) (*mail.Message, error) { return &m, nil }) {
			t.Fatal("queued past the capacity")
		}
	}
	if !strings.Contains(logs.String(), `"reason":"queue full"`) {
		t.Fatalf("logs %s", logs)
	}
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan struct{})
	go func() { defer close(done); box.Run(ctx) }()
	defer func() { cancel(); <-done }()
	flush(t, box)
	// Only the first send spent the per-recipient cap (2 / h): one more still goes.
	if !box.Send("t", m) {
		t.Fatal("the dropped sends spent the per-recipient cap")
	}
	flush(t, box)
	if n := len(s.Messages()); n != 2 {
		t.Fatalf("%d delivered", n)
	}
}

func TestOutbox_LaterBuildsInTheSenderAndAppliesTheCaps(t *testing.T) {
	s := tu.NewSMTPServer(t, tu.SMTPOptions{})
	env := newOutbox(t, mail.NewSMTP(sender(s, mail.TLSStartTLS)), 1, 100)
	var ran []string
	build := func(to string) func(context.Context) (*mail.Message, error) {
		return func(context.Context) (*mail.Message, error) {
			ran = append(ran, to) // only the sender goroutine runs jobs, one at a time
			if to == "" {
				return nil, nil
			}
			return &mail.Message{To: to, Subject: "x", Text: "later"}, nil
		}
	}
	for _, to := range []string{"b@example.test", "", "b@example.test", "c@example.test"} {
		if !env.box.Later("reset", build(to)) {
			t.Fatal("not queued")
		}
	}
	flush(t, env.box)
	if len(ran) != 4 {
		t.Fatalf("ran %v", ran)
	}
	if len(s.To("b@example.test")) != 1 || len(s.To("c@example.test")) != 1 {
		t.Fatalf("delivered %d", len(s.Messages()))
	}
	if !strings.Contains(env.logs.String(), `"msg":"email_suppressed","purpose":"reset","cap":"per_recipient"`) {
		t.Fatalf("logs %s", env.logs)
	}
}

func TestSMTP_IPv6HostIsDialledWithOneSetOfBrackets(t *testing.T) {
	// Nothing listens there; the error names the address it dialled.
	err := mail.NewSMTP(mail.SMTPConfig{Host: "::1", Port: 1, TLS: mail.TLSNone, DialTimeout: time.Second}).
		Send(context.Background(), "rh@acme.test", "a@example.test", []byte("x"))
	if err == nil || !strings.Contains(err.Error(), "[::1]:1") || strings.Contains(err.Error(), "[[") {
		t.Fatalf("err %v", err)
	}
}
