package engine_test

import (
	"encoding/json"
	"reflect"
	"strings"
	"testing"

	"github.com/outboundani/wattdb/internal/engine"
	"github.com/outboundani/wattdb/internal/vectors"
)

// TestConversionsMatchVectors checks conversions and parse against their
// recorded values and errors.
func TestConversionsMatchVectors(t *testing.T) {
	checked := 0
	for _, vc := range vectorCases(t) {
		first := vc.c.Chain[0]
		if !strings.HasPrefix(first.Method, "conversions.") && first.Method != "parse" {
			continue
		}
		t.Run(vc.name, func(t *testing.T) {
			got, err := runConversion(t, vc, first)
			if vc.c.Error != nil {
				if err == nil {
					t.Fatalf("ElectroDB failed (%s) but wattdb returned %v", vc.c.Error.Message, got)
				}
				return
			}
			if err != nil {
				t.Fatal(err)
			}
			want, err := vectors.AttributeValue(vc.c.Value)
			if err != nil {
				t.Fatal(err)
			}
			if s, ok := got.(string); ok { // cursors: compare what they encode
				var w string
				json.Unmarshal(vc.c.Value, &struct {
					S *string `json:"S"`
				}{&w})
				if !reflect.DeepEqual(decodeCursorJSON(s), decodeCursorJSON(w)) {
					t.Errorf("cursor = %s, want %s", decodeCursorJSON(s), decodeCursorJSON(w))
				}
			} else if g := toAV(t, got); !reflect.DeepEqual(g, want) {
				t.Errorf("value differs\n got: %s\nwant: %s", show2(t, got), vc.c.Value)
			}
			checked++
		})
	}
	if checked == 0 {
		t.Fatal("no conversion cases were checked")
	}
}

func runConversion(t *testing.T, vc vectorCase, step vectors.Step) (any, error) {
	t.Helper()
	m := vc.m
	if step.Method == "parse" {
		resp := mustAttrs(t, step.Args[0])
		var opts struct {
			Data       string   `json:"data"`
			Attributes []string `json:"attributes"`
		}
		if len(step.Args) > 1 {
			json.Unmarshal(step.Args[1], &opts)
		}
		out, err := engine.Parse(m, resp, engine.FormatOptions{IncludeKeys: opts.Data == "includeKeys", Attributes: opts.Attributes}, true)
		if err != nil {
			return nil, err
		}
		if d, ok := out.(map[string]any); ok && d == nil {
			out = nil
		}
		return map[string]any{"data": out}, nil
	}
	path := strings.Split(strings.TrimPrefix(step.Method, "conversions."), ".")
	var strict engine.Strict
	if len(step.Args) > 1 {
		var o struct {
			Strict string `json:"strict"`
		}
		json.Unmarshal(step.Args[1], &o)
		strict = engine.Strict(o.Strict)
	}
	cursor := func() string {
		var s string
		json.Unmarshal(step.Args[0], &s)
		return s
	}
	keys := func() map[string]any { return map[string]any(mustAttrs(t, step.Args[0])) }
	if path[0] == "byAccessPattern" {
		ix, ok := m.AccessPattern(path[1])
		if !ok {
			t.Fatalf("unknown access pattern %s", path[1])
		}
		switch strings.Join(path[2:], ".") {
		case "fromComposite.toKeys":
			return engine.CompositeToKeysByIndex(m, ix, mustAttrs(t, step.Args[0]), strict)
		case "fromComposite.toCursor":
			return engine.CompositeToCursorByIndex(m, ix, mustAttrs(t, step.Args[0]))
		case "fromKeys.toCursor":
			return engine.KeysToCursorByIndex(m, ix, keys())
		case "fromKeys.toComposite":
			return engine.KeysToCompositeByIndex(m, ix, keys())
		case "fromCursor.toKeys":
			return engine.CursorToKeys(cursor())
		case "fromCursor.toComposite":
			return engine.CursorToCompositeByIndex(m, ix, cursor())
		}
	}
	switch strings.Join(path, ".") {
	case "fromComposite.toKeys":
		return engine.CompositeToKeys(m, mustAttrs(t, step.Args[0]), strict)
	case "fromComposite.toCursor":
		return engine.CompositeToCursor(m, mustAttrs(t, step.Args[0]))
	case "fromKeys.toCursor":
		return engine.KeysToCursor(keys())
	case "fromKeys.toComposite":
		return engine.KeysToComposite(m, keys())
	case "fromCursor.toKeys":
		return engine.CursorToKeys(cursor())
	case "fromCursor.toComposite":
		return engine.CursorToComposite(m, cursor())
	}
	t.Fatalf("unsupported conversion %s", step.Method)
	return nil, nil
}
