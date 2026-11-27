// Package clientip resolves the client address behind explicitly trusted proxies and maps it to
// a rate-limit bucket (06-go-server.md §5.9).
package clientip

import (
	"fmt"
	"log/slog"
	"net"
	"net/http"
	"net/netip"
	"strings"
	"sync"
)

// allTrafficSample is how many requests the "all traffic from one peer" warning looks at.
const allTrafficSample = 200

// Resolver finds the client IP of a request.
type Resolver struct {
	trusted []netip.Prefix
	header  string // canonical form, for lookups
	name    string // as configured, for messages
	isXFF   bool
	logger  *slog.Logger
	logIP   bool

	warnHeaderOnce sync.Once

	mu         sync.Mutex
	seen       int
	firstPeer  netip.Addr
	samePeer   bool
	warnedPeer bool
}

// New returns a resolver. header is the client-IP header honoured from trusted peers
// (X-Forwarded-For by default). logIP allows peer addresses in the warnings.
func New(trusted []netip.Prefix, header string, logger *slog.Logger, logIP bool) *Resolver {
	if header == "" {
		header = "X-Forwarded-For"
	}
	return &Resolver{
		trusted:  trusted,
		header:   http.CanonicalHeaderKey(header),
		name:     header,
		isXFF:    strings.EqualFold(header, "X-Forwarded-For"),
		logger:   logger,
		logIP:    logIP,
		samePeer: true,
	}
}

// Trusted reports whether a is inside a trusted-proxy CIDR.
func (r *Resolver) Trusted(a netip.Addr) bool {
	a = a.Unmap()
	for _, p := range r.trusted {
		if p.Contains(a) {
			return true
		}
	}
	return false
}

// Peer returns the TCP peer address of the request.
func Peer(req *http.Request) (netip.Addr, bool) {
	host, _, err := net.SplitHostPort(req.RemoteAddr)
	if err != nil {
		host = req.RemoteAddr
	}
	a, err := netip.ParseAddr(host)
	if err != nil {
		return netip.Addr{}, false
	}
	return a.Unmap(), true
}

// PeerIsTrusted reports whether the request's TCP peer is a trusted proxy.
func (r *Resolver) PeerIsTrusted(req *http.Request) bool {
	a, ok := Peer(req)
	return ok && r.Trusted(a)
}

// ClientIP returns the client address as a string: from the configured header when the peer is
// trusted (for X-Forwarded-For, the right-most entry that is not itself a trusted proxy; for a
// single-value header, the value as is), otherwise the peer address.
func (r *Resolver) ClientIP(req *http.Request) string {
	peer, ok := Peer(req)
	peerStr := strings.TrimSuffix(strings.TrimPrefix(req.RemoteAddr, "["), "]")
	if ok {
		peerStr = peer.String()
	}
	values := req.Header.Values(r.header)
	if ok && r.Trusted(peer) {
		if len(values) == 0 {
			return peerStr
		}
		if !r.isXFF {
			if v := strings.TrimSpace(values[0]); v != "" {
				return v
			}
			return peerStr
		}
		var hops []string
		for _, v := range values {
			for _, h := range strings.Split(v, ",") {
				if h = strings.TrimSpace(h); h != "" {
					hops = append(hops, h)
				}
			}
		}
		for i := len(hops) - 1; i >= 0; i-- {
			a, err := netip.ParseAddr(stripPort(hops[i]))
			if err != nil {
				return hops[i]
			}
			if !r.Trusted(a) {
				return a.Unmap().String()
			}
		}
		if len(hops) > 0 {
			return stripPort(hops[0])
		}
		return peerStr
	}
	r.observeUntrusted(peer, ok, len(values) > 0)
	return peerStr
}

// stripPort removes a port or brackets from an X-Forwarded-For hop ("[::1]:80", "1.2.3.4:5").
func stripPort(h string) string {
	if strings.HasPrefix(h, "[") {
		if i := strings.Index(h, "]"); i > 0 {
			return h[1:i]
		}
	}
	if strings.Count(h, ":") == 1 {
		host, _, err := net.SplitHostPort(h)
		if err == nil {
			return host
		}
	}
	return h
}

func (r *Resolver) observeUntrusted(peer netip.Addr, ok, sentHeader bool) {
	if sentHeader {
		r.warnHeaderOnce.Do(func() {
			msg := fmt.Sprintf("a request from an untrusted peer carried %s; it was ignored. If a reverse proxy is in front of this server, add its address to REQHARBOR_TRUSTED_PROXIES", r.name)
			r.logger.Warn(msg, r.peerAttr(peer)...)
		})
	}
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.warnedPeer || r.seen >= allTrafficSample || !ok {
		return
	}
	if peer.IsLoopback() {
		r.seen = allTrafficSample // loopback traffic: nothing to warn about
		return
	}
	if r.seen == 0 {
		r.firstPeer = peer
	} else if peer != r.firstPeer {
		r.samePeer = false
	}
	r.seen++
	if r.seen == allTrafficSample && r.samePeer {
		r.warnedPeer = true
		r.logger.Warn("every request so far came from one address; if that is a reverse proxy, set REQHARBOR_TRUSTED_PROXIES, otherwise all users share one rate-limit bucket", r.peerAttr(peer)...)
	}
}

func (r *Resolver) peerAttr(peer netip.Addr) []any {
	if r.logIP && peer.IsValid() {
		return []any{"peer", peer.String()}
	}
	return nil
}

// Bucket maps a client address to its rate-limit bucket (port of the Worker's rateLimitBucket,
// QA-R1-025): IPv4 per address, IPv6 per /64, IPv4-mapped IPv6 as IPv4. Anything that does not
// parse is its own bucket.
func Bucket(ip string) string {
	if !strings.Contains(ip, ":") {
		return strings.TrimSpace(ip)
	}
	s := strings.ToLower(strings.TrimSpace(ip))
	s = strings.TrimSuffix(strings.TrimPrefix(s, "["), "]")
	if i := strings.IndexByte(s, '%'); i >= 0 {
		s = s[:i]
	}
	a, err := netip.ParseAddr(s)
	if err != nil || !a.Is6() {
		return strings.TrimSpace(ip)
	}
	if a.Is4In6() {
		return a.Unmap().String()
	}
	b := a.As16()
	return fmt.Sprintf("%x:%x:%x:%x::/64",
		uint16(b[0])<<8|uint16(b[1]), uint16(b[2])<<8|uint16(b[3]),
		uint16(b[4])<<8|uint16(b[5]), uint16(b[6])<<8|uint16(b[7]))
}
