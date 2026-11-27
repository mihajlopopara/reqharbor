package testutil

import (
	"bufio"
	"context"
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/tls"
	"crypto/x509"
	"crypto/x509/pkix"
	"encoding/base64"
	"fmt"
	"io"
	"math/big"
	"mime"
	"mime/quotedprintable"
	"net"
	netmail "net/mail"
	"net/url"
	"regexp"
	"strconv"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/mihajlopopara/reqharbor/server/internal/config"
)

// SMTPServer is a fake SMTP relay for tests (06-go-server.md §10 S9): net.Listen plus a minimal
// responder for EHLO, STARTTLS, AUTH PLAIN and LOGIN, MAIL, RCPT, DATA, RSET, NOOP and QUIT. It
// records every accepted message.
type SMTPServer struct {
	Host string
	Port int
	// ClientTLS trusts the server's self-signed certificate.
	ClientTLS *tls.Config

	opts      SMTPOptions
	ln        net.Listener
	serverTLS *tls.Config

	mu       sync.Mutex
	messages []SMTPMessage
	conns    int
	authLog  []string // every AUTH payload decoded (tests check what was sent, and over what)
	arrived  chan struct{}
}

// SMTPOptions configure the fake server.
type SMTPOptions struct {
	// Mode is "starttls" (the default: plain, STARTTLS offered), "tls" (implicit TLS) or "none"
	// (plain, no STARTTLS offered).
	Mode string
	// Username and Password, when set, are required (AUTH before MAIL).
	Username, Password string
	// AuthMechs is the AUTH line (default "PLAIN LOGIN").
	AuthMechs string
	// RejectRcpt answers every RCPT with 550 and the address in the text.
	RejectRcpt bool
	// Stall accepts connections and never answers.
	Stall bool
}

// SMTPMessage is one accepted message.
type SMTPMessage struct {
	From string
	To   []string
	Raw  string // the DATA, dot-unstuffed
	TLS  bool   // the session was encrypted when DATA was sent
	Auth string // the authenticated user, if any

	Header netmail.Header
	Body   string // the decoded text/plain body
}

// Subject is the decoded subject.
func (m SMTPMessage) Subject() string {
	s, err := new(mime.WordDecoder).DecodeHeader(m.Header.Get("Subject"))
	if err != nil {
		return m.Header.Get("Subject")
	}
	return s
}

var sixDigitsRe = regexp.MustCompile(`(?m)^\s+(\d{6})\s*$`)

// Code returns the 6-digit signup code in the body, or "".
func (m SMTPMessage) Code() string {
	if s := sixDigitsRe.FindStringSubmatch(m.Body); s != nil {
		return s[1]
	}
	return ""
}

var resetLinkRe = regexp.MustCompile(`https?://\S+/auth/reset#t=([A-Za-z0-9_-]+)`)

// ResetToken returns the token of the reset link in the body, or "".
func (m SMTPMessage) ResetToken() string {
	if s := resetLinkRe.FindStringSubmatch(m.Body); s != nil {
		return s[1]
	}
	return ""
}

// NewSMTPServer starts a fake relay on 127.0.0.1; it stops with the test.
func NewSMTPServer(t testing.TB, opts SMTPOptions) *SMTPServer {
	t.Helper()
	if opts.Mode == "" {
		opts.Mode = "starttls"
	}
	if opts.AuthMechs == "" {
		opts.AuthMechs = "PLAIN LOGIN"
	}
	cert, pool := selfSigned(t)
	s := &SMTPServer{opts: opts, serverTLS: &tls.Config{Certificates: []tls.Certificate{cert}, MinVersion: tls.VersionTLS12},
		ClientTLS: &tls.Config{RootCAs: pool, MinVersion: tls.VersionTLS12}, arrived: make(chan struct{}, 1024)}
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	if opts.Mode == "tls" {
		ln = tls.NewListener(ln, s.serverTLS)
	}
	s.ln = ln
	s.Host = "127.0.0.1"
	s.Port = ln.Addr().(*net.TCPAddr).Port
	var wg sync.WaitGroup
	t.Cleanup(func() { ln.Close(); wg.Wait() })
	wg.Add(1)
	go func() {
		defer wg.Done()
		for {
			c, err := ln.Accept()
			if err != nil {
				return
			}
			wg.Add(1)
			go func() { defer wg.Done(); s.serve(c) }()
		}
	}()
	return s
}

