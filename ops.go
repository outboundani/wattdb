package wattdb

import (
	"context"
	"errors"

	"github.com/aws/aws-sdk-go-v2/service/dynamodb"
	"github.com/aws/aws-sdk-go-v2/service/dynamodb/types"
	"github.com/outboundani/wattdb/internal/engine"
)

// Table is the runtime half of a generated client: an entity bound to a
// Config and a table (decision D26). Generated packages wrap it; E is the
// read struct and P the Partial.
type Table[E, P any] struct {
	cfg           *Config
	def           *EntityDef
	table         string
	decode        func(map[string]types.AttributeValue) (E, error)
	decodePartial func(map[string]types.AttributeValue) (P, error)
}

// NewTable is used by generated code.
func NewTable[E, P any](cfg *Config, def *EntityDef, table string, decode func(map[string]types.AttributeValue) (E, error), decodePartial func(map[string]types.AttributeValue) (P, error)) (*Table[E, P], error) {
	if cfg == nil || cfg.client == nil {
		return nil, errNoClient
	}
	if table == "" {
		return nil, errors.New("wattdb: table name is empty")
	}
	return &Table[E, P]{cfg: cfg, def: def, table: table, decode: decode, decodePartial: decodePartial}, nil
}

func (t *Table[E, P]) decodeData(r engine.Response) (E, error) {
	var zero E
	if r.Data == nil {
		return zero, ErrNotFound
	}
	av, err := engine.ToAttributeMap(r.Data)
	if err != nil {
		return zero, err
	}
	return t.decode(av)
}

// ReturnMode chooses what Returning gives back (decision D33).
type ReturnMode string

const (
	// AllNew is the whole item after the write.
	AllNew ReturnMode = "all_new"
	// AllOld is the whole item before the write.
	AllOld ReturnMode = "all_old"
)

// PartialReturnMode chooses what ReturningUpdated gives back.
type PartialReturnMode string

const (
	// UpdatedNew is the updated attributes after the write.
	UpdatedNew PartialReturnMode = "updated_new"
	// UpdatedOld is the updated attributes before the write.
	UpdatedOld PartialReturnMode = "updated_old"
)

// Outcome is the result of a write made with ReturnRejected (decision D37).
type Outcome[T any] struct {
	// Rejected is true when the write's condition failed.
	Rejected bool
	// Item is the written item for puts; with WithOld, the existing item
	// when the write was rejected. Nil otherwise.
	Item *T
}

// RejectOption configures ReturnRejected.
type RejectOption func(*engine.Options)

// WithOld asks DynamoDB to return the existing item when the condition fails.
func WithOld() RejectOption {
	return func(o *engine.Options) { o.ReturnRejected = "all_old" }
}

// call holds the per-call state shared by every builder (decisions D40, D49).
type call struct {
	client DynamoDB
	table  string
	opts   engine.Options
	conds  []Cond
}

func (c *call) where(conds []Cond) { c.conds = append(c.conds, conds...) }

// build applies the conditions and renders the request.
func (c *call) build(w *engine.Write, defaultTable string, exec bool) (*engine.Request, error) {
	for _, cond := range c.conds {
		ec, err := cond.engine()
		if err != nil {
			return nil, err
		}
		w.Where(ec)
	}
	opts := c.opts
	opts.Exec = exec
	if c.table != "" {
		opts.Table = c.table
	}
	return w.Build(defaultTable, opts)
}

func (c *call) clientFor(cfg *Config, def *EntityDef) DynamoDB {
	return cfg.pick(c.client, def.model.Entity)
}

// Get reads one item by key (decisions D27, D28).
type Get[E, P any] struct {
	t      *Table[E, P]
	key    map[string]types.AttributeValue
	err    error
	c      call
	mutate []func(*dynamodb.GetItemInput)
}

// Get starts a get; generated clients call it with an encoded Key.
func (t *Table[E, P]) Get(key map[string]types.AttributeValue, err error) *Get[E, P] {
	return &Get[E, P]{t: t, key: key, err: err}
}

