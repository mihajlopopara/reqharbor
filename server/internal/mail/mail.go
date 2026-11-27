// Package mail sends the server's few emails over SMTP (06-go-server.md §5.5, §5.7, §6, S9):
// signup codes, the "you already have an account" notice and password-reset links.
//
// Messages are plain text, rendered here so that no caller can inject a header: the recipient
// must be a bare address, the subject loses every control character and is RFC 2047 encoded when
// needed, and the body is quoted-printable. Sending is asynchronous (Outbox), so a page never
// waits for the SMTP server and its timing never depends on whether an email was sent.
package mail

import (
	"bytes"
	"crypto/rand"
	"encoding/hex"
	"errors"
	"fmt"
	"mime"
	"mime/quotedprintable"
	netmail "net/mail"
	"strings"
	"time"
	"unicode"
	"unicode/utf8"
)

// Message is one plain-text email to one recipient.
type Message struct {
	To      string // a bare address: a@b.example
	Subject string
	Text    string
}

// maxSubjectRunes bounds the subject (a long REQHARBOR_SERVER_NAME), well under RFC 5322's
// 998-octet line limit even after encoding.
const maxSubjectRunes = 200

// ErrInvalidRecipient is returned for a recipient that is not a single bare address.
var ErrInvalidRecipient = errors.New("mail: the recipient is not a single bare address")

// ParseFrom validates REQHARBOR_SMTP_FROM ("ReqHarbor <rh@acme.dev>" or "rh@acme.dev").
func ParseFrom(s string) (*netmail.Address, error) {
	if strings.ContainsAny(s, "\r\n") {
		return nil, errors.New("contains a line break")
	}
	a, err := netmail.ParseAddress(s)
	if err != nil {
		return nil, err
	}
	if !strings.Contains(a.Address, "@") {
		return nil, errors.New("has no address")
	}
	return a, nil
}

// checkRecipient accepts only a bare address without any character that could end a header or
// an SMTP command.
func checkRecipient(to string) error {
	if to == "" || len(to) > 254 || strings.ContainsFunc(to, func(r rune) bool { return r < 0x21 || r == 0x7f || r == '<' || r == '>' || r == ',' }) {
		return ErrInvalidRecipient
	}
	a, err := netmail.ParseAddress(to)
	if err != nil || a.Name != "" || a.Address != to {
		return ErrInvalidRecipient
	}
	return nil
}

// cleanHeaderText makes s safe as an unstructured header value: control characters (CR and LF
// above all) become spaces, runs of spaces collapse, and it is cut to max runes.
func cleanHeaderText(s string, max int) string {
	s = strings.Map(func(r rune) rune {
		if unicode.IsControl(r) || r == utf8.RuneError {
			return ' '
		}
		return r
	}, s)
	s = strings.Join(strings.Fields(s), " ")
	if utf8.RuneCountInString(s) > max {
		s = string([]rune(s)[:max])
	}
	return s
}

// render builds the RFC 5322 message. It returns ErrInvalidRecipient for a bad recipient.
func render(from *netmail.Address, m Message, now time.Time) ([]byte, error) {
	if err := checkRecipient(m.To); err != nil {
		return nil, err
	}
	var b bytes.Buffer
	header := func(k, v string) { b.WriteString(k + ": " + v + "\r\n") }
	header("From", from.String())
	header("To", m.To)
	header("Subject", mime.QEncoding.Encode("utf-8", cleanHeaderText(m.Subject, maxSubjectRunes)))
	header("Date", now.UTC().Format(time.RFC1123Z))
	header("Message-ID", messageID(from.Address))
	header("MIME-Version", "1.0")
	header("Content-Type", "text/plain; charset=utf-8")
	header("Content-Transfer-Encoding", "quoted-printable")
	header("Auto-Submitted", "auto-generated")
	b.WriteString("\r\n")
	qp := quotedprintable.NewWriter(&b)
	text := strings.ReplaceAll(m.Text, "\r\n", "\n")
	if _, err := qp.Write([]byte(text)); err != nil {
		return nil, err
	}
	if err := qp.Close(); err != nil {
		return nil, err
	}
	if !bytes.HasSuffix(b.Bytes(), []byte("\r\n")) {
		b.WriteString("\r\n")
	}
	return b.Bytes(), nil
}

// messageID is <random@domain of the sender>.
func messageID(from string) string {
	var r [16]byte
	_, _ = rand.Read(r[:])
	domain := "reqharbor.invalid"
	if at := strings.LastIndexByte(from, '@'); at >= 0 && at < len(from)-1 {
		domain = from[at+1:]
	}
	return fmt.Sprintf("<%s@%s>", hex.EncodeToString(r[:]), domain)
}
