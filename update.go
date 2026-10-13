package wattdb

import (
	"context"
	"fmt"
	"strconv"
	"strings"

	"github.com/aws/aws-sdk-go-v2/service/dynamodb"
	"github.com/aws/aws-sdk-go-v2/service/dynamodb/types"
	"github.com/outboundani/wattdb/internal/engine"
)

// Descriptor interfaces accepted by the update builders. Only generated
// descriptors implement them; E ties a descriptor to its entity, so a user
// descriptor can't be used in a task update.
type (
	// Settable is any descriptor of entity E holding a T.
	Settable[E, T any] interface {
		attrPath() engine.Path
		value(T) (any, error)
		entity(E)
	}
	// Removable is any descriptor of entity E.
	Removable[E any] interface {
		attrPath() engine.Path
		entity(E)
	}
	// Addable is a number or set descriptor (ADD).
	Addable[E, T any] interface {
		Settable[E, T]
		addable()
	}
	// Subtractable is a number descriptor.
	Subtractable[E, T any] interface {
		Settable[E, T]
		subtractable()
	}
	// Appendable is a list descriptor.
	Appendable[E, T any] interface {
		Settable[E, T]
		appendable()
	}
	// Deletable is a set descriptor (DELETE).
	Deletable[E, T any] interface {
		Settable[E, T]
		deletable()
	}
)

func (Attr[E, T]) entity(E)            {}
func (NumberAttr[E, T]) subtractable() {}
func (SetAttr[E, I]) deletable()       {}

// step is one recorded builder call, replayed onto the engine at Exec.
type step func(w *engine.Write) error

func valueStep[E, T any](op string, d Settable[E, T], v T) step {
	return func(w *engine.Write) error {
		x, err := d.value(v)
		if err != nil {
			return err
		}
		w.Apply(op, []engine.Entry{{Path: d.attrPath().String(), Value: x}})
		return nil
	}
}

// Update changes attributes of one item (update; patch when created through
// Patch). Operations apply in the order they're called.
type Update[E, P any] struct {
	t      *Table[E, P]
	key    map[string]types.AttributeValue
	patch  bool
	err    error
	steps  []step
	c      call
	mutate []func(*dynamodb.UpdateItemInput)
}

// Update starts an update; generated clients call it with an encoded Key.
func (t *Table[E, P]) Update(key map[string]types.AttributeValue, err error) *Update[E, P] {
	return &Update[E, P]{t: t, key: key, err: err}
}

// Patch starts a patch: an update that fails if the item doesn't exist.
func (t *Table[E, P]) Patch(key map[string]types.AttributeValue, err error) *Update[E, P] {
	return &Update[E, P]{t: t, key: key, err: err, patch: true}
}

func (u *Update[E, P]) add(s step) *Update[E, P] { u.steps = append(u.steps, s); return u }

// Set sets an attribute (SET).
func (u *Update[E, P]) Set[T any](d Settable[E, T], v T) *Update[E, P] {
	return u.add(valueStep("set", d, v))
}

// IfNotExists sets an attribute only if it has no value.
func (u *Update[E, P]) IfNotExists[T any](d Settable[E, T], v T) *Update[E, P] {
	return u.add(valueStep("ifNotExists", d, v))
}

// Add adds to a number or adds items to a set (ADD).
func (u *Update[E, P]) Add[T any](d Addable[E, T], v T) *Update[E, P] {
	return u.add(valueStep("add", d, v))
}

// Subtract subtracts from a number, treating a missing value as 0.
func (u *Update[E, P]) Subtract[T any](d Subtractable[E, T], v T) *Update[E, P] {
	return u.add(valueStep("subtract", d, v))
}

// Append appends items to a list, creating it if missing.
func (u *Update[E, P]) Append[T any](d Appendable[E, T], v T) *Update[E, P] {
	return u.add(valueStep("append", d, v))
}

// Delete removes items from a set (DELETE).
func (u *Update[E, P]) Delete[T any](d Deletable[E, T], v T) *Update[E, P] {
	return u.add(valueStep("delete", d, v))
}

// Remove removes attributes (REMOVE).
func (u *Update[E, P]) Remove(ds ...Removable[E]) *Update[E, P] {
	return u.add(func(w *engine.Write) error {
		paths := make([]string, len(ds))
		for i, d := range ds {
			paths[i] = d.attrPath().String()
		}
		w.Remove(paths)
		return nil
	})
}

// Composite supplies a key composite that isn't being set, so index keys
// that need it can be rebuilt; it's also added as an equality condition.
func (u *Update[E, P]) Composite[T any](d Settable[E, T], v T) *Update[E, P] {
	return u.add(func(w *engine.Write) error {
		x, err := d.value(v)
		if err != nil {
			return err
		}
		w.Composite([]engine.Entry{{Path: d.attrPath().String(), Value: x}})
		return nil
	})
}

