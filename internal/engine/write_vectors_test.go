package engine_test

import (
	"encoding/json"
	"fmt"
	"reflect"
	"slices"
	"strings"
	"testing"

	"github.com/aws/aws-sdk-go-v2/service/dynamodb/types"
	"github.com/outboundani/wattdb/internal/engine"
	"github.com/outboundani/wattdb/internal/model"
	"github.com/outboundani/wattdb/internal/vectors"
)

var writeMethods = []string{"get", "put", "create", "delete", "remove", "update", "patch", "upsert"}

// TestWritesMatchVectors drives every recorded single-item case through the
// engine and compares the whole request (placeholders included) and the
// formatted result.
func TestWritesMatchVectors(t *testing.T) {
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
				t.Fatal(err)
			}
			models[e.Name] = m
		}
		for _, c := range v.Cases {
			m := models[c.Entity]
			if m == nil || len(c.Chain) == 0 || !slices.Contains(writeMethods, c.Chain[0].Method) {
				continue
			}
			if len(c.Chain[0].Args) == 0 || !isObject(c.Chain[0].Args[0]) {
				continue // batch forms
			}
			name := strings.TrimSuffix(fileBase(v.File), ".json") + "/" + c.Name
			t.Run(name, func(t *testing.T) {
				checkParams(t, m, v.Table, c)
				checkGo(t, m, v.Table, c)
				checked++
			})
		}
	}
	t.Logf("checked %d single-item cases", checked)
}

func fileBase(p string) string { return p[strings.LastIndex(p, "/")+1:] }

// buildWrite replays a case's chain on the engine builder.
func buildWrite(t *testing.T, m *model.Model, c vectors.Case) *engine.Write {
	t.Helper()
	return buildWriteChain(t, m, c.Chain)
}

func buildWriteChain(t *testing.T, m *model.Model, chain []vectors.Step) *engine.Write {
	t.Helper()
	first := chain[0]
	attrs := mustAttrs(t, first.Args[0])
	var w *engine.Write
	switch first.Method {
	case "get":
		w = engine.NewGet(m, attrs)
	case "check":
		w = engine.NewCheck(m, attrs)
	case "put", "create":
		w = engine.NewPut(m, attrs, first.Method == "create")
	case "delete", "remove":
		w = engine.NewDelete(m, attrs, first.Method == "remove")
	case "update", "patch":
		w = engine.NewUpdate(m, attrs, first.Method == "patch")
	case "upsert":
		keys, err := vectors.Keys(first.Args[0])
		if err != nil {
			t.Fatal(err)
		}
		w = engine.NewUpsert(m, attrs, keys)
	}
	for _, step := range chain[1:] {
		switch step.Method {
		case "set", "add", "subtract", "append", "delete", "ifNotExists", "composite":
			entries := entriesOf(t, step.Args[0])
			if step.Method == "composite" {
				w.Composite(entries)
			} else {
				w.Apply(step.Method, entries)
			}
		case "remove":
			var paths []string
			if err := json.Unmarshal(step.Args[0], &paths); err != nil {
				t.Fatal(err)
			}
			w.Remove(paths)
		case "data":
			w.Data(dataOps(t, step.Args[0]))
		case "where":
			w.Where(whereCond(t, step.Args[0]))
		default:
			t.Fatalf("unsupported chain step %q", step.Method)
		}
	}
	return w
}

func mustAttrs(t *testing.T, raw json.RawMessage) engine.Attrs {
	t.Helper()
	a, err := vectors.Attrs(raw)
	if err != nil {
		t.Fatal(err)
	}
	return a
}

func entriesOf(t *testing.T, raw json.RawMessage) []engine.Entry {
	t.Helper()
	keys, err := vectors.Keys(raw)
	if err != nil {
		t.Fatal(err)
	}
	values := mustAttrs(t, raw)
	out := make([]engine.Entry, len(keys))
	for i, k := range keys {
		out[i] = engine.Entry{Path: k, Value: values[k]}
	}
	return out
}

// fnArgs decodes {"$fn": name, "args": [...]}.
func fnArgs(t *testing.T, raw json.RawMessage, want string) []json.RawMessage {
	t.Helper()
	var fn vectors.Fn
	if err := json.Unmarshal(raw, &fn); err != nil || fn.Name != want {
		t.Fatalf("expected $fn %q, got %s", want, raw)
	}
	return fn.Args
}

