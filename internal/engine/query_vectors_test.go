package engine_test

import (
	"encoding/base64"
	"encoding/json"
	"reflect"
	"strconv"
	"strings"
	"testing"

	"github.com/outboundani/wattdb/internal/engine"
	"github.com/outboundani/wattdb/internal/model"
	"github.com/outboundani/wattdb/internal/vectors"
)

// TestQueriesMatchVectors replays recorded queries and scans: params,
// every request sent (pages, cursors, hydrate), and the results.
func TestQueriesMatchVectors(t *testing.T) {
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
			if m == nil || len(c.Chain) == 0 {
				continue
			}
			first := c.Chain[0].Method
			if !strings.HasPrefix(first, "query.") && first != "scan" && !strings.HasPrefix(first, "scan.") && first != "find" && first != "match" {
				continue
			}
			t.Run(fileBase(v.File)+"/"+c.Name, func(t *testing.T) {
				if c.Params != nil || c.ParamsError != nil {
					checkQueryParams(t, m, v.Table, c)
				}
				if c.Go != nil {
					checkQueryGo(t, m, v.Table, c)
				}
				checked++
			})
		}
	}
	t.Logf("checked %d query cases", checked)
}

func buildQuery(t *testing.T, m *model.Model, c vectors.Case) *engine.Query {
	t.Helper()
	first := c.Chain[0]
	var q *engine.Query
	switch {
	case first.Method == "scan":
		q = engine.NewScan(m, "")
	case strings.HasPrefix(first.Method, "scan."):
		q = engine.NewScan(m, strings.TrimPrefix(first.Method, "scan."))
	case first.Method == "find":
		q = engine.NewFind(m, entriesOf(t, first.Args[0]))
	case first.Method == "match":
		q = engine.NewMatch(m, entriesOf(t, first.Args[0]))
	default:
		q = engine.NewQuery(m, strings.TrimPrefix(first.Method, "query."), entriesOf(t, first.Args[0]))
	}
	applySteps(t, q, c.Chain[1:])
	return q
}

func applySteps(t *testing.T, q *engine.Query, steps []vectors.Step) {
	t.Helper()
	ops := map[string]engine.QueryType{"gt": engine.QueryGt, "gte": engine.QueryGte, "lt": engine.QueryLt, "lte": engine.QueryLte}
	for _, step := range steps {
		switch step.Method {
		case "begins":
			q.Begins(entriesOf(t, step.Args[0]))
		case "between":
			q.Between(entriesOf(t, step.Args[0]), entriesOf(t, step.Args[1]))
		case "where":
			q.Where(whereCond(t, step.Args[0]))
		default:
			typ, ok := ops[step.Method]
			if !ok {
				t.Fatalf("unsupported step %q", step.Method)
			}
			q.Compare(typ, entriesOf(t, step.Args[0]))
		}
	}
}

type queryOpts struct {
	engine.QueryOptions
	run    engine.RunOptions
	cursor string
}

func queryOptionsOf(t *testing.T, raw json.RawMessage, exec bool) queryOpts {
	t.Helper()
	o := queryOpts{QueryOptions: engine.QueryOptions{Exec: exec}, run: engine.RunOptions{Pages: 1}}
	if raw == nil {
		return o
	}
	var x struct {
		Table           string          `json:"table"`
		Limit           int             `json:"limit"`
		Order           string          `json:"order"`
		Consistent      bool            `json:"consistent"`
		Attributes      []string        `json:"attributes"`
		IgnoreOwnership bool            `json:"ignoreOwnership"`
		Compare         string          `json:"compare"`
		Data            string          `json:"data"`
		Hydrate         bool            `json:"hydrate"`
		Pages           json.RawMessage `json:"pages"`
		Count           int             `json:"count"`
		Cursor          string          `json:"cursor"`
	}
	if err := json.Unmarshal(raw, &x); err != nil {
		t.Fatal(err)
	}
	o.Table, o.Limit, o.Order, o.Consistent = x.Table, x.Limit, x.Order, x.Consistent
	o.Attributes, o.IgnoreOwnership, o.Compare, o.Hydrate = x.Attributes, x.IgnoreOwnership, x.Compare, x.Hydrate
	o.Raw, o.IncludeKeys = x.Data == "raw", x.Data == "includeKeys"
	o.run.Count, o.run.IgnoreOwnership, o.run.Attributes = x.Count, x.IgnoreOwnership, x.Attributes
	o.cursor = x.Cursor
	switch p := strings.Trim(string(x.Pages), `"`); p {
	case "":
	case "all":
		o.run.Pages = 0
	default:
		n, _ := strconv.Atoi(p)
		o.run.Pages = n
	}
	return o
}

func checkQueryParams(t *testing.T, m *model.Model, table string, c vectors.Case) {
	t.Helper()
	o := queryOptionsOf(t, c.ParamsOptions, false)
	r, err := buildQuery(t, m, c).Build(table, o.QueryOptions)
	if c.ParamsError != nil {
		if err == nil {
			t.Fatalf("ElectroDB failed (%s) but wattdb built %v", c.ParamsError.Message, r.Params)
		}
		return
	}
	if err != nil {
		t.Fatalf("Build: %v", err)
	}
	var want any
	json.Unmarshal(c.Params[0].Request, &want)
	if got := wire(t, r.Params); !reflect.DeepEqual(got, want) {
		g, _ := json.MarshalIndent(got, "", " ")
		w, _ := json.MarshalIndent(want, "", " ")
		t.Errorf("params differ\n got: %s\nwant: %s", g, w)
	}
}

