package model

import (
	"reflect"
	"strings"
	"testing"
)

func TestParseTemplate(t *testing.T) {
	tests := []struct {
		in   string
		want []Label
	}{
		{"user#${userId}", []Label{{Name: "userId", Label: "user#"}}},
		{"${a}#${b}", []Label{{Name: "a"}, {Name: "b", Label: "#"}}},
		{"a${x}b", []Label{{Name: "x", Label: "a"}, {Label: "b"}}},
		{"$x${y}", []Label{{Name: "y", Label: "$x"}}},
		{"${open", []Label{{Name: "open"}}},
	}
	for _, tt := range tests {
		if got := parseTemplate(tt.in); !reflect.DeepEqual(got, tt.want) {
			t.Errorf("parseTemplate(%q) = %+v, want %+v", tt.in, got, tt.want)
		}
	}
}

func TestFormatCasingIsASCII(t *testing.T) {
	if got := FormatCasing("AbC-ÄÖ", Lower); got != "abc-ÄÖ" {
		t.Errorf("lower = %q", got)
	}
	if got := FormatCasing("abc-äö", Upper); got != "ABC-äö" {
		t.Errorf("upper = %q", got)
	}
	if got := FormatCasing("AbC", None); got != "AbC" {
		t.Errorf("none = %q", got)
	}
}

func TestNewErrors(t *testing.T) {
	str := func(names ...string) []AttributeDef {
		var out []AttributeDef
		for _, n := range names {
			out = append(out, AttributeDef{Name: n, Type: TypeString})
		}
		return out
	}
	base := func(indexes ...IndexDef) Definition {
		return Definition{Entity: "e", Version: "1", Service: "s", Attributes: str("a", "b"), Indexes: indexes}
	}
	primary := IndexDef{AccessPattern: "p", PK: KeyDef{Field: "pk", Composite: []string{"a"}}, SK: &KeyDef{Field: "sk", Composite: []string{}}}

	tests := []struct {
		name string
		def  Definition
		want string
	}{
		{"missing table index", base(IndexDef{AccessPattern: "g", Index: "gsi1", PK: KeyDef{Field: "g", Composite: []string{"a"}}}), "missing the table index"},
		{"collection without sk", base(primary, IndexDef{AccessPattern: "g", Index: "gsi1", Collection: []string{"c"}, PK: KeyDef{Field: "g", Composite: []string{"a"}}}), "collections require an sk"},
		{"cast number with two composites", base(IndexDef{AccessPattern: "p", PK: KeyDef{Field: "pk", Composite: []string{"a", "b"}, Cast: "number"}}), "cast to number"},
		{"condition on table index", base(IndexDef{AccessPattern: "p", Condition: func(map[string]any) (bool, error) { return true, nil }, PK: KeyDef{Field: "pk", Composite: []string{"a"}}}), "only allowed on secondary"},
		{"same field for pk and sk", base(IndexDef{AccessPattern: "p", PK: KeyDef{Field: "k", Composite: []string{"a"}}, SK: &KeyDef{Field: "k", Composite: []string{"b"}}}), "both pk and sk"},
		{"attribute key field with two composites", base(IndexDef{AccessPattern: "p", PK: KeyDef{Field: "a", Composite: []string{"a", "b"}}}), "can't have other composites"},
		{"unknown composite", base(IndexDef{AccessPattern: "p", PK: KeyDef{Field: "pk", Composite: []string{"zz"}}}), "not an attribute"},
		{"map composite", Definition{Entity: "e", Version: "1", Service: "s", Attributes: []AttributeDef{{Name: "m", Type: TypeMap, Properties: []AttributeDef{}}}, Indexes: []IndexDef{{AccessPattern: "p", PK: KeyDef{Field: "pk", Composite: []string{"m"}}}}}, "composites must be"},
		{"duplicate field", Definition{Entity: "e", Version: "1", Service: "s", Attributes: []AttributeDef{{Name: "a", Field: "f"}, {Name: "b", Field: "f"}}, Indexes: []IndexDef{{AccessPattern: "p", PK: KeyDef{Field: "pk", Composite: []string{"a"}}}}}, "already used"},
		{"watching a watcher", Definition{Entity: "e", Version: "1", Service: "s", Attributes: []AttributeDef{{Name: "a"}, {Name: "b", Watch: []string{"a"}}, {Name: "c", Watch: []string{"b"}}}, Indexes: []IndexDef{{AccessPattern: "p", PK: KeyDef{Field: "pk", Composite: []string{"a"}}}}}, "itself a watcher"},
		{"watching an unknown attribute", Definition{Entity: "e", Version: "1", Service: "s", Attributes: []AttributeDef{{Name: "a", Watch: []string{"zz"}}}, Indexes: []IndexDef{{AccessPattern: "p", PK: KeyDef{Field: "pk", Composite: []string{"a"}}}}}, "unknown attribute"},
		{"template mismatch", base(IndexDef{AccessPattern: "p", PK: KeyDef{Field: "pk", Composite: []string{"a"}, Template: ptr("x#${b}")}}), "don't match"},
	}
	for _, tt := range tests {
		_, err := New(tt.def)
		if err == nil || !strings.Contains(err.Error(), tt.want) {
			t.Errorf("%s: err = %v, want %q", tt.name, err, tt.want)
		}
	}
}

func ptr(s string) *string { return &s }
