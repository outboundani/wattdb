package wattdb

import (
	"context"
	"errors"
	"fmt"
	"iter"

	"github.com/aws/aws-sdk-go-v2/service/dynamodb"
	"github.com/aws/aws-sdk-go-v2/service/dynamodb/types"
	"github.com/outboundani/wattdb/internal/engine"
)

// Cursor resumes a query or scan where a previous one stopped (decision
// D32). It's interchangeable with ElectroDB cursors. Empty means "no more".
type Cursor string

// Page is one page of results and the cursor after it.
type Page[T any] struct {
	Items  []T
	Cursor Cursor
}

// WithKeys is an item with the key attributes stored alongside it
// (IncludeKeys, decision D38).
type WithKeys[T any] struct {
	Item T
	Keys map[string]types.AttributeValue
}

// CompareMode chooses how sort-key comparisons treat composite values
// (decision D51).
type CompareMode string

const (
	// CompareKeys compares the built key strings (the default).
	CompareKeys CompareMode = "keys"
	// CompareAttributes also filters on the composite attributes.
	CompareAttributes CompareMode = "attributes"
)

// ErrUnprocessed is returned (with the items found) when Hydrate couldn't
// fetch some items; the error is an *UnprocessedError.
var ErrUnprocessed = errors.New("wattdb: some items weren't fetched")

// UnprocessedError lists the keys Hydrate couldn't fetch.
type UnprocessedError struct {
	Keys []map[string]types.AttributeValue
}

func (e *UnprocessedError) Error() string {
	return fmt.Sprintf("wattdb: %d items weren't fetched", len(e.Keys))
}

func (e *UnprocessedError) Unwrap() error { return ErrUnprocessed }

// Facet is a key composite value; generated code passes them to Query.
type Facet struct {
	Name  string
	Value types.AttributeValue
}

// reader is the state shared by queries and scans.
type reader[E, P any] struct {
	t             *Table[E, P]
	scan          bool
	find          findMode
	accessPattern string
	facets        []Facet
	sortKey       func(*engine.Query, func(types.AttributeValue) any)
	conds         []Cond
	opts          engine.QueryOptions
	pages         int
	count         int
	cursor        Cursor
	client        DynamoDB
	table         string
	mutateQ       []func(*dynamodb.QueryInput)
	mutateS       []func(*dynamodb.ScanInput)
	err           error
}

func engineValue(av types.AttributeValue) any {
	if av == nil {
		return engine.Undefined
	}
	return engine.FromAttributeValue(av)
}

func (r *reader[E, P]) build(exec bool) (*engine.QueryRequest, error) {
	if r.err != nil {
		return nil, r.err
	}
	m := r.t.def.model
	var q *engine.Query
	entries := make([]engine.Entry, len(r.facets))
	for i, f := range r.facets {
		entries[i] = engine.Entry{Path: f.Name, Value: engineValue(f.Value)}
	}
	switch {
	case r.find == findFind:
		q = engine.NewFind(m, entries)
	case r.find == findMatch:
		q = engine.NewMatch(m, entries)
	case r.scan:
		q = engine.NewScan(m, r.accessPattern)
	default:
		q = engine.NewQuery(m, r.accessPattern, entries)
		if r.sortKey != nil {
			r.sortKey(q, engineValue)
		}
	}
	for _, c := range r.conds {
		ec, err := c.engine()
		if err != nil {
			return nil, err
		}
		q.Where(ec)
	}
	opts := r.opts
	opts.Exec = exec
	if r.table != "" {
		opts.Table = r.table
	}
	req, err := q.Build(r.t.table, opts)
	if err != nil {
		return nil, err
	}
	if r.find != findNone {
		r.scan = req.Target == "Scan"
	}
	return req, nil
}

func (r *reader[E, P]) runner(ctx context.Context) *sdkRunner {
	client := r.client
	if client == nil {
		client = r.t.cfg.client
	}
	return &sdkRunner{ctx: ctx, client: client, scan: r.scan, mutateQ: r.mutateQ, mutateS: r.mutateS, consistent: r.opts.Consistent}
}