// Env returns the REQHARBOR_SMTP_* settings that point a server at this relay.
func (s *SMTPServer) Env() []string {
	env := []string{
		"REQHARBOR_SMTP_HOST=" + s.Host,
		"REQHARBOR_SMTP_PORT=" + strconv.Itoa(s.Port),
		"REQHARBOR_SMTP_FROM=Acme ReqHarbor <rh@acme.test>",
		"REQHARBOR_SMTP_TLS=" + s.opts.Mode,
	}
	if s.opts.Username != "" {
		env = append(env, "REQHARBOR_SMTP_USERNAME="+s.opts.Username, "REQHARBOR_SMTP_PASSWORD="+s.opts.Password)
	}
	return env
}

// Messages returns a copy of the accepted messages.
func (s *SMTPServer) Messages() []SMTPMessage {
	s.mu.Lock()
	defer s.mu.Unlock()
	return append([]SMTPMessage(nil), s.messages...)
}

// To returns the accepted messages for one recipient.
func (s *SMTPServer) To(addr string) []SMTPMessage {
	var out []SMTPMessage
	for _, m := range s.Messages() {
		for _, r := range m.To {
			if strings.EqualFold(r, addr) {
				out = append(out, m)
			}
		}
	}
	return out
}

// Connections is the number of sessions so far.
func (s *SMTPServer) Connections() int {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.conns
}

// AuthLog returns the decoded AUTH exchanges (mechanism, user, password, whether TLS).
func (s *SMTPServer) AuthLog() []string {
	s.mu.Lock()
	defer s.mu.Unlock()
	return append([]string(nil), s.authLog...)
}

// Wait waits until at least n messages arrived and returns them.
func (s *SMTPServer) Wait(t testing.TB, n int) []SMTPMessage {
	t.Helper()
	deadline := time.After(10 * time.Second)
	for {
		if m := s.Messages(); len(m) >= n {
			return m
		}
		select {
		case <-s.arrived:
		case <-deadline:
			t.Fatalf("fake SMTP: %d messages after 10s, want %d", len(s.Messages()), n)
		}
	}
}

type smtpSession struct {
	s       *SMTPServer
	conn    net.Conn
	r       *bufio.Reader
	isTLS   bool
	user    string
	from    string
	rcpts   []string
	helloed bool
}

func (x *smtpSession) reply(code int, lines ...string) {
	for i, l := range lines {
		sep := " "
		if i < len(lines)-1 {
			sep = "-"
		}
		fmt.Fprintf(x.conn, "%d%s%s\r\n", code, sep, l)
	}
}

func (x *smtpSession) line() (string, error) {
	l, err := x.r.ReadString('\n')
	return strings.TrimRight(l, "\r\n"), err
}

