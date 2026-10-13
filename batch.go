package wattdb

import (
	"context"
	"sync"
	"time"

	"github.com/aws/aws-sdk-go-v2/service/dynamodb"
	"github.com/aws/aws-sdk-go-v2/service/dynamodb/types"
	"github.com/outboundani/wattdb/internal/engine"
)

// BackoffPolicy controls RetryUnprocessed: how many retries, and the delay
// before each (decision D41).
type BackoffPolicy struct {
	// Retries is the number of retries after the first attempt.
	Retries int
	// Delay returns the wait before retry n (starting at 0).
	Delay func(n int) time.Duration
}

// Backoff retries up to retries times, waiting base, 2×base, 4×base, ...
func Backoff(retries int, base time.Duration) BackoffPolicy {
	return BackoffPolicy{Retries: retries, Delay: func(n int) time.Duration { return base << n }}
}

// batch holds the per-call state of batch operations.
type batch struct {
	client     DynamoDB
	table      string
	concurrent int
	retry      *BackoffPolicy
}

func (b *batch) clientFor(cfg *Config) DynamoDB {
	if b.client != nil {
		return b.client
	}
	return cfg.client
}

// runChunks runs fn over the requests, up to b.concurrent at a time
// (ElectroDB's "concurrent" option; 1 by default).
func (b *batch) runChunks(ctx context.Context, n int, fn func(ctx context.Context, i int) error) error {
	limit := max(b.concurrent, 1)
	ctx, cancel := context.WithCancel(ctx)
	defer cancel()
	sem := make(chan struct{}, limit)
	var wg sync.WaitGroup
	var once sync.Once
	var first error
	for i := range n {
		select {
		case sem <- struct{}{}:
		case <-ctx.Done():
		}
		if ctx.Err() != nil {
			break
		}
		wg.Go(func() {
			defer func() { <-sem }()
			if err := fn(ctx, i); err != nil {
				once.Do(func() { first = err; cancel() })
			}
		})
	}
	wg.Wait()
	if first != nil {
		return first
	}
	return ctx.Err()
}

// retrying sends a request and, with RetryUnprocessed, resends what comes
// back unprocessed. send returns the unprocessed part, or nil when done.
func (b *batch) retrying(ctx context.Context, send func(attempt int) (more bool, err error)) error {
	for attempt := 0; ; attempt++ {
		more, err := send(attempt)
		if err != nil || !more || b.retry == nil || attempt >= b.retry.Retries {
			return err
		}
		t := time.NewTimer(b.retry.Delay(attempt))
		select {
		case <-ctx.Done():
			t.Stop()
			return ctx.Err()
		case <-t.C:
		}
	}
}

// keyIndex maps table keys back to the caller's inputs, so unprocessed
// work comes back as what was passed in.
type keyIndex struct {
	m     map[string][]int
	pk    string
	sk    string
	hasSK bool
}

func newKeyIndex(def *EntityDef) *keyIndex {
	t := def.model.TableIndex()
	return &keyIndex{m: map[string][]int{}, pk: t.PK.Field, sk: t.SK.Field, hasSK: t.HasSK}
}

func (k *keyIndex) id(item map[string]any) string {
	id := engine.KeyString(item[k.pk])
	if k.hasSK {
		id += "\x00" + engine.KeyString(item[k.sk])
	}
	return id
}

func (k *keyIndex) add(item map[string]any, i int) {
	id := k.id(item)
	k.m[id] = append(k.m[id], i)
}

func (k *keyIndex) lookup(item map[string]any) []int { return k.m[k.id(item)] }

// BatchGet reads many items by key (decision D41). K is the entity's Key.
type BatchGet[E, K any] struct {
	cfg     *Config
	def     *EntityDef
	table   string
	decode  func(map[string]any) (E, error)
	keys    []K
	encoded []map[string]types.AttributeValue
	err     error
	b       batch
	mutate  []func(*dynamodb.BatchGetItemInput)
}

// NewBatchGet is used by generated code.
func NewBatchGet[E, P, K any](t *Table[E, P], keys []K, encode func(K) (map[string]types.AttributeValue, error)) *BatchGet[E, K] {
	bg := &BatchGet[E, K]{cfg: t.cfg, def: t.def, table: t.table, decode: t.decodeItem, keys: keys}
	for _, k := range keys {
		av, err := encode(k)
		if err != nil {
			bg.err = err
			break
		}
		bg.encoded = append(bg.encoded, av)
	}
	return bg
}

// Concurrent sends up to n chunks of 100 keys at once.
func (g *BatchGet[E, K]) Concurrent(n int) *BatchGet[E, K] { g.b.concurrent = n; return g }

