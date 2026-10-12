package engine

import (
	"math"
	"testing"

	"github.com/outboundani/wattdb/internal/model"
)

func TestFloatNumber(t *testing.T) {
	tests := []struct {
		in   float64
		want Number
	}{
		{42, "42"},
		{-17.25, "-17.25"},
		{0.1, "0.1"},
		{0.30000000000000004, "0.30000000000000004"},
		{math.Copysign(0, -1), "0"},
		{9007199254740991, "9007199254740991"},
		{1e20, "100000000000000000000"},
		// D56: plain decimals where JS switches to exponents.
		{1e-7, "0.0000001"},
		{1e21, "1000000000000000000000"},
	}
	for _, tt := range tests {
		if got := FloatNumber(tt.in); got != tt.want {
			t.Errorf("FloatNumber(%v) = %q, want %q", tt.in, got, tt.want)
		}
	}
}

func TestJSParseInt(t *testing.T) {
	tests := []struct {
		in   string
		want Number
		ok   bool
	}{
		{"42", "42", true},
		{"42abc", "42", true},
		{"  -7", "-7", true},
		{"+5", "5", true},
		{"007", "7", true},
		{"-0", "0", true},
		{"0x1A", "26", true},
		{"1.9", "1", true},
		{"abc", "", false},
		{"", "", false},
		{"-", "", false},
	}
	for _, tt := range tests {
		got, ok := jsParseInt(tt.in)
		if got != tt.want || ok != tt.ok {
			t.Errorf("jsParseInt(%q) = %q, %v; want %q, %v", tt.in, got, ok, tt.want, tt.ok)
		}
	}
}

func TestPad(t *testing.T) {
	tests := []struct {
		in   string
		p    *model.Padding
		want string
	}{
		{"42", &model.Padding{Length: 5, Char: "0"}, "00042"},
		{"x", &model.Padding{Length: 5, Char: "ab"}, "ababx"},
		{"123456", &model.Padding{Length: 3, Char: "0"}, "123456"},
		{"7", nil, "7"},
		{"7", &model.Padding{Length: 3}, "7"},
	}
	for _, tt := range tests {
		if got := pad(tt.in, tt.p); got != tt.want {
			t.Errorf("pad(%q, %+v) = %q, want %q", tt.in, tt.p, got, tt.want)
		}
	}
}

func TestNumericCastKey(t *testing.T) {
	tests := []struct {
		in   any
		want any
	}{
		{Number("7"), Number("7")},
		{"12px", Number("12")},
		{true, Number("1")},
		{false, Number("0")},
	}
	for _, tt := range tests {
		got, ok, err := numericCastKey("n", Attrs{"n": tt.in})
		if err != nil || !ok || got != tt.want {
			t.Errorf("numericCastKey(%v) = %v, %v, %v; want %v", tt.in, got, ok, err, tt.want)
		}
	}
	if _, _, err := numericCastKey("n", Attrs{"n": "abc"}); err == nil {
		t.Error("numericCastKey(\"abc\") should fail")
	}
	if got, ok, err := numericCastKey("n", Attrs{}); got != nil || ok || err != nil {
		t.Errorf("numericCastKey(missing) = %v, %v, %v", got, ok, err)
	}
}