// Consistent makes the read strongly consistent.
func (g *Get[E, P]) Consistent() *Get[E, P] { g.c.opts.Consistent = true; return g }

// Table overrides the table for this call.
func (g *Get[E, P]) Table(name string) *Get[E, P] { g.c.table = name; return g }

// Client overrides the DynamoDB client for this call.
func (g *Get[E, P]) Client(c DynamoDB) *Get[E, P] { g.c.client = c; return g }

// Mutate edits the request before it's sent (decision D40).
func (g *Get[E, P]) Mutate(fn func(*dynamodb.GetItemInput)) *Get[E, P] {
	g.mutate = append(g.mutate, fn)
	return g
}

func (g *Get[E, P]) request(exec bool) (*engine.Request, *dynamodb.GetItemInput, error) {
	if g.err != nil {
		return nil, nil, g.err
	}
	r, err := g.c.build(engine.NewGet(g.t.def.model, engine.FromAttributeMap(g.key)), g.t.table, exec)
	if err != nil {
		return nil, nil, err
	}
	p, err := toSDK(r)
	if err != nil {
		return nil, nil, err
	}
	in := p.get()
	for _, fn := range g.mutate {
		fn(in)
	}
	return r, in, nil
}

// Params returns the request without sending it (decision D39).
func (g *Get[E, P]) Params() (*dynamodb.GetItemInput, error) {
	_, in, err := g.request(false)
	return in, err
}

// Exec reads the item; a missing item is ErrNotFound.
func (g *Get[E, P]) Exec(ctx context.Context) (E, error) {
	var zero E
	r, in, err := g.request(true)
	if err != nil {
		return zero, err
	}
	out, err := g.c.clientFor(g.t.cfg, g.t.def).GetItem(ctx, in)
	if err != nil {
		return zero, wrapAWS(err)
	}
	resp := map[string]any{}
	if out.Item != nil {
		resp["Item"] = engine.FromAttributeMap(out.Item)
	}
	res, err := r.Format(resp)
	if err != nil {
		return zero, err
	}
	return g.t.decodeData(res)
}

// Put writes a whole item (create when created through Create).
type Put[E, P any] struct {
	t      *Table[E, P]
	item   map[string]types.AttributeValue
	create bool
	err    error
	c      call
	mutate []func(*dynamodb.PutItemInput)
}

// Put starts a put; generated clients call it with an encoded Input.
func (t *Table[E, P]) Put(item map[string]types.AttributeValue, err error) *Put[E, P] {
	return &Put[E, P]{t: t, item: item, err: err}
}

// Create starts a create: a put that fails if the item exists.
func (t *Table[E, P]) Create(item map[string]types.AttributeValue, err error) *Put[E, P] {
	return &Put[E, P]{t: t, item: item, err: err, create: true}
}

// Where adds conditions; they're ANDed with any others.
func (p *Put[E, P]) Where(conds ...Cond) *Put[E, P] { p.c.where(conds); return p }

// Table overrides the table for this call.
func (p *Put[E, P]) Table(name string) *Put[E, P] { p.c.table = name; return p }

// Client overrides the DynamoDB client for this call.
func (p *Put[E, P]) Client(c DynamoDB) *Put[E, P] { p.c.client = c; return p }

// Mutate edits the request before it's sent.
func (p *Put[E, P]) Mutate(fn func(*dynamodb.PutItemInput)) *Put[E, P] {
	p.mutate = append(p.mutate, fn)
	return p
}

// Returning makes Exec return the item before the write (AllOld) instead of
// the written item. DynamoDB doesn't support AllNew for puts.
func (p *Put[E, P]) Returning(m ReturnMode) *Put[E, P] { p.c.opts.Response = string(m); return p }

