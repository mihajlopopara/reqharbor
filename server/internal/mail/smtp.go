package mail

import (
	"context"
	"crypto/tls"
	"errors"
	"fmt"
	"net"
	"net/smtp"
	"net/textproto"
	"strconv"
	"strings"
	"time"
)

// TLS modes (REQHARBOR_SMTP_TLS).
const (
	TLSStartTLS = "starttls" // plain connect, then STARTTLS, which is required
	TLSImplicit = "tls"      // TLS from the first byte (port 465)
	TLSNone     = "none"     // no TLS; config allows it only for a loopback host
)

// Default timeouts of one delivery.
const (
	DefaultDialTimeout    = 10 * time.Second
	DefaultSessionTimeout = 30 * time.Second
)

// SMTPConfig is the SMTP relay of REQHARBOR_SMTP_*.
type SMTPConfig struct {
	Host     string
	Port     int
	Username string
	Password string // never logged; sent only over TLS (or to a loopback host)
	TLS      string // TLSStartTLS, TLSImplicit or TLSNone
	// HeloName is the name sent with EHLO (the public URL's host).
	HeloName string
	// TLSConfig is the base client TLS configuration (tests: a pool trusting the fake server);
	// nil means the system roots. ServerName is always Host.
	TLSConfig *tls.Config
	// DialTimeout bounds the connection; SessionTimeout the whole delivery. Zero means the
	// defaults.
	DialTimeout, SessionTimeout time.Duration
}

// SMTP delivers messages to one relay, one connection per message.
type SMTP struct{ cfg SMTPConfig }

// NewSMTP returns a sender for cfg.
func NewSMTP(cfg SMTPConfig) *SMTP {
	if cfg.DialTimeout <= 0 {
		cfg.DialTimeout = DefaultDialTimeout
	}
	if cfg.SessionTimeout <= 0 {
		cfg.SessionTimeout = DefaultSessionTimeout
	}
	if cfg.HeloName == "" {
		cfg.HeloName = "localhost"
	}
	return &SMTP{cfg: cfg}
}

// Sender delivers one rendered message.
type Sender interface {
	Send(ctx context.Context, from, to string, data []byte) error
}

func (s *SMTP) tlsConfig() *tls.Config {
	var c *tls.Config
	if s.cfg.TLSConfig != nil {
		c = s.cfg.TLSConfig.Clone()
	} else {
		c = &tls.Config{}
	}
	c.ServerName = s.cfg.Host
	if c.MinVersion < tls.VersionTLS12 {
		c.MinVersion = tls.VersionTLS12
	}
	return c
}