// sdkRunner performs a query's DynamoDB calls.
type sdkRunner struct {
	ctx        context.Context
	client     DynamoDB
	scan       bool
	mutateQ    []func(*dynamodb.QueryInput)
	mutateS    []func(*dynamodb.ScanInput)
	consistent bool
	queryOuts  []*dynamodb.QueryOutput
	scanOuts   []*dynamodb.ScanOutput
	keepRaw    bool
}

func (s *sdkRunner) Page(params map[string]any) ([]map[string]any, map[string]any, error) {
	p, err := paramsToSDK(params)
	if err != nil {
		return nil, nil, err
	}
	var rawItems []map[string]types.AttributeValue
	var lek map[string]types.AttributeValue
	if s.scan {
		in := p.scan()
		for _, fn := range s.mutateS {
			fn(in)
		}
		out, err := s.client.Scan(s.ctx, in)
		if err != nil {
			return nil, nil, wrapAWS(err)
		}
		if s.keepRaw {
			s.scanOuts = append(s.scanOuts, out)
		}
		rawItems, lek = out.Items, out.LastEvaluatedKey
	} else {
		in := p.query()
		for _, fn := range s.mutateQ {
			fn(in)
		}
		out, err := s.client.Query(s.ctx, in)
		if err != nil {
			return nil, nil, wrapAWS(err)
		}
		if s.keepRaw {
			s.queryOuts = append(s.queryOuts, out)
		}
		rawItems, lek = out.Items, out.LastEvaluatedKey
	}
	converted := make([]map[string]any, len(rawItems))
	for i, it := range rawItems {
		converted[i] = engine.FromAttributeMap(it)
	}
	var next map[string]any
	if len(lek) > 0 {
		next = engine.FromAttributeMap(lek)
	}
	return converted, next, nil
}

func (s *sdkRunner) BatchGet(table string, keys []map[string]any) ([]map[string]any, error) {
	var out []map[string]any
	for start := 0; start < len(keys); start += 100 {
		end := min(start+100, len(keys))
		ks := make([]map[string]types.AttributeValue, 0, end-start)
		for _, k := range keys[start:end] {
			av, err := engine.ToAttributeMap(k)
			if err != nil {
				return nil, err
			}
			ks = append(ks, av)
		}
		ka := types.KeysAndAttributes{Keys: ks}
		if s.consistent {
			ka.ConsistentRead = boolPtr(true)
		}
		res, err := s.client.BatchGetItem(s.ctx, &dynamodb.BatchGetItemInput{RequestItems: map[string]types.KeysAndAttributes{table: ka}})
		if err != nil {
			return nil, wrapAWS(err)
		}
		for _, it := range res.Responses[table] {
			out = append(out, engine.FromAttributeMap(it))
		}
	}
	return out, nil
}

func boolPtr(b bool) *bool { return &b }

// fetch runs the query as Exec does, decoding items with decode.
func fetch[E, P, T any](ctx context.Context, r *reader[E, P], decode func(map[string]any) (T, error)) ([]T, Cursor, error) {
	req, err := r.build(true)
	if err != nil {
		return nil, "", err
	}
	start, err := req.StartKey(string(r.cursor))
	if err != nil {
		return nil, "", err
	}
	res, err := req.Run(start, engine.RunOptions{Pages: r.pages, Count: r.count, IgnoreOwnership: r.opts.IgnoreOwnership, Attributes: r.opts.Attributes}, r.runner(ctx))
	if err != nil {
		return nil, "", err
	}
	return decodePage(res, decode)
}

func decodePage[T any](res engine.RunResult, decode func(map[string]any) (T, error)) ([]T, Cursor, error) {
	items := make([]T, 0, len(res.Items))
	for _, it := range res.Items {
		v, err := decode(it)
		if err != nil {
			return nil, "", err
		}
		items = append(items, v)
	}
	cur, err := engine.EncodeCursor(res.Next)
	if err != nil {
		return nil, "", err
	}
	if len(res.Unprocessed) > 0 {
		ue := &UnprocessedError{}
		for _, k := range res.Unprocessed {
			av, err := engine.ToAttributeMap(k)
			if err != nil {
				return nil, "", err
			}
			ue.Keys = append(ue.Keys, av)
		}
		return items, Cursor(cur), ue
	}
	return items, Cursor(cur), nil
}

