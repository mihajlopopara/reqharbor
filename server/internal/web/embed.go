// Package web embeds the HTML templates and static assets of the server (06-go-server.md §3).
// Nothing is read from disk at runtime.
package web

import (
	"crypto/sha256"
	"embed"
	"encoding/base64"
	"strings"
)

// Files holds templates/ and static/.
//
//go:embed templates static
var Files embed.FS

func mustRead(name string) string {
	b, err := Files.ReadFile(name)
	if err != nil {
		panic(err)
	}
	return string(b)
}

// The share page's inline assets, byte for byte the Worker's PAGE_CSS, PAGE_JS and LOGO_SVG
// (worker/src/share-page.ts). The page embeds them unchanged, so the CSP hashes below match.
var (
	ShareCSS = mustRead("static/share.css")
	ShareJS  = mustRead("static/share.js")
	LogoSVG  = mustRead("static/logo.svg")
)

// HashBase64 is the base64 SHA-256 of s, as a CSP source expects it.
func HashBase64(s string) string {
	sum := sha256.Sum256([]byte(s))
	return base64.StdEncoding.EncodeToString(sum[:])
}

// ShareCSP is the Content-Security-Policy of the public share pages, built exactly like the
// Worker's sharePageCsp(): only the exact inline style and script may run; nothing else loads.
var ShareCSP = strings.Join([]string{
	"default-src 'none'",
	"script-src 'sha256-" + HashBase64(ShareJS) + "'",
	"style-src 'sha256-" + HashBase64(ShareCSS) + "'",
	"img-src 'none'",
	"connect-src 'none'",
	"base-uri 'none'",
	"form-action 'none'",
	"frame-ancestors 'none'",
}, "; ")

// The hosted sign-in pages' inline assets (06-go-server.md §5.3). Each page embeds them byte for
// byte, so the hashes in AuthCSP match.
var (
	AuthCSS = mustRead("static/auth.css")
	ResetJS = mustRead("static/reset.js")
	PowJS   = mustRead("static/pow.js")
)

// AuthCSP is the Content-Security-Policy of every /auth/* page. form-action allows the loopback
// redirect that follows a successful POST (browsers apply form-action to it).
var AuthCSP = strings.Join([]string{
	"default-src 'none'",
	"style-src 'sha256-" + HashBase64(AuthCSS) + "'",
	"script-src 'sha256-" + HashBase64(ResetJS) + "' 'sha256-" + HashBase64(PowJS) + "'",
	"img-src data:",
	"form-action 'self' http://127.0.0.1:*",
	"frame-ancestors 'none'",
	"base-uri 'none'",
}, "; ")
