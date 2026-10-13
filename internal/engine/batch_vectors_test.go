package engine_test

import (
	"encoding/json"
	"reflect"
	"testing"

	"github.com/outboundani/wattdb/internal/engine"
	"github.com/outboundani/wattdb/internal/vectors"
)

// TestBatchesMatchVectors replays recorded batch gets, puts and deletes:
// the chunked requests, and the formatted results and unprocessed items.
func TestBatchesMatchVectors(t *testing.T) {
	checked := 0
	for _, vc := range vectorCases(t) {
		first := vc.c.Chain[0]
		if len(first.Args) == 0 || isObject(first.Args[0]) {
			continue
		}
		switch first.Method {
		case "get", "put", "delete":
		default:
			continue
		}
		t.Run(vc.name, func(t *testing.T) {
			var raws []json.RawMessage
			if err := json.Unmarshal(first.Args[0], &raws); err != nil {
				t.Fatal(err)
			}
			args := make([]engine.Attrs, len(raws))
			for i, r := range raws {
				a, err := vectors.Attrs(r)
				if err != nil {
					t.Fatal(err)
				}
				args[i] = a
			}
			var opts struct {
				PreserveBatchOrder bool     `json:"preserveBatchOrder"`
				Attributes         []string `json:"attributes"`
				IgnoreOwnership    bool     `json:"ignoreOwnership"`
			}
			if vc.c.GoOptions != nil {
				json.Unmarshal(vc.c.GoOptions, &opts)
			}

			var requests []map[string]any
			var apply func(resp map[string]json.RawMessage)
			var result func() any
			var err error
			switch first.Method {
			case "get":
				var b *engine.BatchGet
				b, err = engine.NewBatchGet(vc.m, vc.table, args, engine.BatchGetOptions{
					Attributes: opts.Attributes, IgnoreOwnership: opts.IgnoreOwnership,
					PreserveOrder: opts.PreserveBatchOrder, Exec: true,
				})
				if b != nil {
					requests = b.Requests
					apply = func(resp map[string]json.RawMessage) {
						var body struct {
							Responses       map[string][]json.RawMessage `json:"Responses"`
							UnprocessedKeys map[string]struct {
								Keys []json.RawMessage `json:"Keys"`
							} `json:"UnprocessedKeys"`
						}
						json.Unmarshal(mustJSON(t, resp), &body)
						if err := b.Apply(items(t, body.Responses[vc.table]), items(t, body.UnprocessedKeys[vc.table].Keys)); err != nil {
							t.Fatal(err)
						}
					}
					result = func() any {
						un := make([]any, len(b.Unprocessed))
						for i, u := range b.Unprocessed {
							un[i] = map[string]any(u)
						}
						data := make([]any, len(b.Data()))
						for i, d := range b.Data() {
							if d != nil {
								data[i] = d
							}
						}
						return map[string]any{"data": data, "unprocessed": un}
					}
				}
			default:
				var b *engine.BatchWrite
				if first.Method == "put" {
					b, err = engine.NewBatchPut(vc.m, vc.table, args, engine.BatchWriteOptions{})
				} else {
					b, err = engine.NewBatchDelete(vc.m, vc.table, args, engine.BatchWriteOptions{})
				}
				if b != nil {
					requests = b.Requests
					apply = func(resp map[string]json.RawMessage) {
						var body struct {
							UnprocessedItems map[string][]struct {
								PutRequest *struct {
									Item json.RawMessage `json:"Item"`
								} `json:"PutRequest"`
								DeleteRequest *struct {
									Key json.RawMessage `json:"Key"`
								} `json:"DeleteRequest"`
							} `json:"UnprocessedItems"`
						}
						json.Unmarshal(mustJSON(t, resp), &body)
						var reqs []map[string]any
						for _, r := range body.UnprocessedItems[vc.table] {
							if r.PutRequest != nil {
								reqs = append(reqs, map[string]any{"PutRequest": map[string]any{"Item": engine.FromAttributeMap(attributeMap(t, r.PutRequest.Item))}})
							} else {
								reqs = append(reqs, map[string]any{"DeleteRequest": map[string]any{"Key": engine.FromAttributeMap(attributeMap(t, r.DeleteRequest.Key))}})
							}
						}
						if err := b.Apply(reqs); err != nil {
							t.Fatal(err)
						}
					}
					result = func() any {
						un := make([]any, len(b.Unprocessed))
						for i, u := range b.Unprocessed {
							un[i] = u
						}
						return map[string]any{"unprocessed": un}
					}
				}
			}

			wantErr := vc.c.ParamsError != nil || (vc.c.Go != nil && vc.c.Go.Error != nil)
			if wantErr {
				if err == nil {
					t.Fatal("ElectroDB failed but wattdb built the batch")
				}
				return
			}
			if err != nil {
				t.Fatal(err)
			}
			if vc.c.Params != nil {
				checkRequests(t, batchWire(t, requests), vc.c.Params)
			}
			if vc.c.Go == nil {
				return
			}
			responses := vc.c.Responses
			for range requests {
				var body map[string]json.RawMessage
				if len(responses) > 0 {
					json.Unmarshal(responses[0], &body)
					responses = responses[1:]
				}
				apply(body)
			}
			checkRequests(t, batchWire(t, requests), vc.c.Go.Requests)
			want, err := vectors.AttributeValue(vc.c.Go.Result)
			if err != nil {
				t.Fatal(err)
			}
			if got := toAV(t, result()); !reflect.DeepEqual(got, want) {
				t.Errorf("result differs\n got: %s\nwant: %s", show2(t, result()), vc.c.Go.Result)
			}
			checked++
		})
	}
	if checked == 0 {
		t.Fatal("no batch cases were checked")
	}
}

func mustJSON(t *testing.T, v any) []byte {
	t.Helper()
	b, err := json.Marshal(v)
	if err != nil {
		t.Fatal(err)
	}
	return b
}

func items(t *testing.T, raws []json.RawMessage) []map[string]any {
	out := make([]map[string]any, len(raws))
	for i, r := range raws {
		out[i] = engine.FromAttributeMap(attributeMap(t, r))
	}
	return out
}

// batchWire renders batch requests in wire form.
func batchWire(t *testing.T, reqs []map[string]any) []any {
	t.Helper()
	av := func(v any) any {
		m, err := engine.ToAttributeMap(v.(map[string]any))
		if err != nil {
			t.Fatal(err)
		}
		return avJSON(m)
	}
	out := make([]any, len(reqs))
	for i, r := range reqs {
		ri := map[string]any{}
		for table, body := range r["RequestItems"].(map[string]any) {
			switch body := body.(type) {
			case map[string]any:
				b := map[string]any{}
				for k, v := range body {
					b[k] = v
				}
				keys := body["Keys"].([]any)
				ks := make([]any, len(keys))
				for j, k := range keys {
					ks[j] = av(k)
				}
				b["Keys"] = ks
				ri[table] = b
			case []any:
				ws := make([]any, len(body))
				for j, w := range body {
					w := w.(map[string]any)
					if p, ok := w["PutRequest"].(map[string]any); ok {
						ws[j] = map[string]any{"PutRequest": map[string]any{"Item": av(p["Item"])}}
					} else {
						d := w["DeleteRequest"].(map[string]any)
						ws[j] = map[string]any{"DeleteRequest": map[string]any{"Key": av(d["Key"])}}
					}
				}
				ri[table] = ws
			}
		}
		out[i] = normalize(t, map[string]any{"RequestItems": ri})
	}
	return out
}
