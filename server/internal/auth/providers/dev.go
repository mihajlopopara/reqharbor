//go:build devauth

package providers

// The dev sign-in provider (06-go-server.md §5.4): a form that takes any email and name, for the
// Go harness, e2e tests and the live client suite. This file is the only code of the provider and
// is compiled only with -tags devauth, which the image and release builds never pass. Its guards:
// the binary refuses a non-loopback REQHARBOR_LISTEN (config), every request must come from a
// loopback TCP peer (Allow; forwarded headers are never consulted), and dev identities are never
// email_trusted, so they never satisfy REQHARBOR_ADMIN_EMAIL or change users.email.

import (
	"net"
	"net/http"
	"net/netip"
	"net/url"
	"strings"
	"unicode/utf8"

	"github.com/mihajlopopara/reqharbor/server/internal/accounts"
	"github.com/mihajlopopara/reqharbor/server/internal/config"
)

// DevCompiled reports whether this binary contains the dev provider.
const DevCompiled = true

// DevFormPath is where the dev form posts.
const DevFormPath = "/auth/dev"

type dev struct{}

func newDev(cfg *config.Config) Provider {
	if !cfg.DevAuth || !config.IsLoopbackListen(cfg.Listen) {
		return nil
	}
	return dev{}
}

func (dev) ID() string    { return "dev" }
func (dev) Label() string { return "Dev sign-in" }

// Allow accepts only a loopback TCP peer. X-Forwarded-For and the other client-IP headers are
// ignored on purpose: a trusted proxy on loopback would otherwise let remote clients through.
func (dev) Allow(r *http.Request) bool {
	host, _, err := net.SplitHostPort(r.RemoteAddr)
	if err != nil {
		return false
	}
	a, err := netip.ParseAddr(host)
	return err == nil && a.Unmap().IsLoopback()
}

func (dev) Template() string {
	return `{{define "content"}}<h1>Dev sign-in</h1>
<p class="meta">{{if .Device}}On {{.Device}}, started {{.Started}}.{{else}}Started {{.Started}}.{{end}} For automated tests on this machine only.</p>
{{if .Error}}<div class="error" role="alert">{{.Error}}</div>{{end}}
<form method="post" action="` + DevFormPath + `">
<input type="hidden" name="ar" value="{{.AR}}">
<label>Email<input type="email" name="email" required maxlength="254" value="{{.Email}}" autofocus></label>
<label>Name<input type="text" name="name" maxlength="100" value="{{.Name}}"></label>
<label>Subject (optional; defaults to the email)<input type="text" name="subject" maxlength="128" autocomplete="off"></label>
<label>Invite or setup code (when the server needs one)<input class="code" type="text" name="code" maxlength="64" autocomplete="off" spellcheck="false"></label>
<button type="submit">Sign in</button>
</form>
<div class="links"><a href="/auth/authorize?{{.ResumeQuery}}">Back to sign in</a></div>
{{end}}`
}

// FromForm takes any valid email, an optional name and an optional subject. The subject defaults
// to the email, so the same email signs in to the same dev identity; a fixed subject with another
// email plays a provider whose email changed. The email is never trusted.
func (dev) FromForm(form url.Values) (*Profile, string) {
	email := accounts.NormalizeEmail(form.Get("email"))
	if !accounts.ValidEmail(email) {
		return nil, "Enter a valid email address."
	}
	name := strings.TrimSpace(form.Get("name"))
	if utf8.RuneCountInString(name) > accounts.MaxNameRunes {
		return nil, "Use at most 100 characters for your name."
	}
	subject := strings.TrimSpace(form.Get("subject"))
	if subject == "" {
		subject = email
	}
	if len(subject) > 128 {
		return nil, "Use at most 128 characters for the subject."
	}
	return &Profile{Provider: "dev", Subject: subject, Email: email, EmailTrusted: false, Name: name}, ""
}

var _ Form = dev{}