// scripted is a Runner answering from recorded responses and logging
// requests in wire form.
type scripted struct {
	t         *testing.T
	responses []json.RawMessage
	requests  []any
}

func (s *scripted) next() map[string]json.RawMessage {
	var body map[string]json.RawMessage
	if len(s.responses) > 0 {
		json.Unmarshal(s.responses[0], &body)
		s.responses = s.responses[1:]
	}
	return body
}

func (s *scripted) Page(params map[string]any) ([]map[string]any, map[string]any, error) {
	s.requests = append(s.requests, wire(s.t, params))
	body := s.next()
	var raws []json.RawMessage
	json.Unmarshal(body["Items"], &raws)
	items := make([]map[string]any, len(raws))
	for i, r := range raws {
		items[i] = engine.FromAttributeMap(attributeMap(s.t, r))
	}
	var last map[string]any
	if lek, ok := body["LastEvaluatedKey"]; ok {
		last = engine.FromAttributeMap(attributeMap(s.t, lek))
	}
	return items, last, nil
}

func (s *scripted) BatchGet(table string, keys []map[string]any) ([]map[string]any, error) {
	ks := make([]any, len(keys))
	for i, k := range keys {
		av, err := engine.ToAttributeMap(k)
		if err != nil {
			s.t.Fatal(err)
		}
		ks[i] = avJSON(av)
	}
	s.requests = append(s.requests, normalize(s.t, map[string]any{"RequestItems": map[string]any{table: map[string]any{"Keys": ks}}}))
	body := s.next()
	var responses map[string][]json.RawMessage
	json.Unmarshal(body["Responses"], &responses)
	var out []map[string]any
	for _, r := range responses[table] {
		out = append(out, engine.FromAttributeMap(attributeMap(s.t, r)))
	}
	return out, nil
}

func checkQueryGo(t *testing.T, m *model.Model, table string, c vectors.Case) {
	t.Helper()
	o := queryOptionsOf(t, c.GoOptions, true)
	r, err := buildQuery(t, m, c).Build(table, o.QueryOptions)
	if c.Go.Error != nil {
		if err == nil {
			t.Fatalf("ElectroDB failed (%s) but wattdb built a request", c.Go.Error.Message)
		}
		return
	}
	if err != nil {
		t.Fatalf("Build: %v", err)
	}
	start, err := r.StartKey(o.cursor)
	if err != nil {
		t.Fatal(err)
	}
	run := &scripted{t: t, responses: c.Responses}
	if o.Raw {
		run.Page(r.WithStartKey(start)) // raw returns the first page as is
		checkRequests(t, run.requests, c.Go.Requests)
		return
	}
	res, err := r.Run(start, o.run, run)
	if err != nil {
		t.Fatal(err)
	}
	checkRequests(t, run.requests, c.Go.Requests)

	var want struct {
		M struct {
			Data        json.RawMessage `json:"data"`
			Cursor      json.RawMessage `json:"cursor"`
			Unprocessed json.RawMessage `json:"unprocessed"`
		} `json:"M"`
	}
	json.Unmarshal(c.Go.Result, &want)
	items := make([]any, len(res.Items))
	for i, it := range res.Items {
		items[i] = it
	}
	got := toAV(t, items)
	if wantData, _ := vectors.AttributeValue(want.M.Data); !reflect.DeepEqual(got, wantData) {
		t.Errorf("data differs\n got: %v\nwant: %s", show2(t, got), want.M.Data)
	}
	gotCursor, err := engine.EncodeCursor(res.Next)
	if err != nil {
		t.Fatal(err)
	}
	var wantCursor struct {
		S *string `json:"S"`
	}
	json.Unmarshal(want.M.Cursor, &wantCursor)
	switch {
	case wantCursor.S == nil && gotCursor != "":
		t.Errorf("cursor = %s, want none", decodeCursorJSON(gotCursor))
	case wantCursor.S != nil && !reflect.DeepEqual(decodeCursorJSON(gotCursor), decodeCursorJSON(*wantCursor.S)):
		t.Errorf("cursor = %s, want %s", decodeCursorJSON(gotCursor), decodeCursorJSON(*wantCursor.S))
	}
	if want.M.Unprocessed != nil {
		un := make([]any, len(res.Unprocessed))
		for i, u := range res.Unprocessed {
			un[i] = u
		}
		if wantU, _ := vectors.AttributeValue(want.M.Unprocessed); !reflect.DeepEqual(toAV(t, un), wantU) {
			t.Errorf("unprocessed differs: got %v", res.Unprocessed)
		}
	}
}

func decodeCursorJSON(c string) any {
	b, _ := base64.RawURLEncoding.DecodeString(c)
	var v any
	json.Unmarshal(b, &v)
	return v
}

func show2(t *testing.T, v any) string {
	b, _ := json.Marshal(v)
	return string(b)
}

func checkRequests(t *testing.T, got []any, want []vectors.Request) {
	t.Helper()
	if len(got) != len(want) {
		t.Fatalf("sent %d requests, ElectroDB sent %d", len(got), len(want))
	}
	for i := range got {
		var w any
		json.Unmarshal(want[i].Request, &w)
		if !reflect.DeepEqual(got[i], w) {
			g, _ := json.MarshalIndent(got[i], "", " ")
			wb, _ := json.MarshalIndent(w, "", " ")
			t.Errorf("request %d differs\n got: %s\nwant: %s", i, g, wb)
		}
	}
}