// pagesOf iterates pages until DynamoDB has no more (decision D31).
func pagesOf[E, P, T any](ctx context.Context, r *reader[E, P], decode func(map[string]any) (T, error)) iter.Seq2[Page[T], error] {
	return func(yield func(Page[T], error) bool) {
		req, err := r.build(true)
		if err != nil {
			yield(Page[T]{}, err)
			return
		}
		start, err := req.StartKey(string(r.cursor))
		if err != nil {
			yield(Page[T]{}, err)
			return
		}
		run := r.runner(ctx)
		for {
			res, err := req.Run(start, engine.RunOptions{Pages: 1, IgnoreOwnership: r.opts.IgnoreOwnership, Attributes: r.opts.Attributes}, run)
			if err != nil {
				yield(Page[T]{}, err)
				return
			}
			items, cur, err := decodePage(res, decode)
			if !yield(Page[T]{Items: items, Cursor: cur}, err) {
				return
			}
			if len(res.Next) == 0 {
				return
			}
			start = res.Next
		}
	}
}

func itemsOf[T any](pages iter.Seq2[Page[T], error]) iter.Seq2[T, error] {
	return func(yield func(T, error) bool) {
		for page, err := range pages {
			if err != nil && !errors.Is(err, ErrUnprocessed) {
				var zero T
				yield(zero, err)
				return
			}
			for _, it := range page.Items {
				if !yield(it, nil) {
					return
				}
			}
			if err != nil {
				var zero T
				if !yield(zero, err) {
					return
				}
			}
		}
	}
}

func (t *Table[E, P]) decodeItem(m map[string]any) (E, error) {
	av, err := engine.ToAttributeMap(m)
	if err != nil {
		var zero E
		return zero, err
	}
	return t.decode(av)
}

func (t *Table[E, P]) decodePartialItem(m map[string]any) (P, error) {
	av, err := engine.ToAttributeMap(m)
	if err != nil {
		var zero P
		return zero, err
	}
	return t.decodePartial(av)
}

// decodeWithKeys splits an item formatted with its keys.
func (t *Table[E, P]) decodeWithKeys(m map[string]any) (WithKeys[E], error) {
	attrs, keys := map[string]any{}, map[string]any{}
	for k, v := range m {
		if _, ok := t.def.model.Attribute(k); ok {
			attrs[k] = v
		} else {
			keys[k] = v
		}
	}
	item, err := t.decodeItem(attrs)
	if err != nil {
		return WithKeys[E]{}, err
	}
	kav, err := engine.ToAttributeMap(keys)
	return WithKeys[E]{Item: item, Keys: kav}, err
}

// Query reads the items of one access pattern (decisions D29–D31).
// Generated clients start queries; their sort-key steps call KeyEq and the
// other Key methods.
type Query[E, P any] struct {
	r reader[E, P]
}

// Query starts a query; generated code calls it.
func (t *Table[E, P]) Query(accessPattern string, facets []Facet, err error) *Query[E, P] {
	return &Query[E, P]{r: reader[E, P]{t: t, accessPattern: accessPattern, facets: facets, pages: 1, err: err}}
}

// KeyEq fixes a sort-key composite; generated sort-key steps call it.
func (q *Query[E, P]) KeyEq(name string, v types.AttributeValue, err error) *Query[E, P] {
	q.fail(err)
	q.r.facets = append(q.r.facets, Facet{Name: name, Value: v})
	return q
}

// KeyCompare ends the key condition with gt, gte, lt or lte on a sort-key
// composite; generated code calls it.
func (q *Query[E, P]) KeyCompare(op, name string, v types.AttributeValue, err error) *Query[E, P] {
	q.fail(err)
	typ, ok := map[string]engine.QueryType{"gt": engine.QueryGt, "gte": engine.QueryGte, "lt": engine.QueryLt, "lte": engine.QueryLte}[op]
	if !ok {
		q.r.err = fmt.Errorf("wattdb: unknown comparison %q", op)
		return q
	}
	q.r.sortKey = func(eq *engine.Query, conv func(types.AttributeValue) any) {
		eq.Compare(typ, []engine.Entry{{Path: name, Value: conv(v)}})
	}
	return q
}