func (s *SMTPServer) serve(c net.Conn) {
	defer c.Close()
	s.mu.Lock()
	s.conns++
	s.mu.Unlock()
	if s.opts.Stall {
		_, _ = io.Copy(io.Discard, c)
		return
	}
	_ = c.SetDeadline(time.Now().Add(30 * time.Second))
	_, isTLS := c.(*tls.Conn)
	x := &smtpSession{s: s, conn: c, r: bufio.NewReader(c), isTLS: isTLS}
	x.reply(220, "fake.smtp.test ESMTP ready")
	for {
		l, err := x.line()
		if err != nil {
			return
		}
		verb, arg, _ := strings.Cut(l, " ")
		switch strings.ToUpper(verb) {
		case "EHLO", "HELO":
			x.helloed = true
			ext := []string{"fake.smtp.test"}
			if s.opts.Mode == "starttls" && !x.isTLS {
				ext = append(ext, "STARTTLS")
			}
			if s.opts.Username != "" && (x.isTLS || s.opts.Mode == "none") {
				ext = append(ext, "AUTH "+s.opts.AuthMechs)
			}
			ext = append(ext, "8BITMIME")
			x.reply(250, ext...)
		case "STARTTLS":
			if s.opts.Mode != "starttls" || x.isTLS {
				x.reply(502, "not available")
				continue
			}
			x.reply(220, "go ahead")
			tc := tls.Server(c, s.serverTLS)
			if err := tc.Handshake(); err != nil {
				return
			}
			x.conn, x.r, x.isTLS, x.helloed = tc, bufio.NewReader(tc), true, false
		case "AUTH":
			if !x.auth(arg) {
				x.reply(535, "5.7.8 authentication failed")
				continue
			}
			x.reply(235, "2.7.0 authenticated")
		case "MAIL":
			if s.opts.Username != "" && x.user == "" {
				x.reply(530, "5.7.0 authentication required")
				continue
			}
			x.from = addrArg(arg)
			x.reply(250, "ok")
		case "RCPT":
			if s.opts.RejectRcpt {
				x.reply(550, "5.1.1 <"+addrArg(arg)+">: recipient address rejected")
				continue
			}
			x.rcpts = append(x.rcpts, addrArg(arg))
			x.reply(250, "ok")
		case "DATA":
			if x.from == "" || len(x.rcpts) == 0 {
				x.reply(503, "need MAIL and RCPT")
				continue
			}
			x.reply(354, "end with .")
			var b strings.Builder
			for {
				dl, err := x.line()
				if err != nil {
					return
				}
				if dl == "." {
					break
				}
				b.WriteString(strings.TrimPrefix(dl, ".") + "\r\n")
			}
			s.accept(SMTPMessage{From: x.from, To: x.rcpts, Raw: b.String(), TLS: x.isTLS, Auth: x.user})
			x.from, x.rcpts = "", nil
			x.reply(250, "queued")
		case "RSET":
			x.from, x.rcpts = "", nil
			x.reply(250, "ok")
		case "NOOP":
			x.reply(250, "ok")
		case "QUIT":
			x.reply(221, "bye")
			return
		default:
			x.reply(502, "unknown command")
		}
	}
}

func (x *smtpSession) auth(arg string) bool {
	mech, rest, _ := strings.Cut(arg, " ")
	var user, pass string
	switch strings.ToUpper(mech) {
	case "PLAIN":
		if rest == "" {
			x.reply(334, "")
			l, err := x.line()
			if err != nil {
				return false
			}
			rest = l
		}
		b, err := base64.StdEncoding.DecodeString(rest)
		if err != nil {
			return false
		}
		parts := strings.Split(string(b), "\x00")
		if len(parts) != 3 {
			return false
		}
		user, pass = parts[1], parts[2]
	case "LOGIN":
		x.reply(334, base64.StdEncoding.EncodeToString([]byte("Username:")))
		l, err := x.line()
		if err != nil {
			return false
		}
		u, _ := base64.StdEncoding.DecodeString(l)
		x.reply(334, base64.StdEncoding.EncodeToString([]byte("Password:")))
		l, err = x.line()
		if err != nil {
			return false
		}
		p, _ := base64.StdEncoding.DecodeString(l)
		user, pass = string(u), string(p)
	default:
		return false
	}
	x.s.mu.Lock()
	x.s.authLog = append(x.s.authLog, fmt.Sprintf("%s user=%s pass=%s tls=%v", strings.ToUpper(mech), user, pass, x.isTLS))
	x.s.mu.Unlock()
	if user != x.s.opts.Username || pass != x.s.opts.Password {
		return false
	}
	x.user = user
	return true
}

func addrArg(arg string) string {
	_, a, _ := strings.Cut(arg, ":")
	a = strings.TrimSpace(a)
	if i := strings.IndexByte(a, ' '); i >= 0 {
		a = a[:i]
	}
	return strings.Trim(a, "<>")
}

