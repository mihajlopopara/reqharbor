package syncsvc

import (
	"strconv"
	"strings"
	"testing"

	"github.com/mihajlopopara/reqharbor/server/internal/httpx"
	"github.com/mihajlopopara/reqharbor/server/internal/jsonx"
)

// TestWins_LWWTruthTable is contract C3.
func TestWins_LWWTruthTable(t *testing.T) {
	cases := []struct {
		inAt         int64
		inDev        string
		stAt         int64
		stDev        string
		incomingWins bool
	}{
		{2, "", 1, "", true},
		{1, "", 2, "", false},
		{1, "", 1, "", true},
		{1, "B", 1, "A", true},
		{1, "A", 1, "B", false},
		{1, "A", 1, "A", true},
		{1, "", 1, "01DEV", false},
		{1, "01DEV", 1, "", true},
		{2, "A", 1, "Z", true},
		{1, "Z", 2, "A", false},
		{1, "a", 1, "B", true}, // byte-wise: 'a' (0x61) > 'B' (0x42)
		{1, "01DEVICEB", 1, "01DEVICEA", true},
		{1, "01DEVICE", 1, "01DEVICEA", false},
	}
	for _, c := range cases {
		if got := Wins(c.inAt, c.inDev, c.stAt, c.stDev); got != c.incomingWins {
			t.Errorf("Wins(%d,%q | %d,%q) = %v", c.inAt, c.inDev, c.stAt, c.stDev, got)
		}
	}
}

func parse(t *testing.T, body string, opts ParseOptions) ([]Change, error) {
	t.Helper()
	v, err := jsonx.Parse([]byte(body))
	if err != nil {
		t.Fatalf("bad test body: %v", err)
	}
	return ParsePush(v, opts)
}

func TestParsePush_UnknownTypeModes(t *testing.T) {
	body := `{"changes":[{"type":"request","id":"r","updatedAt":1,"deleted":false,"data":{}},{"type":"protoFile","id":"p","updatedAt":1,"deleted":false,"data":{"x":1}}]}`
	if _, err := parse(t, body, ParseOptions{}); err == nil {
		t.Fatal("legacy accepted an unknown type")
	} else if e, _ := httpx.AsAPIError(err); e.Code != "unsupported_type" || e.Status != 400 {
		t.Fatalf("%v", err)
	}
	cs, err := parse(t, body, ParseOptions{UnknownTypePerChange: true})
	if err != nil || len(cs) != 2 || cs[0].Rejected != "" || cs[1].Rejected != ReasonUnsupportedType || cs[1].Size != 0 {
		t.Fatalf("%+v %v", cs, err)
	}
	// Every other check still applies to the unknown-type change.
	bad := `{"changes":[{"type":"protoFile","id":"a/b","updatedAt":1,"deleted":false,"data":{}}]}`
	if _, err := parse(t, bad, ParseOptions{UnknownTypePerChange: true}); err == nil {
		t.Fatal("bad id accepted")
	}
	// A non-string type is a 400 in both modes.
	if _, err := parse(t, `{"changes":[{"type":5,"id":"a","updatedAt":1,"deleted":false,"data":{}}]}`, ParseOptions{UnknownTypePerChange: true}); err == nil {
		t.Fatal("numeric type accepted")
	}
}