func dataOps(t *testing.T, raw json.RawMessage) []engine.DataOp {
	t.Helper()
	var ops []engine.DataOp
	for _, st := range fnArgs(t, raw, "data") {
		var parts []json.RawMessage
		if err := json.Unmarshal(st, &parts); err != nil {
			t.Fatal(err)
		}
		var op, path string
		json.Unmarshal(parts[0], &op)
		json.Unmarshal(parts[1], &path)
		d := engine.DataOp{Op: op, Path: engine.ParsePath(path)}
		for _, rv := range parts[2:] {
			val, err := vectors.Value(rv)
			if err != nil {
				t.Fatal(err)
			}
			d.Values = append(d.Values, val)
		}
		ops = append(ops, d)
	}
	return ops
}

func whereCond(t *testing.T, raw json.RawMessage) engine.Cond {
	t.Helper()
	args := fnArgs(t, raw, "where")
	return condOf(t, args[0])
}

func condOf(t *testing.T, raw json.RawMessage) engine.Cond {
	t.Helper()
	var parts []json.RawMessage
	if err := json.Unmarshal(raw, &parts); err != nil {
		t.Fatal(err)
	}
	var head string
	json.Unmarshal(parts[0], &head)
	switch head {
	case "and", "or":
		var list []engine.Cond
		for _, p := range parts[1:] {
			list = append(list, condOf(t, p))
		}
		if head == "and" {
			return engine.CondAnd(list)
		}
		return engine.CondOr(list)
	case "not":
		return engine.CondNot{C: condOf(t, parts[1])}
	default:
		var path string
		json.Unmarshal(parts[1], &path)
		op := engine.CondOp{Op: head, Path: engine.ParsePath(path)}
		for _, rv := range parts[2:] {
			val, err := vectors.Value(rv)
			if err != nil {
				t.Fatal(err)
			}
			op.Values = append(op.Values, val)
		}
		return op
	}
}

func optionsOf(t *testing.T, raw json.RawMessage, exec bool) engine.Options {
	t.Helper()
	o := engine.Options{Exec: exec}
	if raw == nil {
		return o
	}
	var x struct {
		Table          string          `json:"table"`
		Response       string          `json:"response"`
		Consistent     bool            `json:"consistent"`
		Attributes     []string        `json:"attributes"`
		Data           string          `json:"data"`
		ReturnRejected json.RawMessage `json:"returnOnConditionCheckFailure"`
	}
	if err := json.Unmarshal(raw, &x); err != nil {
		t.Fatal(err)
	}
	o.Table, o.Response, o.Consistent, o.Attributes = x.Table, x.Response, x.Consistent, x.Attributes
	o.IncludeKeys = x.Data == "includeKeys"
	switch s := strings.Trim(string(x.ReturnRejected), `"`); s {
	case "true", "all_old":
		o.ReturnRejected = s
	}
	return o
}

// wire renders engine params in the recorder's JSON form.
func wire(t *testing.T, params map[string]any) any {
	t.Helper()
	out := map[string]any{}
	for k, v := range params {
		switch v := v.(type) {
		case map[string]any:
			av, err := engine.ToAttributeMap(v)
			if err != nil {
				t.Fatal(err)
			}
			out[k] = avJSON(av)
		default:
			out[k] = v
		}
	}
	return normalize(t, out)
}

func avJSON(m map[string]types.AttributeValue) map[string]any {
	out := map[string]any{}
	for k, v := range m {
		out[k] = avToJSON(v)
	}
	return out
}

func avToJSON(av types.AttributeValue) any {
	switch v := av.(type) {
	case *types.AttributeValueMemberS:
		return map[string]any{"S": v.Value}
	case *types.AttributeValueMemberN:
		return map[string]any{"N": v.Value}
	case *types.AttributeValueMemberBOOL:
		return map[string]any{"BOOL": v.Value}
	case *types.AttributeValueMemberNULL:
		return map[string]any{"NULL": true}
	case *types.AttributeValueMemberSS:
		return map[string]any{"SS": v.Value}
	case *types.AttributeValueMemberNS:
		return map[string]any{"NS": v.Value}
	case *types.AttributeValueMemberL:
		l := make([]any, len(v.Value))
		for i, e := range v.Value {
			l[i] = avToJSON(e)
		}
		return map[string]any{"L": l}
	case *types.AttributeValueMemberM:
		return map[string]any{"M": avJSON(v.Value)}
	default:
		return fmt.Sprintf("%T", av)
	}
}

