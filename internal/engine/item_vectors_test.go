package engine_test

import (
	"encoding/json"
	"fmt"
	"path/filepath"
	"reflect"
	"strings"
	"testing"

	"github.com/aws/aws-sdk-go-v2/service/dynamodb/types"
	"github.com/outboundani/wattdb/internal/engine"
	"github.com/outboundani/wattdb/internal/model"
	"github.com/outboundani/wattdb/internal/vectors"
)

type vectorCase struct {
	name  string
	m     *model.Model
	c     vectors.Case
	table string
}

// vectorCases loads every vector case whose entity builds.
func vectorCases(t *testing.T) []vectorCase {
	t.Helper()
	all, err := vectors.LoadAll()
	if err != nil {
		t.Fatal(err)
	}
	var out []vectorCase
	for _, v := range all {
		models := map[string]*model.Model{}
		for _, e := range v.Entities {
			m, err := e.Model()
			if err != nil {
				t.Fatalf("%s: entity %s: %v", filepath.Base(v.File), e.Name, err)
			}
			models[e.Name] = m
		}
		for _, c := range v.Cases {
			if m := models[c.Entity]; m != nil && len(c.Chain) > 0 {
				name := fmt.Sprintf("%s/%s", strings.TrimSuffix(filepath.Base(v.File), ".json"), c.Name)
				out = append(out, vectorCase{name, m, c, v.Table})
			}
		}
	}
	return out
}

// TestPutItemsMatchVectors checks whole put/create items, and that wattdb
// rejects what ElectroDB rejects.
func TestPutItemsMatchVectors(t *testing.T) {
	checked := 0
	for _, vc := range vectorCases(t) {
		first := vc.c.Chain[0]
		if (first.Method != "put" && first.Method != "create") || len(first.Args) == 0 || !isObject(first.Args[0]) {
			continue
		}
		t.Run(vc.name, func(t *testing.T) {
			payload, err := vectors.Attrs(first.Args[0])
			if err != nil {
				t.Fatal(err)
			}
			item, err := engine.PutItem(vc.m, payload)
			var wire map[string]types.AttributeValue
			if err == nil {
				wire, err = engine.ToAttributeMap(item)
			}

			jsErr := vc.c.ParamsError
			if jsErr == nil && vc.c.Error != nil {
				jsErr = vc.c.Error
			}
			switch {
			case jsErr != nil && strings.Contains(jsErr.Message, "MAX_SAFE_INTEGER"):
				return // D7: wattdb doesn't guard unsafe numbers
			case jsErr != nil:
				if err == nil {
					t.Fatalf("ElectroDB failed (%s) but wattdb wrote %v", jsErr.Message, show(wire))
				}
			case err != nil:
				t.Fatalf("PutItem: %v", err)
			default:
				reqs := vc.c.Requests()
				if len(reqs) == 0 {
					t.Skip("no recorded request")
				}
				var body struct {
					Item json.RawMessage `json:"Item"`
				}
				if err := json.Unmarshal(reqs[0].Request, &body); err != nil {
					t.Fatal(err)
				}
				compare(t, wire, attributeMap(t, body.Item))
			}
			checked++
		})
	}
	if checked == 0 {
		t.Fatal("no put cases were checked")
	}
	t.Logf("checked %d put cases", checked)
}

// TestReadFormattingMatchesVectors checks get results: ownership filtering
// and read formatting of the scripted item.
func TestReadFormattingMatchesVectors(t *testing.T) {
	checked := 0
	for _, vc := range vectorCases(t) {
		method, _, _ := strings.Cut(vc.c.Chain[0].Method, ".")
		if vc.c.Go == nil || vc.c.Go.Result == nil || len(vc.c.Responses) == 0 {
			continue
		}
		if method != "get" || !isObject(vc.c.Chain[0].Args[0]) {
			continue // queries, scans and batches have their own tests
		}
		t.Run(vc.name, func(t *testing.T) {
			var opts struct {
				Data            string   `json:"data"`
				Attributes      []string `json:"attributes"`
				IgnoreOwnership bool     `json:"ignoreOwnership"`
			}
			if vc.c.GoOptions != nil {
				if err := json.Unmarshal(vc.c.GoOptions, &opts); err != nil {
					t.Fatal(err)
				}
			}
			format := engine.FormatOptions{IncludeKeys: opts.Data == "includeKeys", Attributes: opts.Attributes}

			result, err := vectors.AttributeValue(vc.c.Go.Result)
			if err != nil {
				t.Fatal(err)
			}
			want := result.(*types.AttributeValueMemberM).Value["data"]

			var formatted []any
			for _, resp := range vc.c.Responses {
				var body struct {
					Item  json.RawMessage   `json:"Item"`
					Items []json.RawMessage `json:"Items"`
				}
				if err := json.Unmarshal(resp, &body); err != nil {
					t.Fatal(err)
				}
				items := body.Items
				if body.Item != nil {
					items = append(items, body.Item)
				}
				for _, raw := range items {
					item := engine.FromAttributeMap(attributeMap(t, raw))
					if !opts.IgnoreOwnership && !engine.OwnsItem(vc.m, item) {
						continue
					}
					data, err := engine.FormatItem(vc.m, item, format)
					if err != nil {
						t.Fatal(err)
					}
					formatted = append(formatted, data)
				}
			}

			var got types.AttributeValue
			if method == "get" {
				got = &types.AttributeValueMemberNULL{Value: true}
				if len(formatted) > 0 {
					got = toAV(t, formatted[0])
				}
			} else {
				if formatted == nil {
					formatted = []any{}
				}
				got = toAV(t, formatted)
			}
			if !reflect.DeepEqual(got, want) {
				t.Errorf("result differs\n got: %#v\nwant: %#v", got, want)
			}
			checked++
		})
	}
	if checked == 0 {
		t.Fatal("no read cases were checked")
	}
	t.Logf("checked %d read cases", checked)
}
