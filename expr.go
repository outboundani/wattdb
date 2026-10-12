package wattdb

import (
	"fmt"
	"strconv"

	"github.com/aws/aws-sdk-go-v2/service/dynamodb/types"
	"github.com/outboundani/wattdb/internal/engine"
)

// Cond is a condition for Where (decision D36). Build them from descriptors
// (user.Email.Eq("a@b.co")) and combine them with And, Or and Not.
type Cond struct {
	build func() (engine.Cond, error)
}

func errCond(err error) Cond {
	return Cond{build: func() (engine.Cond, error) { return nil, err }}
}

// And is true when every condition is, rendered "(a) AND (b)".
func And(conds ...Cond) Cond {
	return Cond{build: func() (engine.Cond, error) {
		out := make(engine.CondAnd, 0, len(conds))
		for _, c := range conds {
			e, err := c.engine()
			if err != nil {
				return nil, err
			}
			out = append(out, e)
		}
		return out, nil
	}}
}

// Or is true when any condition is, rendered "(a) OR (b)".
func Or(conds ...Cond) Cond {
	return Cond{build: func() (engine.Cond, error) {
		out := make(engine.CondOr, 0, len(conds))
		for _, c := range conds {
			e, err := c.engine()
			if err != nil {
				return nil, err
			}
			out = append(out, e)
		}
		return out, nil
	}}
}

// Not negates a condition, rendered "NOT (a)".
func Not(c Cond) Cond {
	return Cond{build: func() (engine.Cond, error) {
		e, err := c.engine()
		if err != nil {
			return nil, err
		}
		return engine.CondNot{C: e}, nil
	}}
}

func (c Cond) engine() (engine.Cond, error) {
	if c.build == nil {
		return nil, fmt.Errorf("wattdb: empty condition")
	}
	return c.build()
}

// RawArg is a name or value referenced by a Raw expression.
type RawArg struct {
	name  *string
	value any
}

// Name is a field name for Raw, referenced as #name in the expression.
func Name(field string) RawArg { return RawArg{name: &field} }

// Value is a value for Raw, referenced as :name in the expression; v is
// converted with attributevalue.
func Value(v any) RawArg { return RawArg{value: v} }

// Raw is a condition written as a DynamoDB expression. Names and values are
// numbered in order: the first Name is #n0, the first Value :v0.
//
//	wattdb.Raw("size(#n0) > :v0", wattdb.Name("tags"), wattdb.Value(3))
func Raw(expr string, args ...RawArg) Cond {
	return Cond{build: func() (engine.Cond, error) {
		raw := engine.CondRaw{Expression: expr, Names: map[string]string{}, Values: map[string]any{}}
		n, v := 0, 0
		for _, a := range args {
			if a.name != nil {
				raw.Names["#n"+strconv.Itoa(n)] = *a.name
				n++
				continue
			}
			av, err := encodeAny(a.value)
			if err != nil {
				return nil, err
			}
			raw.Values[":v"+strconv.Itoa(v)] = engine.FromAttributeValue(av)
			v++
		}
		return raw, nil
	}}
}

// pathed is implemented by every descriptor.
type pathed interface {
	attrPath() engine.Path
}

// Attr describes an attribute of entity E holding a T, for conditions and
// updates (decisions D34, D36). Generated packages declare one per
// attribute (user.Email); applications don't construct them.
type Attr[E, T any] struct {
	path []string
	enc  func(T) (types.AttributeValue, error)
}

// NewAttr is used by generated code.
func NewAttr[E, T any](path []string, enc func(T) (types.AttributeValue, error)) Attr[E, T] {
	return Attr[E, T]{path: path, enc: enc}
}

func (a Attr[E, T]) attrPath() engine.Path { return engine.Path(a.path) }

// String returns the attribute's path, e.g. "profile.displayName".
func (a Attr[E, T]) String() string { return engine.Path(a.path).String() }

func (a Attr[E, T]) value(v T) (any, error) {
	av, err := a.enc(v)
	if err != nil {
		return nil, fmt.Errorf("wattdb: %s: %w", a, err)
	}
	if av == nil {
		return engine.Undefined, nil
	}
	return engine.FromAttributeValue(av), nil
}

func (a Attr[E, T]) op(op string, vals ...T) Cond {
	return Cond{build: func() (engine.Cond, error) {
		c := engine.CondOp{Op: op, Path: a.attrPath()}
		for _, v := range vals {
			x, err := a.value(v)
			if err != nil {
				return nil, err
			}
			c.Values = append(c.Values, x)
		}
		return c, nil
	}}
}

