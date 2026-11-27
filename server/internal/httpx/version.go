package httpx

import (
	"net/http"
	"strconv"
	"strings"
)

// VersionHeader carries the desktop client's version (M0).
const VersionHeader = "X-ReqHarbor-Version"

// CheckClientVersion is the REQHARBOR_MIN_CLIENT_VERSION gate of /v1/spaces/* and
// /v1/auth/token (06-go-server.md §4): 426 client_too_old when the header is missing or lower.
// An empty min disables it. Legacy routes are never gated.
func CheckClientVersion(r *http.Request, min string) error {
	if min == "" {
		return nil
	}
	v := strings.TrimSpace(r.Header.Get(VersionHeader))
	if v == "" || CompareVersions(v, min) < 0 {
		return NewError(426, "client_too_old", "This server needs ReqHarbor "+min+" or newer")
	}
	return nil
}

// CompareVersions compares two semantic versions (major.minor.patch[-pre]); a release sorts after
// its pre-releases. Unparsable versions sort lowest.
func CompareVersions(a, b string) int {
	pa, preA, okA := parseVersion(a)
	pb, preB, okB := parseVersion(b)
	switch {
	case !okA && !okB:
		return 0
	case !okA:
		return -1
	case !okB:
		return 1
	}
	for i := range 3 {
		if pa[i] != pb[i] {
			if pa[i] < pb[i] {
				return -1
			}
			return 1
		}
	}
	switch {
	case preA == preB:
		return 0
	case preA == "":
		return 1
	case preB == "":
		return -1
	}
	return comparePre(preA, preB)
}

func parseVersion(s string) ([3]uint64, string, bool) {
	var out [3]uint64
	s = strings.TrimPrefix(strings.TrimSpace(s), "v")
	if i := strings.IndexByte(s, '+'); i >= 0 {
		s = s[:i]
	}
	core, pre, _ := strings.Cut(s, "-")
	parts := strings.Split(core, ".")
	if len(parts) != 3 {
		return out, "", false
	}
	for i, p := range parts {
		n, err := strconv.ParseUint(p, 10, 64)
		if err != nil {
			return out, "", false
		}
		out[i] = n
	}
	return out, pre, true
}

func comparePre(a, b string) int {
	as, bs := strings.Split(a, "."), strings.Split(b, ".")
	for i := 0; i < len(as) && i < len(bs); i++ {
		na, errA := strconv.ParseUint(as[i], 10, 64)
		nb, errB := strconv.ParseUint(bs[i], 10, 64)
		switch {
		case errA == nil && errB == nil:
			if na != nb {
				if na < nb {
					return -1
				}
				return 1
			}
		case errA == nil:
			return -1
		case errB == nil:
			return 1
		default:
			if c := strings.Compare(as[i], bs[i]); c != 0 {
				return c
			}
		}
	}
	switch {
	case len(as) < len(bs):
		return -1
	case len(as) > len(bs):
		return 1
	}
	return 0
}
