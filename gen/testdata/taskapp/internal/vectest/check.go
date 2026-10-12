package vectest

import (
	"bytes"
	"encoding/json"
	"reflect"
	"strings"
	"testing"

	"github.com/aws/aws-sdk-go-v2/service/dynamodb/types"
	"github.com/outboundani/wattdb"
)

// Strict decodes a recorded input into a generated Input, failing on
// attributes the Input doesn't have.
func Strict(arg json.RawMessage, v any) error {
	dec := json.NewDecoder(bytes.NewReader(arg))
	dec.DisallowUnknownFields()
	return dec.Decode(v)
}

// Divergences are recorded inputs the typed API handles differently from
// ElectroDB, by design, keyed by case name.
var Divergences = map[string]string{
	"null for a string":                     "Go can't pass JS null for a string; nil means missing",
	"empty set":                             "D18: an empty set is written as missing, where JS throws",
	"list item missing a required property": "D17: required map properties are plain values, so a missing one is written as its zero value",
}

// CheckPuts runs recorded put inputs through a generated package's Input
// and callbacks and compares the written items. Inputs the typed API can't
// express (unknown attributes, wrong JSON types) are skipped.
func CheckPuts(t *testing.T, vector string, def *wattdb.EntityDef, encode func(json.RawMessage) (map[string]types.AttributeValue, error)) {
	cases, err := Load(vector)
	if err != nil {
		t.Fatal(err)
	}
	checked := 0
	for _, c := range cases {
		if c.Method != "put" || c.Arg == nil || (c.Item == nil && c.Rejected == "") {
			continue
		}
		t.Run(c.Name, func(t *testing.T) {
			if why, ok := Divergences[c.Name]; ok {
				t.Skip(why)
			}
			in, err := encode(c.Arg)
			if err != nil {
				t.Skipf("not expressible with the typed Input: %v", err)
			}
			got, err := def.PutItem(in)
			switch {
			case c.Rejected != "":
				if err == nil {
					t.Fatalf("ElectroDB rejected this (%s) but wattdb wrote %v", c.Rejected, got)
				}
			case err != nil:
				t.Fatalf("PutItem: %v", err)
			case !reflect.DeepEqual(got, c.Item):
				t.Errorf("items differ\n got: %v\nwant: %v", got, c.Item)
			}
			checked++
		})
	}
	if checked == 0 {
		t.Errorf("%s: no put cases checked", vector)
	}
	t.Logf("%s: checked %d put cases", vector, checked)
}

// CheckReads formats recorded items through a generated package's getters
// and compares the decoded struct, as JSON, with what ElectroDB returned.
func CheckReads(t *testing.T, vector string, def *wattdb.EntityDef, decode func(map[string]types.AttributeValue) (any, error)) {
	cases, err := Load(vector)
	if err != nil {
		t.Fatal(err)
	}
	for _, c := range cases {
		if c.Method != "get" || c.Stored == nil || strings.Contains(c.Name, "includeKeys") || strings.Contains(c.Name, "attributes") {
			continue
		}
		t.Run(c.Name, func(t *testing.T) {
			out, owned, err := def.FormatItem(c.Stored)
			if err != nil || !owned {
				t.Fatalf("FormatItem: owned=%v err=%v", owned, err)
			}
			v, err := decode(out)
			if err != nil {
				t.Fatal(err)
			}
			got, _ := json.Marshal(v)
			want, _ := json.Marshal(c.Data)
			var g, w any
			json.Unmarshal(got, &g)
			json.Unmarshal(want, &w)
			if !reflect.DeepEqual(g, w) {
				t.Errorf("reads differ\n got: %s\nwant: %s", got, want)
			}
		})
	}
}