func TestParsePush_ValidationMessagesAndOrder(t *testing.T) {
	cases := []struct{ body, msg string }{
		{`{}`, "`changes` must be an array"},
		{`{"changes":[1]}`, "changes[0] must be an object"},
		{`{"changes":[{"type":"history"}]}`, "changes[0].type must be one of project, collection, folder, request, environment (history and settings are never synced)"},
		{`{"changes":[{"type":"request","id":"x y"}]}`, "changes[0].id must be a 1-128 character id"},
		{`{"changes":[{"type":"request","id":"x","updatedAt":-0.5}]}`, "changes[0].updatedAt must be a non-negative integer (epoch ms)"},
		{`{"changes":[{"type":"request","id":"x","updatedAt":1e400}]}`, "changes[0].updatedAt must be a non-negative integer (epoch ms)"},
		{`{"changes":[{"type":"request","id":"x","updatedAt":1,"deleted":0}]}`, "changes[0].deleted must be a boolean"},
		{`{"changes":[{"type":"request","id":"x","updatedAt":1,"deleted":false,"deviceId":true}]}`, "changes[0].deviceId must be a string of at most 128 id characters"},
		{`{"changes":[{"type":"request","id":"x","updatedAt":1,"deleted":true,"data":{}}]}`, "changes[0].data must be null for a tombstone"},
		{`{"changes":[{"type":"request","id":"x","updatedAt":1,"deleted":false}]}`, "changes[0].data must be an object unless deleted is true"},
		{`{"changes":[{"type":"request","id":"x","updatedAt":1,"deleted":false,"data":{}},{"type":"request","id":"x","updatedAt":2,"deleted":true}]}`, "Duplicate change for request x in one push"},
	}
	for _, c := range cases {
		_, err := parse(t, c.body, ParseOptions{})
		e, ok := httpx.AsAPIError(err)
		if !ok || e.Message != c.msg {
			t.Errorf("%s: %v, want %q", c.body, err, c.msg)
		}
	}
	// Same type and id in different types is not a duplicate; -0 and 1e3 are safe integers.
	cs, err := parse(t, `{"changes":[{"type":"request","id":"x","updatedAt":-0,"deleted":false,"data":{}},{"type":"folder","id":"x","updatedAt":1e3,"deleted":true,"data":null,"deviceId":null}]}`, ParseOptions{})
	if err != nil || len(cs) != 2 || cs[0].UpdatedAt != 0 || cs[1].UpdatedAt != 1000 || cs[1].Data != nil {
		t.Fatalf("%+v %v", cs, err)
	}
}

func TestStripSecretValuesForSync(t *testing.T) {
	v, _ := jsonx.Parse([]byte(`{"name":"e","variables":[{"key":"a","value":"1","secret":false},{"value":"s","key":"b","secret":true},{"key":"c","secret":"true","value":"kept"},"junk",{"key":"d","secret":true}]}`))
	out, _ := jsonx.Canonical(StripSecretValuesForSync(v))
	want := `{"name":"e","variables":[{"key":"a","value":"1","secret":false},{"value":"","key":"b","secret":true},{"key":"c","secret":"true","value":"kept"},"junk",{"key":"d","secret":true,"value":""}]}`
	if string(out) != want {
		t.Fatalf("\n got %s\nwant %s", out, want)
	}
	orig, _ := jsonx.Canonical(v)
	if strings.Contains(string(orig), `"value":""`) {
		t.Fatal("input mutated")
	}
	noVars, _ := jsonx.Parse([]byte(`{"variables":"x"}`))
	if StripSecretValuesForSync(noVars) != noVars {
		t.Fatal("non-array variables changed")
	}
}

func TestParsePullParams(t *testing.T) {
	for _, c := range []struct {
		since, limit string
		s            int64
		l            int
		ok           bool
	}{
		{"", "", 0, 500, true},
		{"5", "10", 5, 10, true},
		{"0", "100000", 0, 500, true},
		{"9007199254740991", "1", 9007199254740991, 1, true},
		{"9007199254740992", "", 0, 0, false},
		{"99999999999999999999", "", 0, 0, false},
		{"-1", "", 0, 0, false},
		{"1e3", "", 0, 0, false},
		{"", "0", 0, 0, false},
		{" 1", "", 0, 0, false},
	} {
		s, l, err := ParsePullParams(c.since, c.limit)
		if (err == nil) != c.ok || (c.ok && (s != c.s || l != c.l)) {
			t.Errorf("%q %q → %d %d %v", c.since, c.limit, s, l, err)
		}
	}
}