// KeyBetween ends the key condition with an inclusive range; generated code
// calls it.
func (q *Query[E, P]) KeyBetween(name string, lo, hi types.AttributeValue, err error) *Query[E, P] {
	q.fail(err)
	q.r.sortKey = func(eq *engine.Query, conv func(types.AttributeValue) any) {
		eq.Between([]engine.Entry{{Path: name, Value: conv(lo)}}, []engine.Entry{{Path: name, Value: conv(hi)}})
	}
	return q
}

// KeyBegins ends the key condition with begins_with; generated code calls
// it.
func (q *Query[E, P]) KeyBegins(name string, v types.AttributeValue, err error) *Query[E, P] {
	q.fail(err)
	q.r.sortKey = func(eq *engine.Query, conv func(types.AttributeValue) any) {
		eq.Begins([]engine.Entry{{Path: name, Value: conv(v)}})
	}
	return q
}

func (q *Query[E, P]) fail(err error) {
	if err != nil && q.r.err == nil {
		q.r.err = err
	}
}

// Where adds filters; they're ANDed.
func (q *Query[E, P]) Where(conds ...Cond) *Query[E, P] {
	q.r.conds = append(q.r.conds, conds...)
	return q
}

// Limit sets DynamoDB's per-request Limit.
func (q *Query[E, P]) Limit(n int) *Query[E, P] { q.r.opts.Limit = n; return q }

// Desc reads the index in descending sort-key order.
func (q *Query[E, P]) Desc() *Query[E, P] { q.r.opts.Order = "desc"; return q }

// Asc reads the index in ascending order (DynamoDB's default, sent
// explicitly).
func (q *Query[E, P]) Asc() *Query[E, P] { q.r.opts.Order = "asc"; return q }

// Consistent makes the reads strongly consistent.
func (q *Query[E, P]) Consistent() *Query[E, P] { q.r.opts.Consistent = true; return q }

// Cursor resumes after a previous page.
func (q *Query[E, P]) Cursor(c Cursor) *Query[E, P] { q.r.cursor = c; return q }

// MaxPages makes Exec read up to n pages (default 1).
func (q *Query[E, P]) MaxPages(n int) *Query[E, P] { q.r.pages = max(n, 1); return q }

// AllPages makes Exec read every page.
func (q *Query[E, P]) AllPages() *Query[E, P] { q.r.pages = 0; return q }

// Count makes Exec stop after n items, returning a cursor to continue.
func (q *Query[E, P]) Count(n int) *Query[E, P] { q.r.count = n; return q }

// Hydrate fetches the full items behind a keys-only or partial projection.
func (q *Query[E, P]) Hydrate() *Query[E, P] { q.r.opts.Hydrate = true; return q }

// IgnoreOwnership returns items without checking that this entity wrote
// them (by identifiers).
func (q *Query[E, P]) IgnoreOwnership() *Query[E, P] { q.r.opts.IgnoreOwnership = true; return q }

// Compare sets how sort-key comparisons treat composites (decision D51).
func (q *Query[E, P]) Compare(m CompareMode) *Query[E, P] { q.r.opts.Compare = string(m); return q }

// Table overrides the table for this call.
func (q *Query[E, P]) Table(name string) *Query[E, P] { q.r.table = name; return q }

// Client overrides the DynamoDB client for this call.
func (q *Query[E, P]) Client(c DynamoDB) *Query[E, P] { q.r.client = c; return q }

// Mutate edits each request before it's sent.
func (q *Query[E, P]) Mutate(fn func(*dynamodb.QueryInput)) *Query[E, P] {
	q.r.mutateQ = append(q.r.mutateQ, fn)
	return q
}

// Params returns the first request without sending it.
func (q *Query[E, P]) Params() (*dynamodb.QueryInput, error) {
	req, err := q.r.build(false)
	if err != nil {
		return nil, err
	}
	p, err := paramsToSDK(req.Params)
	if err != nil {
		return nil, err
	}
	in := p.query()
	for _, fn := range q.r.mutateQ {
		fn(in)
	}
	return in, nil
}