// Send delivers data from from to to. Every step shares one deadline (SessionTimeout), and ctx
// cancellation aborts the connection. STARTTLS mode refuses a server that does not offer it, and
// credentials are sent only over TLS or to a loopback host.
func (s *SMTP) Send(ctx context.Context, from, to string, data []byte) error {
	if err := checkRecipient(to); err != nil {
		return err
	}
	if strings.ContainsAny(from, "\r\n<>") {
		return errors.New("mail: invalid sender")
	}
	ctx, cancel := context.WithTimeout(ctx, s.cfg.SessionTimeout)
	defer cancel()
	addr := net.JoinHostPort(s.cfg.Host, strconv.Itoa(s.cfg.Port))
	dialer := &net.Dialer{Timeout: s.cfg.DialTimeout}
	var conn net.Conn
	var err error
	if s.cfg.TLS == TLSImplicit {
		conn, err = (&tls.Dialer{NetDialer: dialer, Config: s.tlsConfig()}).DialContext(ctx, "tcp", addr)
	} else {
		conn, err = dialer.DialContext(ctx, "tcp", addr)
	}
	if err != nil {
		return fmt.Errorf("connect to %s: %w", addr, err)
	}
	deadline, _ := ctx.Deadline()
	_ = conn.SetDeadline(deadline)
	stop := context.AfterFunc(ctx, func() { _ = conn.SetDeadline(time.Unix(1, 0)) })
	defer stop()

	c, err := smtp.NewClient(conn, s.cfg.Host)
	if err != nil {
		conn.Close()
		return describe("greeting", err)
	}
	defer c.Close()
	if err := c.Hello(s.cfg.HeloName); err != nil {
		return describe("EHLO", err)
	}
	if s.cfg.TLS == TLSStartTLS {
		if ok, _ := c.Extension("STARTTLS"); !ok {
			return errors.New("the SMTP server does not offer STARTTLS; use REQHARBOR_SMTP_TLS=tls for port 465")
		}
		if err := c.StartTLS(s.tlsConfig()); err != nil {
			return describe("STARTTLS", err)
		}
	}
	if s.cfg.Username != "" {
		ok, mechs := c.Extension("AUTH")
		if !ok {
			return errors.New("the SMTP server does not offer AUTH; unset REQHARBOR_SMTP_USERNAME or check the port")
		}
		var a smtp.Auth
		switch fields := strings.Fields(strings.ToUpper(mechs)); {
		case contains(fields, "PLAIN"):
			a = smtp.PlainAuth("", s.cfg.Username, s.cfg.Password, s.cfg.Host)
		case contains(fields, "LOGIN"):
			a = &loginAuth{user: s.cfg.Username, pass: s.cfg.Password, host: s.cfg.Host}
		default:
			return fmt.Errorf("the SMTP server offers no supported AUTH mechanism (PLAIN or LOGIN), only %q", mechs)
		}
		if err := c.Auth(a); err != nil {
			return describe("AUTH", err)
		}
	}
	if err := c.Mail(from); err != nil {
		return describe("MAIL FROM", err)
	}
	if err := c.Rcpt(to); err != nil {
		return describe("RCPT TO", err)
	}
	w, err := c.Data()
	if err != nil {
		return describe("DATA", err)
	}
	if _, err := w.Write(data); err != nil {
		return describe("DATA", err)
	}
	if err := w.Close(); err != nil {
		return describe("DATA", err)
	}
	_ = c.Quit() // the message is accepted; a failed QUIT changes nothing
	return nil
}

func contains(list []string, s string) bool {
	for _, v := range list {
		if v == s {
			return true
		}
	}
	return false
}

// SMTPError is a rejection by the SMTP server. Its message keeps only the step and the reply
// code: the reply text may echo the recipient address, which must not reach the logs.
type SMTPError struct {
	Step string
	Code int
}

func (e *SMTPError) Error() string {
	return fmt.Sprintf("the SMTP server refused %s with code %d", e.Step, e.Code)
}

func describe(step string, err error) error {
	var tp *textproto.Error
	if errors.As(err, &tp) {
		return &SMTPError{Step: step, Code: tp.Code}
	}
	return fmt.Errorf("SMTP %s: %w", step, err)
}

// loginAuth is AUTH LOGIN, for relays that do not offer PLAIN (some Microsoft 365 and Exchange
// setups). Like smtp.PlainAuth it refuses to send credentials without TLS unless the host is
// loopback.
type loginAuth struct {
	user, pass, host string
	step             int
}

func (a *loginAuth) Start(server *smtp.ServerInfo) (string, []byte, error) {
	if !server.TLS && !isLoopback(server.Name) {
		return "", nil, errors.New("unencrypted connection")
	}
	if server.Name != a.host {
		return "", nil, errors.New("wrong host name")
	}
	return "LOGIN", nil, nil
}

func (a *loginAuth) Next(_ []byte, more bool) ([]byte, error) {
	if !more {
		return nil, nil
	}
	a.step++
	switch a.step {
	case 1:
		return []byte(a.user), nil
	case 2:
		return []byte(a.pass), nil
	}
	return nil, errors.New("unexpected AUTH LOGIN challenge")
}

func isLoopback(host string) bool {
	if strings.EqualFold(host, "localhost") {
		return true
	}
	ip := net.ParseIP(strings.Trim(host, "[]"))
	return ip != nil && ip.IsLoopback()
}