// ReturnRejected makes Exec report a failed condition in the Outcome
// instead of as ErrConditionFailed.
func (p *Put[E, P]) ReturnRejected(opts ...RejectOption) *Rejectable[E] {
	p.c.opts.ReturnRejected = "true"
	for _, o := range opts {
		o(&p.c.opts)
	}
	return &Rejectable[E]{exec: p.exec}
}

func (p *Put[E, P]) request(exec bool) (*engine.Request, *dynamodb.PutItemInput, error) {
	if p.err != nil {
		return nil, nil, p.err
	}
	r, err := p.c.build(engine.NewPut(p.t.def.model, engine.FromAttributeMap(p.item), p.create), p.t.table, exec)
	if err != nil {
		return nil, nil, err
	}
	sp, err := toSDK(r)
	if err != nil {
		return nil, nil, err
	}
	in := sp.put()
	for _, fn := range p.mutate {
		fn(in)
	}
	return r, in, nil
}

// Params returns the request without sending it.
func (p *Put[E, P]) Params() (*dynamodb.PutItemInput, error) {
	_, in, err := p.request(false)
	return in, err
}

// Exec writes the item and returns it as written (or the old item, with
// Returning(AllOld); ErrNotFound if there was none).
func (p *Put[E, P]) Exec(ctx context.Context) (E, error) {
	o, err := p.exec(ctx)
	if err != nil {
		var zero E
		return zero, err
	}
	return *o.Item, nil
}

func (p *Put[E, P]) exec(ctx context.Context) (Outcome[E], error) {
	r, in, err := p.request(true)
	if err != nil {
		return Outcome[E]{}, err
	}
	out, err := p.c.clientFor(p.t.cfg, p.t.def).PutItem(ctx, in)
	if err != nil {
		return rejected(p.t, r, err)
	}
	resp := map[string]any{}
	if out.Attributes != nil {
		resp["Attributes"] = engine.FromAttributeMap(out.Attributes)
	}
	res, err := r.Format(resp)
	if err != nil {
		return Outcome[E]{}, err
	}
	item, err := p.t.decodeData(res)
	if err != nil {
		return Outcome[E]{}, err
	}
	return Outcome[E]{Item: &item}, nil
}

// rejected turns a condition failure into an Outcome when ReturnRejected
// was requested, and otherwise wraps the error.
func rejected[E, P any](t *Table[E, P], r *engine.Request, err error) (Outcome[E], error) {
	failed, old := conditionFailure(err)
	if !failed || r.Params == nil || !rejectRequested(r) {
		return Outcome[E]{}, wrapAWS(err)
	}
	var oldItem map[string]any
	if old != nil {
		oldItem = engine.FromAttributeMap(old)
	}
	res, ferr := r.FormatRejected(oldItem)
	if ferr != nil {
		return Outcome[E]{}, ferr
	}
	o := Outcome[E]{Rejected: true}
	if res.Data != nil {
		item, derr := t.decodeData(res)
		if derr != nil {
			return Outcome[E]{}, derr
		}
		o.Item = &item
	}
	return o, nil
}

func rejectRequested(r *engine.Request) bool { return r.RejectRequested() }

// Rejectable is a write whose Exec reports a failed condition in its
// Outcome (decision D37).
type Rejectable[E any] struct {
	exec func(context.Context) (Outcome[E], error)
}

// Exec runs the write.
func (r *Rejectable[E]) Exec(ctx context.Context) (Outcome[E], error) { return r.exec(ctx) }

// Delete deletes one item by key (remove when created through Remove).
type Delete[E, P any] struct {
	t      *Table[E, P]
	key    map[string]types.AttributeValue
	remove bool
	err    error
	c      call
	mutate []func(*dynamodb.DeleteItemInput)
}

// Delete starts a delete.
func (t *Table[E, P]) Delete(key map[string]types.AttributeValue, err error) *Delete[E, P] {
	return &Delete[E, P]{t: t, key: key, err: err}
}