func (a Attr[E, T]) Eq(v T) Cond           { return a.op("eq", v) }
func (a Attr[E, T]) Ne(v T) Cond           { return a.op("ne", v) }
func (a Attr[E, T]) Gt(v T) Cond           { return a.op("gt", v) }
func (a Attr[E, T]) Gte(v T) Cond          { return a.op("gte", v) }
func (a Attr[E, T]) Lt(v T) Cond           { return a.op("lt", v) }
func (a Attr[E, T]) Lte(v T) Cond          { return a.op("lte", v) }
func (a Attr[E, T]) Between(lo, hi T) Cond { return a.op("between", lo, hi) }
func (a Attr[E, T]) BeginsWith(v T) Cond   { return a.op("begins", v) }
func (a Attr[E, T]) Exists() Cond          { return a.op("exists") }
func (a Attr[E, T]) NotExists() Cond       { return a.op("notExists") }

// Contains is DynamoDB's contains() with a value of the attribute's type,
// e.g. a substring of a string attribute.
func (a Attr[E, T]) Contains(v T) Cond    { return a.op("contains", v) }
func (a Attr[E, T]) NotContains(v T) Cond { return a.op("notContains", v) }

// Type is DynamoDB's attribute_type(), with a type code such as "S" or "N".
func (a Attr[E, T]) Type(code string) Cond {
	return Cond{build: func() (engine.Cond, error) {
		return engine.CondOp{Op: "type", Path: a.attrPath(), Values: []any{code}}, nil
	}}
}

// NumberAttr describes a number attribute; Add and Subtract accept it.
type NumberAttr[E any, T ~int | ~int64 | ~float64] struct {
	Attr[E, T]
}

// NewNumberAttr is used by generated code.
func NewNumberAttr[E any, T ~int | ~int64 | ~float64](path []string, enc func(T) (types.AttributeValue, error)) NumberAttr[E, T] {
	return NumberAttr[E, T]{NewAttr[E, T](path, enc)}
}

func (NumberAttr[E, T]) addable() {}

// SetAttr describes a set attribute of I values, held as []I; Add and Delete
// accept it.
type SetAttr[E, I any] struct {
	Attr[E, []I]
	item func(I) (types.AttributeValue, error)
}

// NewSetAttr is used by generated code.
func NewSetAttr[E, I any](path []string, enc func([]I) (types.AttributeValue, error), item func(I) (types.AttributeValue, error)) SetAttr[E, I] {
	return SetAttr[E, I]{Attr: NewAttr[E, []I](path, enc), item: item}
}

func (SetAttr[E, I]) addable() {}

// Contains is true when the set holds v.
func (s SetAttr[E, I]) Contains(v I) Cond    { return itemCond(s.Attr, s.item, "contains", v) }
func (s SetAttr[E, I]) NotContains(v I) Cond { return itemCond(s.Attr, s.item, "notContains", v) }

// ListAttr describes a list attribute of I values; At addresses one item,
// whose descriptor is a D.
type ListAttr[E, I, D any] struct {
	Attr[E, []I]
	item func(I) (types.AttributeValue, error)
	at   func(path []string) D
}

// NewListAttr is used by generated code.
func NewListAttr[E, I, D any](path []string, enc func([]I) (types.AttributeValue, error), item func(I) (types.AttributeValue, error), at func([]string) D) ListAttr[E, I, D] {
	return ListAttr[E, I, D]{Attr: NewAttr[E, []I](path, enc), item: item, at: at}
}

// At addresses item i (ElectroDB's "path[i]").
func (l ListAttr[E, I, D]) At(i int) D {
	return l.at(append(append([]string{}, l.path...), strconv.Itoa(i)))
}

func (ListAttr[E, I, D]) appendable() {}

// Contains is true when the list holds v.
func (l ListAttr[E, I, D]) Contains(v I) Cond    { return itemCond(l.Attr, l.item, "contains", v) }
func (l ListAttr[E, I, D]) NotContains(v I) Cond { return itemCond(l.Attr, l.item, "notContains", v) }

func itemCond[E, T, I any](a Attr[E, T], enc func(I) (types.AttributeValue, error), op string, v I) Cond {
	return Cond{build: func() (engine.Cond, error) {
		av, err := enc(v)
		if err != nil {
			return nil, err
		}
		return engine.CondOp{Op: op, Path: a.attrPath(), Values: []any{engine.FromAttributeValue(av)}}, nil
	}}
}

// SubPath is used by generated code: a nested descriptor's path.
func SubPath(parent []string, child string) []string {
	return append(append([]string{}, parent...), child)
}
