package engine

import (
	"testing"

	"github.com/outboundani/wattdb/internal/model"
)

func TestJSNumber(t *testing.T) {
	tests := []struct {
		in   any
		want Number
		ok   bool
	}{
		{"12", "12", true},
		{" 1.5 ", "1.5", true},
		{"", "0", true},
		{"0x10", "16", true},
		{"0b11", "3", true},
		{"1e3", "1000", true},
		{"12px", "", false},
		{"Infinity", "", false},
		{true, "1", true},
		{nil, "0", true},
		{[]any{}, "0", true},
		{[]any{"7"}, "7", true},
		{[]any{1, 2}, "", false},
		{map[string]any{}, "", false},
	}
	for _, tt := range tests {
		got, ok := jsNumber(tt.in)
		if got != tt.want || ok != tt.ok {
			t.Errorf("jsNumber(%#v) = %q, %v; want %q, %v", tt.in, got, ok, tt.want, tt.ok)
		}
	}
}

func TestJSONStringify(t *testing.T) {
	v := map[string]any{"b": []any{Number("1"), nil, true}, "a": "x\"y", "u": Undefined}
	if got, want := jsonStringify(v), `{"a":"x\"y","b":[1,null,true]}`; got != want {
		t.Errorf("jsonStringify = %s, want %s", got, want)
	}
}

func TestCast(t *testing.T) {
	str := &model.Attribute{Name: "s", Path: "s", Cast: "string"}
	if got, err := cast(str, Number("5")); err != nil || got != "5" {
		t.Errorf("cast string(5) = %v, %v", got, err)
	}
	if _, err := cast(str, Undefined); err == nil {
		t.Error("cast string(undefined) should fail, as in ElectroDB")
	}
	num := &model.Attribute{Name: "n", Path: "n", Cast: "number"}
	if got, err := cast(num, "42"); err != nil || got != Number("42") {
		t.Errorf("cast number(\"42\") = %v, %v", got, err)
	}
	if _, err := cast(num, "abc"); err == nil {
		t.Error("cast number(\"abc\") should fail")
	}
}

func TestRemoveFixings(t *testing.T) {
	tests := []struct{ prefix, postfix, in, want string }{
		{"user#", "", "USER#abc", "abc"},
		{"", "#v1", "abc#V1", "abc"},
		{"a#", "#z", "a#mid#z", "mid"},
		{"x#", "", "nope", "nope"},
	}
	for _, tt := range tests {
		if got := removeFixings(tt.prefix, tt.postfix, tt.in); got != tt.want {
			t.Errorf("removeFixings(%q, %q, %q) = %q, want %q", tt.prefix, tt.postfix, tt.in, got, tt.want)
		}
	}
}