// RetryUnprocessed resends unprocessed keys with backoff.
func (g *BatchGet[E, K]) RetryUnprocessed(p BackoffPolicy) *BatchGet[E, K] { g.b.retry = &p; return g }

// Table overrides the table for this call.
func (g *BatchGet[E, K]) Table(name string) *BatchGet[E, K] { g.b.table = name; return g }

// Client overrides the DynamoDB client for this call.
func (g *BatchGet[E, K]) Client(c DynamoDB) *BatchGet[E, K] { g.b.client = c; return g }

// Mutate edits each request before it's sent.
func (g *BatchGet[E, K]) Mutate(fn func(*dynamodb.BatchGetItemInput)) *BatchGet[E, K] {
	g.mutate = append(g.mutate, fn)
	return g
}

// PreserveOrder returns the items aligned with the keys, nil where missing.
func (g *BatchGet[E, K]) PreserveOrder() *OrderedBatchGet[E, K] {
	return &OrderedBatchGet[E, K]{g: g}
}

func (g *BatchGet[E, K]) build(exec, preserve bool) (*engine.BatchGet, error) {
	if g.err != nil {
		return nil, g.err
	}
	composites := make([]engine.Attrs, len(g.encoded))
	for i, k := range g.encoded {
		composites[i] = engine.FromAttributeMap(k)
	}
	return engine.NewBatchGet(g.def.model, g.table, composites, engine.BatchGetOptions{
		Table: g.b.table, PreserveOrder: preserve, Exec: exec,
	})
}

func batchGetInput(params map[string]any, mutate []func(*dynamodb.BatchGetItemInput)) (*dynamodb.BatchGetItemInput, error) {
	in := &dynamodb.BatchGetItemInput{RequestItems: map[string]types.KeysAndAttributes{}}
	for table, body := range params["RequestItems"].(map[string]any) {
		body := body.(map[string]any)
		var ka types.KeysAndAttributes
		for k, v := range body {
			switch k {
			case "Keys":
				for _, key := range v.([]any) {
					av, err := engine.ToAttributeMap(key.(map[string]any))
					if err != nil {
						return nil, err
					}
					ka.Keys = append(ka.Keys, av)
				}
			case "ProjectionExpression":
				s := v.(string)
				ka.ProjectionExpression = &s
			case "ExpressionAttributeNames":
				ka.ExpressionAttributeNames = v.(map[string]string)
			}
		}
		in.RequestItems[table] = ka
	}
	for _, fn := range mutate {
		fn(in)
	}
	return in, nil
}

// Params returns the requests without sending them, one per chunk.
func (g *BatchGet[E, K]) Params() ([]*dynamodb.BatchGetItemInput, error) {
	b, err := g.build(false, false)
	if err != nil {
		return nil, err
	}
	var out []*dynamodb.BatchGetItemInput
	for _, p := range b.Requests {
		in, err := batchGetInput(p, g.mutate)
		if err != nil {
			return nil, err
		}
		out = append(out, in)
	}
	return out, nil
}

func (g *BatchGet[E, K]) run(ctx context.Context, preserve bool) (*engine.BatchGet, []K, error) {
	b, err := g.build(true, preserve)
	if err != nil {
		return nil, nil, err
	}
	client := g.b.clientFor(g.cfg)
	var mu sync.Mutex
	err = g.b.runChunks(ctx, len(b.Requests), func(ctx context.Context, i int) error {
		req := b.Requests[i]
		return g.b.retrying(ctx, func(attempt int) (bool, error) {
			in, err := batchGetInput(req, g.mutate)
			if err != nil {
				return false, err
			}
			out, err := client.BatchGetItem(ctx, in)
			if err != nil {
				return false, wrapAWS(err)
			}
			items := make([]map[string]any, len(out.Responses[b.Table()]))
			for j, it := range out.Responses[b.Table()] {
				items[j] = engine.FromAttributeMap(it)
			}
			var unprocessed []map[string]any
			for _, k := range out.UnprocessedKeys[b.Table()].Keys {
				unprocessed = append(unprocessed, engine.FromAttributeMap(k))
			}
			mu.Lock()
			defer mu.Unlock()
			last := g.b.retry == nil || attempt >= g.b.retry.Retries
			if last {
				err = b.Apply(items, unprocessed)
			} else {
				err = b.Apply(items, nil)
			}
			if err != nil || len(unprocessed) == 0 {
				return false, err
			}
			req = b.Retry(unprocessed)
			return true, nil
		})
	})
	if err != nil {
		return nil, nil, err
	}
	idx := newKeyIndex(g.def)
	for i, k := range requestKeys(b.Requests, b.Table()) {
		idx.add(k, i)
	}
	var unprocessed []K
	seen := map[int]bool{}
	for _, k := range b.UnprocessedKeys {
		for _, i := range idx.lookup(k) {
			if !seen[i] {
				seen[i] = true
				unprocessed = append(unprocessed, g.keys[i])
			}
		}
	}
	return b, unprocessed, nil
}

