package wattdb

import (
	"context"
	"fmt"
	"iter"

	"github.com/aws/aws-sdk-go-v2/service/dynamodb"
	"github.com/aws/aws-sdk-go-v2/service/dynamodb/types"
	"github.com/outboundani/wattdb/internal/engine"
)

// CollectionMember is one entity of a collection, as generated code
// describes it.
type CollectionMember struct {
	// Alias is the entity's name in the service.
	Alias string
	Def   *EntityDef
}

// Collection queries several entities sharing an index at once (decision
// D43). R is the generated result struct, with one slice per member.
type Collection[R any] struct {
	cfg     *Config
	table   string
	name    string
	members []engine.CollectionMember
	// add decodes a member's item and appends it to the result.
	add func(r *R, member int, item map[string]types.AttributeValue) error
}

// NewCollection is used by generated code.
func NewCollection[R any](cfg *Config, table, name string, members []CollectionMember, add func(r *R, member int, item map[string]types.AttributeValue) error) (*Collection[R], error) {
	if cfg == nil || cfg.client == nil {
		return nil, errNoClient
	}
	if table == "" {
		return nil, fmt.Errorf("wattdb: table name is empty")
	}
	c := &Collection[R]{cfg: cfg, table: table, name: name, add: add}
	for _, m := range members {
		c.members = append(c.members, engine.CollectionMember{Alias: m.Alias, Model: m.Def.model})
	}
	return c, nil
}

// CollectionPage is one page of a collection query and the cursor after
// it.
type CollectionPage[R any] struct {
	Result R
	Cursor Cursor
}

// CollectionQuery is a collection query being built.
type CollectionQuery[R any] struct {
	c       *Collection[R]
	facets  []Facet
	sortKey func(*engine.Query, func(types.AttributeValue) any)
	conds   []Cond
	opts    engine.QueryOptions
	pages   int
	cursor  Cursor
	client  DynamoDB
	table   string
	mutate  []func(*dynamodb.QueryInput)
	err     error
}

// Query starts a query; generated code calls it with the key composites.
func (c *Collection[R]) Query(facets []Facet, err error) *CollectionQuery[R] {
	return &CollectionQuery[R]{c: c, facets: facets, pages: 1, err: err}
}

func (q *CollectionQuery[R]) fail(err error) {
	if err != nil && q.err == nil {
		q.err = err
	}
}

// KeyEq fixes a sort-key composite; generated sort-key steps call it.
func (q *CollectionQuery[R]) KeyEq(name string, v types.AttributeValue, err error) *CollectionQuery[R] {
	q.fail(err)
	q.facets = append(q.facets, Facet{Name: name, Value: v})
	return q
}

// KeyCompare ends the key condition with gt, gte, lt or lte.
func (q *CollectionQuery[R]) KeyCompare(op, name string, v types.AttributeValue, err error) *CollectionQuery[R] {
	q.fail(err)
	typ, ok := map[string]engine.QueryType{"gt": engine.QueryGt, "gte": engine.QueryGte, "lt": engine.QueryLt, "lte": engine.QueryLte}[op]
	if !ok {
		q.fail(fmt.Errorf("wattdb: unknown comparison %q", op))
		return q
	}
	q.sortKey = func(eq *engine.Query, conv func(types.AttributeValue) any) {
		eq.Compare(typ, []engine.Entry{{Path: name, Value: conv(v)}})
	}
	return q
}

// KeyBetween ends the key condition with an inclusive range.
func (q *CollectionQuery[R]) KeyBetween(name string, lo, hi types.AttributeValue, err error) *CollectionQuery[R] {
	q.fail(err)
	q.sortKey = func(eq *engine.Query, conv func(types.AttributeValue) any) {
		eq.Between([]engine.Entry{{Path: name, Value: conv(lo)}}, []engine.Entry{{Path: name, Value: conv(hi)}})
	}
	return q
}

// KeyBegins ends the key condition with begins_with.
func (q *CollectionQuery[R]) KeyBegins(name string, v types.AttributeValue, err error) *CollectionQuery[R] {
	q.fail(err)
	q.sortKey = func(eq *engine.Query, conv func(types.AttributeValue) any) {
		eq.Begins([]engine.Entry{{Path: name, Value: conv(v)}})
	}
	return q
}

// Where adds filters; they're ANDed. Attributes of any member can be used.
func (q *CollectionQuery[R]) Where(conds ...Cond) *CollectionQuery[R] {
	q.conds = append(q.conds, conds...)
	return q
}

// Limit sets DynamoDB's per-request Limit.
func (q *CollectionQuery[R]) Limit(n int) *CollectionQuery[R] { q.opts.Limit = n; return q }

// Desc reads in descending sort-key order.
func (q *CollectionQuery[R]) Desc() *CollectionQuery[R] { q.opts.Order = "desc"; return q }

// Asc sends ScanIndexForward=true explicitly.
func (q *CollectionQuery[R]) Asc() *CollectionQuery[R] { q.opts.Order = "asc"; return q }

// Consistent makes the reads strongly consistent.
func (q *CollectionQuery[R]) Consistent() *CollectionQuery[R] { q.opts.Consistent = true; return q }

// Cursor resumes from a previous cursor.
func (q *CollectionQuery[R]) Cursor(c Cursor) *CollectionQuery[R] { q.cursor = c; return q }

// MaxPages reads up to n pages (default 1).
func (q *CollectionQuery[R]) MaxPages(n int) *CollectionQuery[R] { q.pages = max(n, 1); return q }

