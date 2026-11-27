package app

// DefaultVersion is the server release this source tree builds. The release workflow requires a
// `server-v*` tag to equal it (06-go-server.md §7.2).
const DefaultVersion = "0.1.0"

// ResolveVersion returns the version set at link time (-X main.version=…), or DefaultVersion for
// a plain `go build`.
func ResolveVersion(linked string) string {
	if linked == "" || linked == "dev" {
		return DefaultVersion
	}
	return linked
}
