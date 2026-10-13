package wattdb

import (
	"context"
	"iter"

	"github.com/aws/aws-sdk-go-v2/service/dynamodb"
	"github.com/aws/aws-sdk-go-v2/service/dynamodb/types"
	"github.com/outboundani/wattdb/internal/engine"
)

// Find reads the items matching some attributes, through the best-fitting
// access pattern or a scan when none fits (decision D44).
type Find[E, P any] struct {
	r reader[E, P]
}

// Find starts a find; generated code calls it with the Filter's set fields.
func (t *Table[E, P]) Find(facets []Facet, err error) *Find[E, P] {
	return &Find[E, P]{r: reader[E, P]{t: t, find: findFind, facets: facets, pages: 1, err: err}}
}

// Match starts a match: a find that also filters on every given attribute.
func (t *Table[E, P]) Match(facets []Facet, err error) *Find[E, P] {
	return &Find[E, P]{r: reader[E, P]{t: t, find: findMatch, facets: facets, pages: 1, err: err}}
}

type findMode int

const (
	findNone findMode = iota
	findFind
	findMatch
)

// Where adds filters; they're ANDed.
func (f *Find[E, P]) Where(conds ...Cond) *Find[E, P] {
	f.r.conds = append(f.r.conds, conds...)
	return f
}

// Limit sets DynamoDB's per-request Limit.
func (f *Find[E, P]) Limit(n int) *Find[E, P] { f.r.opts.Limit = n; return f }

// Consistent makes the reads strongly consistent.
func (f *Find[E, P]) Consistent() *Find[E, P] { f.r.opts.Consistent = true; return f }

// Cursor resumes from a previous cursor.
func (f *Find[E, P]) Cursor(c Cursor) *Find[E, P] { f.r.cursor = c; return f }

// MaxPages reads up to n pages (default 1).
func (f *Find[E, P]) MaxPages(n int) *Find[E, P] { f.r.pages = max(n, 1); return f }

// AllPages reads until DynamoDB has no more.
func (f *Find[E, P]) AllPages() *Find[E, P] { f.r.pages = 0; return f }

// Count stops after n items.
func (f *Find[E, P]) Count(n int) *Find[E, P] { f.r.count = n; return f }

// IgnoreOwnership skips the entity identifier checks.
func (f *Find[E, P]) IgnoreOwnership() *Find[E, P] { f.r.opts.IgnoreOwnership = true; return f }

// Table overrides the table for this call.
func (f *Find[E, P]) Table(name string) *Find[E, P] { f.r.table = name; return f }

// Client overrides the DynamoDB client for this call.
func (f *Find[E, P]) Client(c DynamoDB) *Find[E, P] { f.r.client = c; return f }

// MutateQuery edits each request when the find runs as a query.
func (f *Find[E, P]) MutateQuery(fn func(*dynamodb.QueryInput)) *Find[E, P] {
	f.r.mutateQ = append(f.r.mutateQ, fn)
	return f
}

// MutateScan edits each request when the find runs as a scan.
func (f *Find[E, P]) MutateScan(fn func(*dynamodb.ScanInput)) *Find[E, P] {
	f.r.mutateS = append(f.r.mutateS, fn)
	return f
}

// Params returns the first request without sending it: a query, or a scan
// when no access pattern fits (the other is nil).
func (f *Find[E, P]) Params() (*dynamodb.QueryInput, *dynamodb.ScanInput, error) {
	req, err := f.r.build(false)
	if err != nil {
		return nil, nil, err
	}
	p, err := paramsToSDK(req.Params)
	if err != nil {
		return nil, nil, err
	}
	if f.r.scan {
		in := p.scan()
		for _, fn := range f.r.mutateS {
			fn(in)
		}
		return nil, in, nil
	}
	in := p.query()
	for _, fn := range f.r.mutateQ {
		fn(in)
	}
	return in, nil, nil
}

// Exec runs the find, reading MaxPages pages (1 by default).
func (f *Find[E, P]) Exec(ctx context.Context) ([]E, Cursor, error) {
	return fetch(ctx, &f.r, f.r.t.decodeItem)
}

// Pages iterates page by page until DynamoDB has no more.
func (f *Find[E, P]) Pages(ctx context.Context) iter.Seq2[Page[E], error] {
	return pagesOf(ctx, &f.r, f.r.t.decodeItem)
}

// All iterates item by item until DynamoDB has no more.
func (f *Find[E, P]) All(ctx context.Context) iter.Seq2[E, error] { return itemsOf(f.Pages(ctx)) }

// Parse formats a raw item read some other way (a stream record, a raw
// query), ignoring ownership unless the keys don't fit the entity (decision
// D46). ErrNotFound means the item isn't the entity's.
func (t *Table[E, P]) Parse(item map[string]types.AttributeValue) (E, error) {
	var zero E
	out, err := engine.Parse(t.def.model, map[string]any{"Item": engine.FromAttributeMap(item)}, engine.FormatOptions{}, true)
	if err != nil {
		return zero, err
	}
	data, _ := out.(map[string]any)
	if data == nil {
		return zero, ErrNotFound
	}
	return t.decodeItem(data)
}