// Exec reads the items, honoring MaxPages/AllPages and Count, and returns a
// cursor to continue from ("" when done).
func (q *Query[E, P]) Exec(ctx context.Context) ([]E, Cursor, error) {
	return fetch(ctx, &q.r, q.r.t.decodeItem)
}

// Pages iterates pages until there are no more.
func (q *Query[E, P]) Pages(ctx context.Context) iter.Seq2[Page[E], error] {
	return pagesOf(ctx, &q.r, q.r.t.decodeItem)
}

// All iterates items until there are no more.
func (q *Query[E, P]) All(ctx context.Context) iter.Seq2[E, error] {
	return itemsOf(q.Pages(ctx))
}

// Attributes reads only the given attributes, returning Partials.
func (q *Query[E, P]) Attributes(ds ...Removable[E]) *PartialRead[E, P] {
	q.r.opts.Attributes = attributeFields(ds)
	return &PartialRead[E, P]{r: &q.r}
}

// IncludeKeys returns items with their stored key attributes.
func (q *Query[E, P]) IncludeKeys() *KeyedRead[E, P] {
	q.r.opts.IncludeKeys = true
	return &KeyedRead[E, P]{r: &q.r}
}

// Raw returns DynamoDB's responses as they are.
func (q *Query[E, P]) Raw() *RawQuery {
	r := &q.r
	return &RawQuery{exec: func(ctx context.Context) ([]*dynamodb.QueryOutput, error) {
		run := r.runner(ctx)
		run.keepRaw = true
		if err := rawRun(r, run); err != nil {
			return nil, err
		}
		return run.queryOuts, nil
	}}
}

func attributeFields[E any](ds []Removable[E]) []string {
	out := make([]string, len(ds))
	for i, d := range ds {
		out[i] = d.attrPath()[0]
	}
	return out
}

// rawRun reads pages without formatting, honoring MaxPages/AllPages.
func rawRun[E, P any](r *reader[E, P], run *sdkRunner) error {
	r.opts.Raw = true
	req, err := r.build(true)
	if err != nil {
		return err
	}
	start, err := req.StartKey(string(r.cursor))
	if err != nil {
		return err
	}
	for i := 0; r.pages == 0 || i < r.pages; i++ {
		_, next, err := run.Page(req.WithStartKey(start))
		if err != nil {
			return err
		}
		if len(next) == 0 {
			return nil
		}
		start = next
	}
	return nil
}

// RawQuery is a query returning DynamoDB's responses.
type RawQuery struct {
	exec func(context.Context) ([]*dynamodb.QueryOutput, error)
}

// Exec runs the query.
func (q *RawQuery) Exec(ctx context.Context) ([]*dynamodb.QueryOutput, error) { return q.exec(ctx) }

// PartialRead is a query or scan returning only some attributes.
type PartialRead[E, P any] struct {
	r *reader[E, P]
}

func (p *PartialRead[E, P]) Exec(ctx context.Context) ([]P, Cursor, error) {
	return fetch(ctx, p.r, p.r.t.decodePartialItem)
}

func (p *PartialRead[E, P]) Pages(ctx context.Context) iter.Seq2[Page[P], error] {
	return pagesOf(ctx, p.r, p.r.t.decodePartialItem)
}

func (p *PartialRead[E, P]) All(ctx context.Context) iter.Seq2[P, error] {
	return itemsOf(p.Pages(ctx))
}

// KeyedRead is a query or scan returning items with their keys.
type KeyedRead[E, P any] struct {
	r *reader[E, P]
}

func (k *KeyedRead[E, P]) Exec(ctx context.Context) ([]WithKeys[E], Cursor, error) {
	return fetch(ctx, k.r, k.r.t.decodeWithKeys)
}

func (k *KeyedRead[E, P]) Pages(ctx context.Context) iter.Seq2[Page[WithKeys[E]], error] {
	return pagesOf(ctx, k.r, k.r.t.decodeWithKeys)
}

func (k *KeyedRead[E, P]) All(ctx context.Context) iter.Seq2[WithKeys[E], error] {
	return itemsOf(k.Pages(ctx))
}

