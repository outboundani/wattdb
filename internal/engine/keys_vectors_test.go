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

// TestKeysMatchVectors checks every key ElectroDB recorded: the key fields of
// put items, the Key of get/delete/update requests, and query key values.
func TestKeysMatchVectors(t *testing.T) {
	all, err := vectors.LoadAll()
	if err != nil {
		t.Fatal(err)
	}
	checked := 0
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
			name := fmt.Sprintf("%s/%s", strings.TrimSuffix(filepath.Base(v.File), ".json"), c.Name)
			m := models[c.Entity]
			if m == nil || c.Error != nil || len(c.Chain) == 0 {
				continue
			}
			reqs := c.Requests()
			if len(reqs) == 0 {
				continue
			}
			t.Run(name, func(t *testing.T) {
				if checkKeys(t, m, c, reqs[0]) {
					checked++
				}
			})
		}
	}
	if checked == 0 {
		t.Fatal("no vector cases were checked")
	}
	t.Logf("checked keys in %d cases", checked)
}

// checkKeys compares the engine's keys with one recorded request. It reports
// whether the case had keys to check.
func checkKeys(t *testing.T, m *model.Model, c vectors.Case, req vectors.Request) bool {
	t.Helper()
	var body struct {
		Item   json.RawMessage `json:"Item"`
		Key    json.RawMessage `json:"Key"`
		Values json.RawMessage `json:"ExpressionAttributeValues"`
	}
	if err := json.Unmarshal(req.Request, &body); err != nil {
		t.Fatal(err)
	}
	first := c.Chain[0]
	method, pattern, _ := strings.Cut(first.Method, ".")
	if len(first.Args) == 0 || !isObject(first.Args[0]) {
		return false // scans and batch operations aren't key-level checks
	}
	args, err := vectors.Attrs(first.Args[0])
	if err != nil {
		t.Fatal(err)
	}

	switch {
	case (method == "put" || method == "create") && req.Target == "PutItem":
		item := attributeMap(t, body.Item)
		set := itemAttrs(m, item)
		got, err := engine.PutKeys(m, set, primaryAttrs(m, set))
		if err != nil {
			t.Fatalf("PutKeys: %v", err)
		}
		compare(t, toAVMap(t, got), keyFields(m, item))

	case method == "get" || method == "delete" || method == "remove" || method == "update" || method == "patch" || method == "upsert":
		got, err := engine.PrimaryKey(m, args)
		if err != nil {
			t.Fatalf("PrimaryKey: %v", err)
		}
		compare(t, toAVMap(t, got), attributeMap(t, body.Key))

	case method == "query":
		ix, ok := m.AccessPattern(pattern)
		if !ok {
			t.Fatalf("unknown access pattern %q", pattern)
		}
		typ, sks := queryFacets(t, c.Chain, args)
		keys, err := engine.QueryKeys(m, ix, typ, args, sks...)
		if err != nil {
			t.Fatalf("QueryKeys: %v", err)
		}
		values := attributeMap(t, body.Values)
		got := map[string]types.AttributeValue{":pk": toAV(t, keys.PK)}
		want := map[string]types.AttributeValue{":pk": values[":pk"]}
		for i, sk := range keys.SK {
			name := fmt.Sprintf(":sk%d", i+1)
			if v, ok := values[name]; ok {
				got[name], want[name] = toAV(t, sk), v
			}
		}
		compare(t, got, want)

	default:
		return false
	}
	return true
}

// queryFacets merges the access-pattern composites with the sort-key clause,
// as ElectroDB's _consolidateQueryFacets does.
func queryFacets(t *testing.T, chain []vectors.Step, base engine.Attrs) (engine.QueryType, []engine.Attrs) {
	merge := func(arg json.RawMessage) engine.Attrs {
		extra, err := vectors.Attrs(arg)
		if err != nil {
			t.Fatal(err)
		}
		out := engine.Attrs{}
		for k, v := range base {
			out[k] = v
		}
		for k, v := range extra {
			out[k] = v
		}
		return out
	}
	types := map[string]engine.QueryType{
		"begins": engine.QueryBegins, "gt": engine.QueryGt, "gte": engine.QueryGte,
		"lt": engine.QueryLt, "lte": engine.QueryLte,
	}
	for _, step := range chain[1:] {
		if step.Method == "between" {
			return engine.QueryBetween, []engine.Attrs{merge(step.Args[0]), merge(step.Args[1])}
		}
		if typ, ok := types[step.Method]; ok {
			return typ, []engine.Attrs{merge(step.Args[0])}
		}
	}
	return engine.QueryIs, []engine.Attrs{base}
}

// itemAttrs maps a stored item's root fields back to attribute names.
func itemAttrs(m *model.Model, item map[string]types.AttributeValue) engine.Attrs {
	out := engine.Attrs{}
	for _, a := range m.Attributes {
		if av, ok := item[a.Field]; ok {
			out[a.Name] = engine.FromAttributeValue(av)
		}
	}
	return out
}

func primaryAttrs(m *model.Model, attrs engine.Attrs) engine.Attrs {
	out := engine.Attrs{}
	for _, name := range m.TableIndex().Composites() {
		if v, ok := attrs[name]; ok {
			out[name] = v
		}
	}
	return out
}

// keyFields returns the item's index key fields.
func keyFields(m *model.Model, item map[string]types.AttributeValue) map[string]types.AttributeValue {
	out := map[string]types.AttributeValue{}
	for _, ix := range m.Indexes {
		if ix.Type == model.Composite {
			continue
		}
		fields := []string{ix.PK.Field}
		if ix.HasSK {
			fields = append(fields, ix.SK.Field)
		}
		for _, f := range fields {
			if av, ok := item[f]; ok {
				out[f] = av
			}
		}
	}
	return out
}

func isObject(data json.RawMessage) bool {
	for _, b := range data {
		switch b {
		case ' ', '\t', '\n', '\r':
			continue
		}
		return b == '{'
	}
	return false
}

func attributeMap(t *testing.T, data json.RawMessage) map[string]types.AttributeValue {
	t.Helper()
	m, err := vectors.AttributeMap(data)
	if err != nil {
		t.Fatal(err)
	}
	return m
}

func toAV(t *testing.T, v any) types.AttributeValue {
	t.Helper()
	av, err := engine.ToAttributeValue(v)
	if err != nil {
		t.Fatal(err)
	}
	return av
}

func toAVMap(t *testing.T, m map[string]any) map[string]types.AttributeValue {
	t.Helper()
	out := make(map[string]types.AttributeValue, len(m))
	for k, v := range m {
		out[k] = toAV(t, v)
	}
	return out
}

func compare(t *testing.T, got, want map[string]types.AttributeValue) {
	t.Helper()
	if !reflect.DeepEqual(got, want) {
		t.Errorf("keys differ\n got: %s\nwant: %s", show(got), show(want))
	}
}

func show(m map[string]types.AttributeValue) string {
	var parts []string
	for k, v := range m {
		switch v := v.(type) {
		case *types.AttributeValueMemberS:
			parts = append(parts, fmt.Sprintf("%s=S:%q", k, v.Value))
		case *types.AttributeValueMemberN:
			parts = append(parts, fmt.Sprintf("%s=N:%s", k, v.Value))
		default:
			parts = append(parts, fmt.Sprintf("%s=%T", k, v))
		}
	}
	return strings.Join(parts, " ")
}
