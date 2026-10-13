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

// TestTransactionsMatchVectors replays recorded write and get transactions:
// params, and results for committed, canceled and get transactions.
func TestTransactionsMatchVectors(t *testing.T) {
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
			if len(c.Chain) == 0 || !strings.HasPrefix(c.Chain[0].Method, "transaction.") {
				continue
			}
			t.Run(fileBase(v.File)+"/"+c.Name, func(t *testing.T) {
				checkTx(t, models, v.Table, c)
				checked++
			})
		}
	}
	if checked == 0 {
		t.Fatal("no transaction cases were checked")
	}
}

func checkTx(t *testing.T, models map[string]*model.Model, table string, c vectors.Case) {
	t.Helper()
	write := c.Chain[0].Method == "transaction.write"
	var fn struct {
		Args []struct {
			Entity string              `json:"entity"`
			Chain  [][]json.RawMessage `json:"chain"`
			Commit json.RawMessage     `json:"commit"`
		} `json:"args"`
	}
	if err := json.Unmarshal(c.Chain[0].Args[0], &fn); err != nil {
		t.Fatal(err)
	}
	build := func(exec bool) []*engine.Request {
		var reqs []*engine.Request
		for _, op := range fn.Args {
			chain, err := vectors.ParseChain(op.Chain)
			if err != nil {
				t.Fatal(err)
			}
			opt := optionsOf(t, op.Commit, exec)
			opt.Transaction = true
			r, err := buildWriteChain(t, models[op.Entity], chain).Build(table, opt)
			if err != nil {
				t.Fatal(err)
			}
			reqs = append(reqs, r)
		}
		return reqs
	}
	token := func(raw json.RawMessage) string {
		var o struct {
			Token string `json:"token"`
		}
		json.Unmarshal(raw, &o)
		return o.Token
	}
	if c.Params != nil {
		checkRequests(t, []any{txWire(t, engine.TxParams(build(false), token(c.ParamsOptions)))}, c.Params)
	}
	if c.Go == nil {
		return
	}
	reqs := build(true)
	got := txWire(t, engine.TxParams(reqs, token(c.GoOptions)))
	var want any
	json.Unmarshal(c.Go.Requests[0].Request, &want)
	if !write || token(c.GoOptions) != "" {
		checkRequests(t, []any{got}, c.Go.Requests)
	} else {
		// ElectroDB sends a random token when none is given.
		delete(want.(map[string]any), "ClientRequestToken")
		if !reflect.DeepEqual(got, want) {
			t.Errorf("request differs")
		}
	}

	var resp struct {
		Type    string `json:"__type"`
		Reasons []struct {
			Code    string          `json:"Code"`
			Message string          `json:"Message"`
			Item    json.RawMessage `json:"Item"`
		} `json:"CancellationReasons"`
		Responses []struct {
			Item json.RawMessage `json:"Item"`
		} `json:"Responses"`
	}
	if len(c.Responses) > 0 {
		json.Unmarshal(c.Responses[0], &resp)
	}
	var results []engine.TxResult
	canceled := strings.HasSuffix(resp.Type, "TransactionCanceledException")
	for i, r := range reqs {
		var res engine.TxResult
		var err error
		switch {
		case canceled:
			var item map[string]any
			if resp.Reasons[i].Item != nil {
				item = engine.FromAttributeMap(attributeMap(t, resp.Reasons[i].Item))
			}
			res, err = r.TxCanceledResult(resp.Reasons[i].Code, resp.Reasons[i].Message, item)
		case write:
			res = engine.TxWriteResult()
		default:
			var item map[string]any
			if resp.Responses[i].Item != nil {
				item = engine.FromAttributeMap(attributeMap(t, resp.Responses[i].Item))
			}
			res, err = r.TxGetResult(item)
		}
		if err != nil {
			t.Fatal(err)
		}
		results = append(results, res)
	}
	data := make([]any, len(results))
	for i, r := range results {
		var item any
		if r.Item != nil {
			item = r.Item
		}
		m := map[string]any{"rejected": r.Rejected, "item": item}
		if write {
			m["code"] = r.Code
			if r.Message != "" {
				m["message"] = r.Message
			}
		}
		data[i] = m
	}
	wantResult, err := vectors.AttributeValue(c.Go.Result)
	if err != nil {
		t.Fatal(err)
	}
	gotResult := toAV(t, map[string]any{"data": data, "canceled": canceled})
	if !reflect.DeepEqual(gotResult, wantResult) {
		t.Errorf("result differs\n got: %s\nwant: %s", show2(t, map[string]any{"data": data, "canceled": canceled}), c.Go.Result)
	}
}

func txWire(t *testing.T, params map[string]any) any {
	t.Helper()
	out := map[string]any{}
	for k, v := range params {
		if k != "TransactItems" {
			out[k] = v
			continue
		}
		items := v.([]any)
		ws := make([]any, len(items))
		for i, it := range items {
			w := map[string]any{}
			for op, p := range it.(map[string]any) {
				w[op] = wire(t, p.(map[string]any))
			}
			ws[i] = w
		}
		out[k] = ws
	}
	return normalize(t, out)
}