// Raw adds a raw update clause, starting with its section keyword (SET,
// REMOVE, ADD or DELETE). Names and values are numbered as in Raw
// conditions (#n0, :v0) but with an "r" prefix (#rn0, :rv0) to keep them
// apart. Raw updates skip validation, as in ElectroDB.
func (u *Update[E, P]) Raw(clause string, args ...RawArg) *Update[E, P] {
	return u.add(rawStep(clause, args))
}

func rawStep(clause string, args []RawArg) step {
	return func(w *engine.Write) error {
		section, rest, ok := strings.Cut(strings.TrimSpace(clause), " ")
		if !ok {
			return fmt.Errorf("wattdb: raw update %q needs a section keyword", clause)
		}
		names, values := map[string]string{}, map[string]any{}
		n, v := 0, 0
		for _, a := range args {
			if a.name != nil {
				names["#rn"+strconv.Itoa(n)] = *a.name
				n++
				continue
			}
			av, err := encodeAny(a.value)
			if err != nil {
				return err
			}
			values[":rv"+strconv.Itoa(v)] = engine.FromAttributeValue(av)
			v++
		}
		w.RawUpdate(strings.ToLower(section), strings.TrimSpace(rest), names, values)
		return nil
	}
}

func (u *Update[E, P]) Where(conds ...Cond) *Update[E, P] { u.c.where(conds); return u }
func (u *Update[E, P]) Table(name string) *Update[E, P]   { u.c.table = name; return u }
func (u *Update[E, P]) Client(c DynamoDB) *Update[E, P]   { u.c.client = c; return u }

// Mutate edits the request before it's sent.
func (u *Update[E, P]) Mutate(fn func(*dynamodb.UpdateItemInput)) *Update[E, P] {
	u.mutate = append(u.mutate, fn)
	return u
}

func (u *Update[E, P]) request(exec bool) (*engine.Request, *dynamodb.UpdateItemInput, error) {
	if u.err != nil {
		return nil, nil, u.err
	}
	w := engine.NewUpdate(u.t.def.model, engine.FromAttributeMap(u.key), u.patch)
	for _, s := range u.steps {
		if err := s(w); err != nil {
			return nil, nil, err
		}
	}
	r, err := u.c.build(w, u.t.table, exec)
	if err != nil {
		return nil, nil, err
	}
	sp, err := toSDK(r)
	if err != nil {
		return nil, nil, err
	}
	in := sp.updateItem()
	for _, fn := range u.mutate {
		fn(in)
	}
	return r, in, nil
}

// Params returns the request without sending it.
func (u *Update[E, P]) Params() (*dynamodb.UpdateItemInput, error) {
	_, in, err := u.request(false)
	return in, err
}

// Exec runs the update.
func (u *Update[E, P]) Exec(ctx context.Context) error {
	_, err := u.exec(ctx)
	return err
}

func (u *Update[E, P]) exec(ctx context.Context) (engine.Response, error) {
	r, in, err := u.request(true)
	if err != nil {
		return engine.Response{}, err
	}
	out, err := u.c.clientFor(u.t.cfg, u.t.def).UpdateItem(ctx, in)
	if err != nil {
		return engine.Response{}, wrapAWS(err)
	}
	resp := map[string]any{}
	if out.Attributes != nil {
		resp["Attributes"] = engine.FromAttributeMap(out.Attributes)
	}
	return r.Format(resp)
}

// Returning makes Exec return the whole item, before (AllOld) or after
// (AllNew) the update.
func (u *Update[E, P]) Returning(m ReturnMode) *Returning[E] {
	u.c.opts.Response = string(m)
	return &Returning[E]{exec: func(ctx context.Context) (E, error) {
		r, err := u.exec(ctx)
		if err != nil {
			var zero E
			return zero, err
		}
		return u.t.decodeData(r)
	}}
}

// ReturningUpdated makes Exec return only the updated attributes, before
// (UpdatedOld) or after (UpdatedNew) the update.
func (u *Update[E, P]) ReturningUpdated(m PartialReturnMode) *ReturningPartial[P] {
	u.c.opts.Response = string(m)
	return &ReturningPartial[P]{exec: func(ctx context.Context) (P, error) {
		var zero P
		r, err := u.exec(ctx)
		if err != nil {
			return zero, err
		}
		if r.Data == nil {
			return zero, ErrNotFound
		}
		av, err := engine.ToAttributeMap(r.Data)
		if err != nil {
			return zero, err
		}
		return u.t.decodePartial(av)
	}}
}

// ReturnRejected makes Exec report a failed condition in the Outcome.
func (u *Update[E, P]) ReturnRejected(opts ...RejectOption) *Rejectable[E] {
	u.c.opts.ReturnRejected = "true"
	for _, o := range opts {
		o(&u.c.opts)
	}
	return &Rejectable[E]{exec: func(ctx context.Context) (Outcome[E], error) {
		r, in, err := u.request(true)
		if err != nil {
			return Outcome[E]{}, err
		}
		if _, err := u.c.clientFor(u.t.cfg, u.t.def).UpdateItem(ctx, in); err != nil {
			return rejected(u.t, r, err)
		}
		return Outcome[E]{}, nil
	}}
}