func normalize(t *testing.T, v any) any {
	t.Helper()
	b, err := json.Marshal(v)
	if err != nil {
		t.Fatal(err)
	}
	var out any
	json.Unmarshal(b, &out)
	return out
}

func checkParams(t *testing.T, m *model.Model, table string, c vectors.Case) {
	t.Helper()
	if c.Params == nil && c.ParamsError == nil {
		return
	}
	req, err := buildWrite(t, m, c).Build(table, optionsOf(t, c.ParamsOptions, false))
	if c.ParamsError != nil {
		if strings.Contains(c.ParamsError.Message, "MAX_SAFE_INTEGER") || strings.Contains(c.ParamsError.Message, "non-empty set") {
			return // DocumentClient marshalling (D7, D18)
		}
		if err == nil {
			t.Fatalf("ElectroDB failed (%s) but wattdb built %v", c.ParamsError.Message, req.Params)
		}
		return
	}
	if err != nil {
		t.Fatalf("Build: %v", err)
	}
	var want any
	json.Unmarshal(c.Params[0].Request, &want)
	if got := wire(t, req.Params); !reflect.DeepEqual(got, want) {
		g, _ := json.MarshalIndent(got, "", " ")
		w, _ := json.MarshalIndent(want, "", " ")
		t.Errorf("params differ\n got: %s\nwant: %s", g, w)
	}
}

func checkGo(t *testing.T, m *model.Model, table string, c vectors.Case) {
	t.Helper()
	if c.Go == nil {
		return
	}
	req, err := buildWrite(t, m, c).Build(table, optionsOf(t, c.GoOptions, true))
	if c.Go.Error != nil && len(c.Go.Requests) == 0 {
		if !strings.Contains(c.Go.Error.Message, "MAX_SAFE_INTEGER") && !strings.Contains(c.Go.Error.Message, "non-empty set") && err == nil {
			t.Fatalf("ElectroDB failed (%s) but wattdb built a request", c.Go.Error.Message)
		}
		return
	}
	if err != nil {
		t.Fatalf("Build: %v", err)
	}
	var want any
	json.Unmarshal(c.Go.Requests[0].Request, &want)
	if got := wire(t, req.Params); !reflect.DeepEqual(got, want) {
		g, _ := json.MarshalIndent(got, "", " ")
		w, _ := json.MarshalIndent(want, "", " ")
		t.Errorf("go request differs\n got: %s\nwant: %s", g, w)
	}
	if c.Go.Error != nil {
		return // AWS errors without returnOnConditionCheckFailure
	}

	var resp engine.Response
	var body map[string]json.RawMessage
	if len(c.Responses) > 0 {
		json.Unmarshal(c.Responses[0], &body)
	}
	if _, failed := body["__type"]; failed {
		var item map[string]any
		if raw, ok := body["Item"]; ok {
			item = engine.FromAttributeMap(attributeMap(t, raw))
		}
		resp, err = req.FormatRejected(item)
	} else {
		fields := map[string]any{}
		for _, k := range []string{"Item", "Attributes"} {
			if raw, ok := body[k]; ok {
				fields[k] = engine.FromAttributeMap(attributeMap(t, raw))
			}
		}
		resp, err = req.Format(fields)
	}
	if err != nil {
		t.Fatalf("Format: %v", err)
	}
	got := map[string]any{}
	if resp.Rejected {
		got["rejected"] = map[string]any{"BOOL": true}
	}
	switch {
	case resp.Data != nil:
		av, err := engine.ToAttributeMap(resp.Data)
		if err != nil {
			t.Fatal(err)
		}
		got["data"] = map[string]any{"M": avJSON(av)}
	case resp.Null:
		got["data"] = map[string]any{"NULL": true}
	}
	var want2 any
	json.Unmarshal(c.Go.Result, &want2)
	if g := normalize(t, map[string]any{"M": got}); !reflect.DeepEqual(g, want2) {
		gb, _ := json.Marshal(g)
		wb, _ := json.Marshal(want2)
		t.Errorf("result differs\n got: %s\nwant: %s", gb, wb)
	}
}
