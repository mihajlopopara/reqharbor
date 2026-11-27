// Package jsonx parses JSON into an ordered tree and canonicalises stored entity data and share
// snapshots (06-go-server.md §1.4).
//
// The parser follows JSON.parse where the Worker relied on it (duplicate keys keep their first
// position and take the last value) and keeps what serde_json needs to judge a value: number
// lexemes verbatim and a flag on strings that contained an unpaired surrogate escape. The strict
// checks (number range, lone surrogates, depth) run in Canonical, so a request body can be parsed
// leniently and each entity judged on its own.
package jsonx

// Kind is the JSON type of a Value.
type Kind uint8

// The JSON kinds.
const (
	Null Kind = iota
	Bool
	Number
	String
	Array
	Object
)

// indexThreshold is the object size above which key lookups go through a map.
const indexThreshold = 8

// Value is one parsed JSON value.
type Value struct {
	Kind Kind
	// B is the value of a Bool.
	B bool
	// S is the decoded text of a String, or the verbatim lexeme of a Number.
	S string
	// Lone is set on a String that contained an unpaired surrogate escape (S then holds U+FFFD
	// in its place), or on an Object with such a key. serde_json refuses both.
	Lone bool
	// Arr holds the items of an Array.
	Arr []*Value
	// Keys and Vals hold the members of an Object in first-occurrence order.
	Keys []string
	Vals []*Value
	idx  map[string]int
}

// NewString returns a String value.
func NewString(s string) *Value { return &Value{Kind: String, S: s} }

// IsObject reports whether v is a JSON object.
func (v *Value) IsObject() bool { return v != nil && v.Kind == Object }

// IsArray reports whether v is a JSON array.
func (v *Value) IsArray() bool { return v != nil && v.Kind == Array }

// IsNull reports whether v is JSON null.
func (v *Value) IsNull() bool { return v != nil && v.Kind == Null }

// IsTrue reports whether v is the JSON literal true.
func (v *Value) IsTrue() bool { return v != nil && v.Kind == Bool && v.B }

// Str returns the text of a String and whether v is one.
func (v *Value) Str() (string, bool) {
	if v == nil || v.Kind != String {
		return "", false
	}
	return v.S, true
}

func (v *Value) find(key string) int {
	if v.idx != nil {
		if i, ok := v.idx[key]; ok {
			return i
		}
		return -1
	}
	for i, k := range v.Keys {
		if k == key {
			return i
		}
	}
	return -1
}

// Get returns the member key of an object, or nil when v is not an object or lacks the key
// (JavaScript's undefined).
func (v *Value) Get(key string) *Value {
	if v == nil || v.Kind != Object {
		return nil
	}
	if i := v.find(key); i >= 0 {
		return v.Vals[i]
	}
	return nil
}

// Set replaces the member key in place, or appends it, like a JavaScript property assignment
// (and like `{...o, key: val}`).
func (v *Value) Set(key string, val *Value) {
	if i := v.find(key); i >= 0 {
		v.Vals[i] = val
		return
	}
	v.Keys = append(v.Keys, key)
	v.Vals = append(v.Vals, val)
	if v.idx != nil {
		v.idx[key] = len(v.Keys) - 1
	} else if len(v.Keys) > indexThreshold {
		v.idx = make(map[string]int, len(v.Keys)*2)
		for i, k := range v.Keys {
			v.idx[k] = i
		}
	}
}

// ShallowCopy returns a copy of an object whose member lists can be changed without touching v
// (JavaScript's `{...v}`).
func (v *Value) ShallowCopy() *Value {
	c := &Value{Kind: v.Kind, B: v.B, S: v.S, Lone: v.Lone}
	c.Arr = append([]*Value(nil), v.Arr...)
	c.Keys = append([]string(nil), v.Keys...)
	c.Vals = append([]*Value(nil), v.Vals...)
	if v.idx != nil {
		c.idx = make(map[string]int, len(v.idx))
		for k, i := range v.idx {
			c.idx[k] = i
		}
	}
	return c
}