// AllPages reads until DynamoDB has no more.
func (q *CollectionQuery[R]) AllPages() *CollectionQuery[R] { q.pages = 0; return q }

// Hydrate fetches whole items for indexes that project only keys.
func (q *CollectionQuery[R]) Hydrate() *CollectionQuery[R] { q.opts.Hydrate = true; return q }

// IgnoreOwnership skips the entity identifier filter and checks.
func (q *CollectionQuery[R]) IgnoreOwnership() *CollectionQuery[R] {
	q.opts.IgnoreOwnership = true
	return q
}

// Compare sets how sort-key comparisons treat composites.
func (q *CollectionQuery[R]) Compare(m CompareMode) *CollectionQuery[R] {
	q.opts.Compare = string(m)
	return q
}

// Table overrides the table for this call.
func (q *CollectionQuery[R]) Table(name string) *CollectionQuery[R] { q.table = name; return q }

// Client overrides the DynamoDB client for this call.
func (q *CollectionQuery[R]) Client(c DynamoDB) *CollectionQuery[R] { q.client = c; return q }

// Mutate edits each request before it's sent.
func (q *CollectionQuery[R]) Mutate(fn func(*dynamodb.QueryInput)) *CollectionQuery[R] {
	q.mutate = append(q.mutate, fn)
	return q
}

func (q *CollectionQuery[R]) build(exec bool) (*engine.QueryRequest, error) {
	if q.err != nil {
		return nil, q.err
	}
	entries := make([]engine.Entry, len(q.facets))
	for i, f := range q.facets {
		entries[i] = engine.Entry{Path: f.Name, Value: engineValue(f.Value)}
	}
	eq := engine.NewCollection(q.c.name, q.c.members, entries)
	if q.sortKey != nil {
		q.sortKey(eq, engineValue)
	}
	for _, c := range q.conds {
		ec, err := c.engine()
		if err != nil {
			return nil, err
		}
		eq.Where(ec)
	}
	opts := q.opts
	opts.Exec = exec
	if q.table != "" {
		opts.Table = q.table
	}
	return eq.Build(q.c.table, opts)
}

func (q *CollectionQuery[R]) runner(ctx context.Context) *sdkRunner {
	client := q.c.cfg.pick(q.client, "")
	return &sdkRunner{ctx: ctx, client: client, mutateQ: q.mutate, consistent: q.opts.Consistent}
}

// Params returns the first request without sending it.
func (q *CollectionQuery[R]) Params() (*dynamodb.QueryInput, error) {
	r, err := q.build(false)
	if err != nil {
		return nil, err
	}
	p, err := paramsToSDK(r.Params)
	if err != nil {
		return nil, err
	}
	in := p.query()
	for _, fn := range q.mutate {
		fn(in)
	}
	return in, nil
}

func (q *CollectionQuery[R]) result(res engine.RunResult) (R, Cursor, error) {
	var out R
	for i, group := range res.Groups {
		for _, it := range group {
			if it == nil {
				continue
			}
			av, err := engine.ToAttributeMap(it)
			if err != nil {
				return out, "", err
			}
			if err := q.c.add(&out, i, av); err != nil {
				return out, "", err
			}
		}
	}
	cur, err := engine.EncodeCursor(res.Next)
	if err != nil {
		return out, "", err
	}
	if len(res.Unprocessed) > 0 {
		ue := &UnprocessedError{}
		for _, k := range res.Unprocessed {
			av, err := engine.ToAttributeMap(k)
			if err != nil {
				return out, "", err
			}
			ue.Keys = append(ue.Keys, av)
		}
		return out, Cursor(cur), ue
	}
	return out, Cursor(cur), nil
}

// Exec runs the query, reading MaxPages pages (1 by default), and returns
// the items grouped by entity with the cursor to continue from.
func (q *CollectionQuery[R]) Exec(ctx context.Context) (R, Cursor, error) {
	var zero R
	req, err := q.build(true)
	if err != nil {
		return zero, "", err
	}
	start, err := req.StartKey(string(q.cursor))
	if err != nil {
		return zero, "", err
	}
	res, err := req.Run(start, engine.RunOptions{Pages: q.pages, IgnoreOwnership: q.opts.IgnoreOwnership, Attributes: q.opts.Attributes}, q.runner(ctx))
	if err != nil {
		return zero, "", err
	}
	return q.result(res)
}

// Pages iterates page by page until DynamoDB has no more.
func (q *CollectionQuery[R]) Pages(ctx context.Context) iter.Seq2[CollectionPage[R], error] {
	return func(yield func(CollectionPage[R], error) bool) {
		req, err := q.build(true)
		if err != nil {
			yield(CollectionPage[R]{}, err)
			return
		}
		start, err := req.StartKey(string(q.cursor))
		if err != nil {
			yield(CollectionPage[R]{}, err)
			return
		}
		run := q.runner(ctx)
		for {
			res, err := req.Run(start, engine.RunOptions{Pages: 1, IgnoreOwnership: q.opts.IgnoreOwnership, Attributes: q.opts.Attributes}, run)
			if err != nil {
				yield(CollectionPage[R]{}, err)
				return
			}
			r, cur, err := q.result(res)
			if !yield(CollectionPage[R]{Result: r, Cursor: cur}, err) {
				return
			}
			if len(res.Next) == 0 {
				return
			}
			start = res.Next
		}
	}
}
