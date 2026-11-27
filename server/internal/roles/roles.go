// Package roles holds the team roles and their ranks (02-product-spec.md §6.6, 04-tech-design.md
// §5.3): viewer 1, editor 2, admin 3, owner 4.
package roles

import "github.com/mihajlopopara/reqharbor/server/internal/httpx"

// The roles.
const (
	Owner  = "owner"
	Admin  = "admin"
	Editor = "editor"
	Viewer = "viewer"
)

// Rank orders the roles; 0 for an unknown role.
func Rank(role string) int {
	switch role {
	case Owner:
		return 4
	case Admin:
		return 3
	case Editor:
		return 2
	case Viewer:
		return 1
	}
	return 0
}

// Valid reports whether role is one of the four roles.
func Valid(role string) bool { return Rank(role) > 0 }

// AtLeast reports whether role ranks at least as high as min.
func AtLeast(role, min string) bool { return Rank(role) >= Rank(min) && Rank(role) > 0 }

// Forbidden is 403 forbidden_role: the caller's role does not allow the action.
func Forbidden(message string) *httpx.APIError {
	return httpx.NewError(403, "forbidden_role", message)
}