// Exec reads the items, returning those found and the keys DynamoDB left
// unprocessed.
func (g *BatchGet[E, K]) Exec(ctx context.Context) ([]E, []K, error) {
	b, unprocessed, err := g.run(ctx, false)
	if err != nil {
		return nil, nil, err
	}
	var found []E
	for _, d := range b.Data() {
		if d == nil {
			continue
		}
		v, err := g.decode(d)
		if err != nil {
			return nil, nil, err
		}
		found = append(found, v)
	}
	return found, unprocessed, nil
}

// OrderedBatchGet is a batch get whose results line up with its keys.
type OrderedBatchGet[E, K any] struct{ g *BatchGet[E, K] }

// Exec reads the items: one entry per distinct key, nil where the item is
// missing or unprocessed.
func (o *OrderedBatchGet[E, K]) Exec(ctx context.Context) ([]*E, []K, error) {
	b, unprocessed, err := o.g.run(ctx, true)
	if err != nil {
		return nil, nil, err
	}
	data := b.Data()
	out := make([]*E, len(data))
	for i, d := range data {
		if d == nil {
			continue
		}
		v, err := o.g.decode(d)
		if err != nil {
			return nil, nil, err
		}
		out[i] = &v
	}
	return out, unprocessed, nil
}

// BatchWrite puts or deletes many items (decision D41). T is what the
// caller passed: the entity's Input for puts, its Key for deletes.
type BatchWrite[T any] struct {
	cfg     *Config
	def     *EntityDef
	table   string
	put     bool
	inputs  []T
	encoded []map[string]types.AttributeValue
	err     error
	b       batch
	mutate  []func(*dynamodb.BatchWriteItemInput)
}

// NewBatchPut is used by generated code.
func NewBatchPut[E, P, I any](t *Table[E, P], inputs []I, encode func(I) (map[string]types.AttributeValue, error)) *BatchWrite[I] {
	return newBatchWrite(t.cfg, t.def, t.table, true, inputs, encode)
}

// NewBatchDelete is used by generated code.
func NewBatchDelete[E, P, K any](t *Table[E, P], keys []K, encode func(K) (map[string]types.AttributeValue, error)) *BatchWrite[K] {
	return newBatchWrite(t.cfg, t.def, t.table, false, keys, encode)
}

func newBatchWrite[T any](cfg *Config, def *EntityDef, table string, put bool, inputs []T, encode func(T) (map[string]types.AttributeValue, error)) *BatchWrite[T] {
	bw := &BatchWrite[T]{cfg: cfg, def: def, table: table, put: put, inputs: inputs}
	for _, in := range inputs {
		av, err := encode(in)
		if err != nil {
			bw.err = err
			break
		}
		bw.encoded = append(bw.encoded, av)
	}
	return bw
}

// Concurrent sends up to n chunks of 25 writes at once.
func (w *BatchWrite[T]) Concurrent(n int) *BatchWrite[T] { w.b.concurrent = n; return w }

// RetryUnprocessed resends unprocessed writes with backoff.
func (w *BatchWrite[T]) RetryUnprocessed(p BackoffPolicy) *BatchWrite[T] { w.b.retry = &p; return w }

// Table overrides the table for this call.
func (w *BatchWrite[T]) Table(name string) *BatchWrite[T] { w.b.table = name; return w }

// Client overrides the DynamoDB client for this call.
func (w *BatchWrite[T]) Client(c DynamoDB) *BatchWrite[T] { w.b.client = c; return w }

// Mutate edits each request before it's sent.
func (w *BatchWrite[T]) Mutate(fn func(*dynamodb.BatchWriteItemInput)) *BatchWrite[T] {
	w.mutate = append(w.mutate, fn)
	return w
}

func (w *BatchWrite[T]) build() (*engine.BatchWrite, error) {
	if w.err != nil {
		return nil, w.err
	}
	items := make([]engine.Attrs, len(w.encoded))
	for i, av := range w.encoded {
		items[i] = engine.FromAttributeMap(av)
	}
	opt := engine.BatchWriteOptions{Table: w.b.table}
	if w.put {
		return engine.NewBatchPut(w.def.model, w.table, items, opt)
	}
	return engine.NewBatchDelete(w.def.model, w.table, items, opt)
}