func FuzzPushValidation(f *testing.F) {
	for _, s := range []string{
		`{"changes":[]}`,
		`{"changes":[{"type":"request","id":"a","updatedAt":1,"deleted":false,"data":{"x":[1,2]}}]}`,
		`{"changes":[{"type":"environment","id":"e","updatedAt":1,"deleted":false,"data":{"variables":[{"secret":true}]}}]}`,
		`{"changes":[{"type":"folder","id":"f","updatedAt":1e3,"deleted":true,"deviceId":null}]}`,
		`{"changes":[{"type":"request","id":"a","updatedAt":1,"deleted":false,"data":{"n":1e999}}]}`,
		`{"changes":"x"}`,
	} {
		f.Add([]byte(s), false)
	}
	f.Fuzz(func(t *testing.T, body []byte, perChange bool) {
		v, err := jsonx.Parse(body)
		if err != nil || !v.IsObject() {
			return
		}
		cs, err := ParsePush(v, ParseOptions{UnknownTypePerChange: perChange})
		if err != nil {
			e, ok := httpx.AsAPIError(err)
			if !ok || e.Status < 400 || e.Status > 499 {
				t.Fatalf("non-4xx error %v for %q", err, body)
			}
			return
		}
		for _, c := range cs {
			if c.Rejected == "" && !c.Deleted {
				if c.Data == nil || int64(len(c.Data)) != c.Size || c.Size > EntityDataBytes {
					t.Fatalf("bad accepted change %+v", c)
				}
				if _, err := jsonx.Canonicalize(c.Data); err != nil {
					t.Fatalf("stored data not canonical-valid: %v", err)
				}
			}
		}
	})
}

// TestParsePush_ClockSkew: with MaxUpdatedAt (account spaces) a change more than 5 minutes ahead
// is a per-change clock_skew, tombstones included; exactly +5 min passes; every 400 check still
// applies to such a change; unsupported_type wins over clock_skew, which wins over the data checks.
func TestParsePush_ClockSkew(t *testing.T) {
	const now = int64(1_800_000_000_000)
	maxAt := now + MaxClockAheadMillis
	big := `"` + strings.Repeat("x", EntityDataBytes) + `"`
	cases := []struct {
		name   string
		change string
		reason string
		code   string // a whole-request 400 instead
	}{
		{"at the limit", `{"type":"request","id":"r","updatedAt":%d,"deleted":false,"data":{}}`, "", ""},
		{"one ms over", `{"type":"request","id":"r","updatedAt":%d,"deleted":false,"data":{}}`, ReasonClockSkew, ""},
		{"tombstone over", `{"type":"request","id":"r","updatedAt":%d,"deleted":true}`, ReasonClockSkew, ""},
		{"too large and skewed", `{"type":"request","id":"r","updatedAt":%d,"deleted":false,"data":{"b":` + big + `}}`, ReasonClockSkew, ""},
		{"unknown type and skewed", `{"type":"protoFile","id":"r","updatedAt":%d,"deleted":false,"data":{}}`, ReasonUnsupportedType, ""},
		{"skewed with bad data", `{"type":"request","id":"r","updatedAt":%d,"deleted":false,"data":[]}`, "", "validation_failed"},
		{"skewed tombstone with data", `{"type":"request","id":"r","updatedAt":%d,"deleted":true,"data":{}}`, "", "validation_failed"},
	}
	for i, c := range cases {
		at := maxAt + 1
		if i == 0 {
			at = maxAt
		}
		body := `{"changes":[` + strings.Replace(c.change, "%d", strconv.FormatInt(at, 10), 1) + `]}`
		cs, err := parse(t, body, ParseOptions{UnknownTypePerChange: true, MaxUpdatedAt: maxAt})
		if c.code != "" {
			if e, ok := httpx.AsAPIError(err); !ok || e.Code != c.code || e.Status != 400 {
				t.Errorf("%s: want 400 %s, got %v", c.name, c.code, err)
			}
			continue
		}
		if err != nil || len(cs) != 1 || cs[0].Rejected != c.reason {
			t.Errorf("%s: %+v %v", c.name, cs, err)
			continue
		}
		if c.reason != "" && (cs[0].Size != 0 || cs[0].Data != nil) {
			t.Errorf("%s: a rejected change carries data", c.name)
		}
	}
	// Legacy (no MaxUpdatedAt) never rejects for skew.
	cs, err := parse(t, `{"changes":[{"type":"request","id":"r","updatedAt":9007199254740991,"deleted":false,"data":{}}]}`, ParseOptions{})
	if err != nil || cs[0].Rejected != "" {
		t.Fatalf("legacy skew: %+v %v", cs, err)
	}
}
