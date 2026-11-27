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