// Upsert writes an item's attributes, creating the item if needed, and
// applies further operations (decision D35).
type Upsert[E, P any] struct {
	t      *Table[E, P]
	item   map[string]types.AttributeValue
	order  []string
	err    error
	steps  []step
	c      call
	mutate []func(*dynamodb.UpdateItemInput)
}

// Upsert starts an upsert; generated clients call it with an encoded Input
// and its attribute order.
func (t *Table[E, P]) Upsert(item map[string]types.AttributeValue, order []string, err error) *Upsert[E, P] {
	return &Upsert[E, P]{t: t, item: item, order: order, err: err}
}

func (u *Upsert[E, P]) add(s step) *Upsert[E, P] { u.steps = append(u.steps, s); return u }

func (u *Upsert[E, P]) Set[T any](d Settable[E, T], v T) *Upsert[E, P] {
	return u.add(valueStep("set", d, v))
}
func (u *Upsert[E, P]) IfNotExists[T any](d Settable[E, T], v T) *Upsert[E, P] {
	return u.add(valueStep("ifNotExists", d, v))
}
func (u *Upsert[E, P]) Add[T any](d Addable[E, T], v T) *Upsert[E, P] {
	return u.add(valueStep("add", d, v))
}
func (u *Upsert[E, P]) Subtract[T any](d Subtractable[E, T], v T) *Upsert[E, P] {
	return u.add(valueStep("subtract", d, v))
}
func (u *Upsert[E, P]) Append[T any](d Appendable[E, T], v T) *Upsert[E, P] {
	return u.add(valueStep("append", d, v))
}

func (u *Upsert[E, P]) Where(conds ...Cond) *Upsert[E, P] { u.c.where(conds); return u }
func (u *Upsert[E, P]) Table(name string) *Upsert[E, P]   { u.c.table = name; return u }
func (u *Upsert[E, P]) Client(c DynamoDB) *Upsert[E, P]   { u.c.client = c; return u }

// Mutate edits the request before it's sent.
func (u *Upsert[E, P]) Mutate(fn func(*dynamodb.UpdateItemInput)) *Upsert[E, P] {
	u.mutate = append(u.mutate, fn)
	return u
}

func (u *Upsert[E, P]) request(exec bool) (*engine.Request, *dynamodb.UpdateItemInput, error) {
	if u.err != nil {
		return nil, nil, u.err
	}
	w := engine.NewUpsert(u.t.def.model, engine.FromAttributeMap(u.item), u.order)
	for _, s := range u.steps {
		if err := s(w); err != nil {
			return nil, nil, err
		}
	}
	r, err := u.c.build(w, u.t.table, exec)
	if err != nil {
		return nil, nil, err
	}
	sp, err := toSDK(r)
	if err != nil {
		return nil, nil, err
	}
	in := sp.updateItem()
	for _, fn := range u.mutate {
		fn(in)
	}
	return r, in, nil
}

// Params returns the request without sending it.
func (u *Upsert[E, P]) Params() (*dynamodb.UpdateItemInput, error) {
	_, in, err := u.request(false)
	return in, err
}

// Exec runs the upsert.
func (u *Upsert[E, P]) Exec(ctx context.Context) error {
	_, err := u.exec(ctx)
	return err
}

func (u *Upsert[E, P]) exec(ctx context.Context) (engine.Response, error) {
	r, in, err := u.request(true)
	if err != nil {
		return engine.Response{}, err
	}
	out, err := u.c.clientFor(u.t.cfg, u.t.def).UpdateItem(ctx, in)
	if err != nil {
		return engine.Response{}, wrapAWS(err)
	}
	resp := map[string]any{}
	if out.Attributes != nil {
		resp["Attributes"] = engine.FromAttributeMap(out.Attributes)
	}
	return r.Format(resp)
}

// Returning makes Exec return the whole item, before or after the upsert.
func (u *Upsert[E, P]) Returning(m ReturnMode) *Returning[E] {
	u.c.opts.Response = string(m)
	return &Returning[E]{exec: func(ctx context.Context) (E, error) {
		r, err := u.exec(ctx)
		if err != nil {
			var zero E
			return zero, err
		}
		return u.t.decodeData(r)
	}}
}

// ReturnRejected makes Exec report a failed condition in the Outcome.
func (u *Upsert[E, P]) ReturnRejected(opts ...RejectOption) *Rejectable[E] {
	u.c.opts.ReturnRejected = "true"
	for _, o := range opts {
		o(&u.c.opts)
	}
	return &Rejectable[E]{exec: func(ctx context.Context) (Outcome[E], error) {
		r, in, err := u.request(true)
		if err != nil {
			return Outcome[E]{}, err
		}
		if _, err := u.c.clientFor(u.t.cfg, u.t.def).UpdateItem(ctx, in); err != nil {
			return rejected(u.t, r, err)
		}
		return Outcome[E]{}, nil
	}}
}
