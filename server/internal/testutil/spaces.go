package testutil

import (
	"testing"
)

// Account-space helpers (S6).

// PersonalSpace returns the personal space id of the signed-in user from GET /v1/me/spaces.
func (h *Harness) PersonalSpace(t testing.TB, access string) string {
	t.Helper()
	res := h.Call(t, "/v1/me/spaces", Req{Token: access})
	if res.StatusCode != 200 {
		t.Fatalf("/v1/me/spaces: %d %s", res.StatusCode, truncate(res.Body))
	}
	spaces, _ := res.JSON(t)["spaces"].([]any)
	for _, s := range spaces {
		m, _ := s.(map[string]any)
		if m["kind"] == "personal" {
			id, _ := m["spaceId"].(string)
			return id
		}
	}
	t.Fatalf("no personal space in %s", truncate(res.Body))
	return ""
}

// SpacePush pushes changes to a space with an access token.
func (h *Harness) SpacePush(t testing.TB, access, spaceID string, cs ...map[string]any) *Resp {
	t.Helper()
	return h.Call(t, "/v1/spaces/"+spaceID+"/sync", Req{Token: access, JSON: Changes(cs...)})
}

// SpacePull pulls a space page; query is the raw query string ("" or "since=3").
func (h *Harness) SpacePull(t testing.TB, access, spaceID, query string) *Resp {
	t.Helper()
	path := "/v1/spaces/" + spaceID + "/sync"
	if query != "" {
		path += "?" + query
	}
	return h.Call(t, path, Req{Token: access})
}