func (s *SMTPServer) accept(m SMTPMessage) {
	if msg, err := netmail.ReadMessage(strings.NewReader(m.Raw)); err == nil {
		m.Header = msg.Header
		var body io.Reader = msg.Body
		if strings.EqualFold(msg.Header.Get("Content-Transfer-Encoding"), "quoted-printable") {
			body = quotedprintable.NewReader(body)
		}
		b, _ := io.ReadAll(body)
		m.Body = strings.ReplaceAll(string(b), "\r\n", "\n")
	}
	s.mu.Lock()
	s.messages = append(s.messages, m)
	s.mu.Unlock()
	select {
	case s.arrived <- struct{}{}:
	default:
	}
}

func selfSigned(t testing.TB) (tls.Certificate, *x509.CertPool) {
	t.Helper()
	key, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	tmpl := &x509.Certificate{
		SerialNumber: big.NewInt(1), Subject: pkix.Name{CommonName: "fake smtp"},
		NotBefore: time.Now().Add(-time.Hour), NotAfter: time.Now().Add(24 * time.Hour),
		KeyUsage:    x509.KeyUsageDigitalSignature | x509.KeyUsageCertSign,
		ExtKeyUsage: []x509.ExtKeyUsage{x509.ExtKeyUsageServerAuth}, BasicConstraintsValid: true, IsCA: true,
		IPAddresses: []net.IP{net.ParseIP("127.0.0.1")}, DNSNames: []string{"localhost"},
	}
	der, err := x509.CreateCertificate(rand.Reader, tmpl, tmpl, &key.PublicKey, key)
	if err != nil {
		t.Fatal(err)
	}
	leaf, err := x509.ParseCertificate(der)
	if err != nil {
		t.Fatal(err)
	}
	pool := x509.NewCertPool()
	pool.AddCert(leaf)
	return tls.Certificate{Certificate: [][]byte{der}, PrivateKey: key, Leaf: leaf}, pool
}

// WithSMTP points the server at a fake relay; the harness runs the server's mail sender.
func WithSMTP(s *SMTPServer) Option {
	return func(o *options) {
		o.smtp = s
		o.env = append(o.env, s.Env()...)
		o.config = append(o.config, func(c *config.Config) { c.Test.SMTPTLSConfig = s.ClientTLS })
	}
}

// FlushMail waits until the server handed every queued email to the relay.
func (h *Harness) FlushMail(t testing.TB) {
	t.Helper()
	if h.App.Mail == nil {
		t.Fatal("FlushMail: SMTP is not configured")
	}
	ctx, cancel := context.WithTimeout(context.Background(), 15*time.Second)
	defer cancel()
	if !h.App.Mail.Flush(ctx) {
		t.Fatal("FlushMail: the mail queue did not drain")
	}
}

// LatestCode returns the newest signup code the fake relay received for email.
func (h *Harness) LatestCode(t testing.TB, email string) string {
	t.Helper()
	if h.SMTP == nil {
		t.Fatal("LatestCode: no fake SMTP relay (WithSMTP)")
	}
	h.FlushMail(t)
	msgs := h.SMTP.To(email)
	for i := len(msgs) - 1; i >= 0; i-- {
		if c := msgs[i].Code(); c != "" {
			return c
		}
	}
	t.Fatalf("no signup code was sent to %s", email)
	return ""
}

// Verify posts a signup code.
func (b *Browser) Verify(t testing.TB, ar, code string) *Resp {
	t.Helper()
	return b.Post(t, "/auth/password/verify", url.Values{"ar": {ar}, "code": {code}})
}

// Resend asks for a new signup code.
func (b *Browser) Resend(t testing.TB, ar string) *Resp {
	t.Helper()
	return b.Post(t, "/auth/password/verify", url.Values{"ar": {ar}, "resend": {"1"}})
}

// confirmSignup finishes a signup that SMTP turned into "Check your email" by entering the code
// the relay received. Without SMTP, or for any other response, it returns res unchanged.
func (h *Harness) confirmSignup(t testing.TB, b *Browser, ar, email string, res *Resp) *Resp {
	t.Helper()
	if h.App.Mail == nil || h.SMTP == nil || res.StatusCode != 200 || !strings.Contains(string(res.Body), "Check your email") {
		return res
	}
	return b.Verify(t, ar, h.LatestCode(t, email))
}
