package engine_test

import (
	"encoding/json"
	"reflect"
	"strings"
	"testing"

	"github.com/outboundani/wattdb/internal/engine"
	"github.com/outboundani/wattdb/internal/model"
	"github.com/outboundani/wattdb/internal/vectors"
)

// TestCollectionsMatchVectors replays recorded collection queries: params,
// requests and the per-entity results.
func TestCollectionsMatchVectors(t *testing.T) {
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
			if len(c.Chain) == 0 || !strings.HasPrefix(c.Chain[0].Method, "collections.") {
				continue
			}
			name := strings.TrimPrefix(c.Chain[0].Method, "collections.")
			var members []engine.CollectionMember
			for _, alias := range v.Services[c.Service] {
				if _, _, ok := models[alias].CollectionIndex(name); ok {
					members = append(members, engine.CollectionMember{Alias: alias, Model: models[alias]})
				}
			}
			t.Run(fileBase(v.File)+"/"+c.Name, func(t *testing.T) {
				build := func() *engine.Query {
					q := engine.NewCollection(name, members, entriesOf(t, c.Chain[0].Args[0]))
					applySteps(t, q, c.Chain[1:])
					return q
				}
				if c.Params != nil {
					o := queryOptionsOf(t, c.ParamsOptions, false)
					r, err := build().Build(v.Table, o.QueryOptions)
					if err != nil {
						t.Fatal(err)
					}
					var want any
					json.Unmarshal(c.Params[0].Request, &want)
					if got := wire(t, r.Params); !reflect.DeepEqual(got, want) {
						g, _ := json.MarshalIndent(got, "", " ")
						t.Errorf("params differ\n got: %s\nwant: %s", g, c.Params[0].Request)
					}
				}
				if c.Go == nil {
					return
				}
				o := queryOptionsOf(t, c.GoOptions, true)
				r, err := build().Build(v.Table, o.QueryOptions)
				if err != nil {
					t.Fatal(err)
				}
				run := &scripted{t: t, responses: c.Responses}
				res, err := r.Run(nil, o.run, run)
				if err != nil {
					t.Fatal(err)
				}
				checkRequests(t, run.requests, c.Go.Requests)
				data := map[string]any{}
				for i, mem := range members {
					items := make([]any, len(res.Groups[i]))
					for j, it := range res.Groups[i] {
						items[j] = it
					}
					data[mem.Alias] = items
				}
				var cursor any
				if res.Next != nil {
					cursor, _ = engine.EncodeCursor(res.Next)
				}
				got := map[string]any{"data": data, "cursor": cursor}
				want, err := vectors.AttributeValue(c.Go.Result)
				if err != nil {
					t.Fatal(err)
				}
				if !reflect.DeepEqual(toAV(t, got), want) {
					t.Errorf("result differs\n got: %s\nwant: %s", show2(t, got), c.Go.Result)
				}
			})
			checked++
		}
	}
	if checked == 0 {
		t.Fatal("no collection cases were checked")
	}
}