// Remove starts a remove: a delete that fails if the item doesn't exist.
func (t *Table[E, P]) Remove(key map[string]types.AttributeValue, err error) *Delete[E, P] {
	return &Delete[E, P]{t: t, key: key, err: err, remove: true}
}

func (d *Delete[E, P]) Where(conds ...Cond) *Delete[E, P] { d.c.where(conds); return d }
func (d *Delete[E, P]) Table(name string) *Delete[E, P]   { d.c.table = name; return d }
func (d *Delete[E, P]) Client(c DynamoDB) *Delete[E, P]   { d.c.client = c; return d }

// Mutate edits the request before it's sent.
func (d *Delete[E, P]) Mutate(fn func(*dynamodb.DeleteItemInput)) *Delete[E, P] {
	d.mutate = append(d.mutate, fn)
	return d
}

// Returning makes Exec return the deleted item (AllOld).
func (d *Delete[E, P]) Returning(m ReturnMode) *Returning[E] {
	d.c.opts.Response = string(m)
	return &Returning[E]{exec: func(ctx context.Context) (E, error) {
		r, err := d.exec(ctx)
		if err != nil {
			var zero E
			return zero, err
		}
		return d.t.decodeData(r)
	}}
}

// ReturnRejected makes Exec report a failed condition in the Outcome.
func (d *Delete[E, P]) ReturnRejected(opts ...RejectOption) *Rejectable[E] {
	d.c.opts.ReturnRejected = "true"
	for _, o := range opts {
		o(&d.c.opts)
	}
	return &Rejectable[E]{exec: func(ctx context.Context) (Outcome[E], error) {
		r, in, err := d.request(true)
		if err != nil {
			return Outcome[E]{}, err
		}
		if _, err := d.c.clientFor(d.t.cfg, d.t.def).DeleteItem(ctx, in); err != nil {
			return rejected(d.t, r, err)
		}
		return Outcome[E]{}, nil
	}}
}

func (d *Delete[E, P]) request(exec bool) (*engine.Request, *dynamodb.DeleteItemInput, error) {
	if d.err != nil {
		return nil, nil, d.err
	}
	r, err := d.c.build(engine.NewDelete(d.t.def.model, engine.FromAttributeMap(d.key), d.remove), d.t.table, exec)
	if err != nil {
		return nil, nil, err
	}
	sp, err := toSDK(r)
	if err != nil {
		return nil, nil, err
	}
	in := sp.delete()
	for _, fn := range d.mutate {
		fn(in)
	}
	return r, in, nil
}

// Params returns the request without sending it.
func (d *Delete[E, P]) Params() (*dynamodb.DeleteItemInput, error) {
	_, in, err := d.request(false)
	return in, err
}

// Exec deletes the item.
func (d *Delete[E, P]) Exec(ctx context.Context) error {
	_, err := d.exec(ctx)
	return err
}

func (d *Delete[E, P]) exec(ctx context.Context) (engine.Response, error) {
	r, in, err := d.request(true)
	if err != nil {
		return engine.Response{}, err
	}
	out, err := d.c.clientFor(d.t.cfg, d.t.def).DeleteItem(ctx, in)
	if err != nil {
		return engine.Response{}, wrapAWS(err)
	}
	resp := map[string]any{}
	if out.Attributes != nil {
		resp["Attributes"] = engine.FromAttributeMap(out.Attributes)
	}
	return r.Format(resp)
}

// Returning is a write whose Exec returns an item (decision D33).
type Returning[E any] struct {
	exec func(context.Context) (E, error)
}

// Exec runs the write; ErrNotFound means DynamoDB returned no item.
func (r *Returning[E]) Exec(ctx context.Context) (E, error) { return r.exec(ctx) }

// ReturningPartial is a write whose Exec returns the updated attributes.
type ReturningPartial[P any] struct {
	exec func(context.Context) (P, error)
}

// Exec runs the write; ErrNotFound means DynamoDB returned nothing.
func (r *ReturningPartial[P]) Exec(ctx context.Context) (P, error) { return r.exec(ctx) }