func batchWriteInput(params map[string]any, mutate []func(*dynamodb.BatchWriteItemInput)) (*dynamodb.BatchWriteItemInput, error) {
	in := &dynamodb.BatchWriteItemInput{RequestItems: map[string][]types.WriteRequest{}}
	for table, reqs := range params["RequestItems"].(map[string]any) {
		for _, r := range reqs.([]any) {
			r := r.(map[string]any)
			var wr types.WriteRequest
			if p, ok := r["PutRequest"].(map[string]any); ok {
				av, err := engine.ToAttributeMap(p["Item"].(map[string]any))
				if err != nil {
					return nil, err
				}
				wr.PutRequest = &types.PutRequest{Item: av}
			} else {
				d := r["DeleteRequest"].(map[string]any)
				av, err := engine.ToAttributeMap(d["Key"].(map[string]any))
				if err != nil {
					return nil, err
				}
				wr.DeleteRequest = &types.DeleteRequest{Key: av}
			}
			in.RequestItems[table] = append(in.RequestItems[table], wr)
		}
	}
	for _, fn := range mutate {
		fn(in)
	}
	return in, nil
}

// Params returns the requests without sending them, one per chunk.
func (w *BatchWrite[T]) Params() ([]*dynamodb.BatchWriteItemInput, error) {
	b, err := w.build()
	if err != nil {
		return nil, err
	}
	var out []*dynamodb.BatchWriteItemInput
	for _, p := range b.Requests {
		in, err := batchWriteInput(p, w.mutate)
		if err != nil {
			return nil, err
		}
		out = append(out, in)
	}
	return out, nil
}

// Exec sends the writes, returning the inputs DynamoDB left unprocessed.
func (w *BatchWrite[T]) Exec(ctx context.Context) ([]T, error) {
	b, err := w.build()
	if err != nil {
		return nil, err
	}
	client := w.b.clientFor(w.cfg)
	var mu sync.Mutex
	var left []map[string]any
	err = w.b.runChunks(ctx, len(b.Requests), func(ctx context.Context, i int) error {
		req := b.Requests[i]
		return w.b.retrying(ctx, func(attempt int) (bool, error) {
			in, err := batchWriteInput(req, w.mutate)
			if err != nil {
				return false, err
			}
			out, err := client.BatchWriteItem(ctx, in)
			if err != nil {
				return false, wrapAWS(err)
			}
			var unprocessed []map[string]any
			for _, wr := range out.UnprocessedItems[b.Table()] {
				switch {
				case wr.PutRequest != nil:
					unprocessed = append(unprocessed, map[string]any{"PutRequest": map[string]any{"Item": engine.FromAttributeMap(wr.PutRequest.Item)}})
				case wr.DeleteRequest != nil:
					unprocessed = append(unprocessed, map[string]any{"DeleteRequest": map[string]any{"Key": engine.FromAttributeMap(wr.DeleteRequest.Key)}})
				}
			}
			if len(unprocessed) == 0 {
				return false, nil
			}
			if w.b.retry == nil || attempt >= w.b.retry.Retries {
				mu.Lock()
				left = append(left, unprocessed...)
				mu.Unlock()
				return false, nil
			}
			req = b.Retry(unprocessed)
			return true, nil
		})
	})
	if err != nil {
		return nil, err
	}
	// Map unprocessed writes back to the caller's inputs by table key.
	idx := newKeyIndex(w.def)
	for i, k := range requestKeys(b.Requests, b.Table()) {
		idx.add(k, i)
	}
	var out []T
	seen := map[int]bool{}
	for _, r := range left {
		for _, i := range idx.lookup(writeKey(r)) {
			if !seen[i] {
				seen[i] = true
				out = append(out, w.inputs[i])
			}
		}
	}
	return out, nil
}

// requestKeys lists the keys (or put items) of batch requests, in order.
func requestKeys(reqs []map[string]any, table string) []map[string]any {
	var out []map[string]any
	for _, req := range reqs {
		switch body := req["RequestItems"].(map[string]any)[table].(type) {
		case map[string]any:
			for _, k := range body["Keys"].([]any) {
				out = append(out, k.(map[string]any))
			}
		case []any:
			for _, r := range body {
				out = append(out, writeKey(r.(map[string]any)))
			}
		}
	}
	return out
}

// writeKey returns a write request's item or key.
func writeKey(r map[string]any) map[string]any {
	if p, ok := r["PutRequest"].(map[string]any); ok {
		return p["Item"].(map[string]any)
	}
	return r["DeleteRequest"].(map[string]any)["Key"].(map[string]any)
}