// Scan reads every item of the entity in the table, or in one index.
type Scan[E, P any] struct {
	r reader[E, P]
}

// Scan starts a scan of the table (accessPattern "") or of an index;
// generated code calls it.
func (t *Table[E, P]) Scan(accessPattern string) *Scan[E, P] {
	return &Scan[E, P]{r: reader[E, P]{t: t, scan: true, accessPattern: accessPattern, pages: 1}}
}

func (s *Scan[E, P]) Where(conds ...Cond) *Scan[E, P] {
	s.r.conds = append(s.r.conds, conds...)
	return s
}
func (s *Scan[E, P]) Limit(n int) *Scan[E, P]       { s.r.opts.Limit = n; return s }
func (s *Scan[E, P]) Consistent() *Scan[E, P]       { s.r.opts.Consistent = true; return s }
func (s *Scan[E, P]) Cursor(c Cursor) *Scan[E, P]   { s.r.cursor = c; return s }
func (s *Scan[E, P]) MaxPages(n int) *Scan[E, P]    { s.r.pages = max(n, 1); return s }
func (s *Scan[E, P]) AllPages() *Scan[E, P]         { s.r.pages = 0; return s }
func (s *Scan[E, P]) Count(n int) *Scan[E, P]       { s.r.count = n; return s }
func (s *Scan[E, P]) Hydrate() *Scan[E, P]          { s.r.opts.Hydrate = true; return s }
func (s *Scan[E, P]) IgnoreOwnership() *Scan[E, P]  { s.r.opts.IgnoreOwnership = true; return s }
func (s *Scan[E, P]) Table(name string) *Scan[E, P] { s.r.table = name; return s }
func (s *Scan[E, P]) Client(c DynamoDB) *Scan[E, P] { s.r.client = c; return s }

// Mutate edits each request before it's sent.
func (s *Scan[E, P]) Mutate(fn func(*dynamodb.ScanInput)) *Scan[E, P] {
	s.r.mutateS = append(s.r.mutateS, fn)
	return s
}

// Params returns the first request without sending it.
func (s *Scan[E, P]) Params() (*dynamodb.ScanInput, error) {
	req, err := s.r.build(false)
	if err != nil {
		return nil, err
	}
	p, err := paramsToSDK(req.Params)
	if err != nil {
		return nil, err
	}
	in := p.scan()
	for _, fn := range s.r.mutateS {
		fn(in)
	}
	return in, nil
}

func (s *Scan[E, P]) Exec(ctx context.Context) ([]E, Cursor, error) {
	return fetch(ctx, &s.r, s.r.t.decodeItem)
}

func (s *Scan[E, P]) Pages(ctx context.Context) iter.Seq2[Page[E], error] {
	return pagesOf(ctx, &s.r, s.r.t.decodeItem)
}

func (s *Scan[E, P]) All(ctx context.Context) iter.Seq2[E, error] { return itemsOf(s.Pages(ctx)) }

func (s *Scan[E, P]) Attributes(ds ...Removable[E]) *PartialRead[E, P] {
	s.r.opts.Attributes = attributeFields(ds)
	return &PartialRead[E, P]{r: &s.r}
}

func (s *Scan[E, P]) IncludeKeys() *KeyedRead[E, P] {
	s.r.opts.IncludeKeys = true
	return &KeyedRead[E, P]{r: &s.r}
}

// Raw returns DynamoDB's responses as they are.
func (s *Scan[E, P]) Raw() *RawScan {
	r := &s.r
	return &RawScan{exec: func(ctx context.Context) ([]*dynamodb.ScanOutput, error) {
		run := r.runner(ctx)
		run.keepRaw = true
		if err := rawRun(r, run); err != nil {
			return nil, err
		}
		return run.scanOuts, nil
	}}
}

// RawScan is a scan returning DynamoDB's responses.
type RawScan struct {
	exec func(context.Context) ([]*dynamodb.ScanOutput, error)
}

// Exec runs the scan.
func (s *RawScan) Exec(ctx context.Context) ([]*dynamodb.ScanOutput, error) { return s.exec(ctx) }
