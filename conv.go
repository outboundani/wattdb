package wattdb

import (
	"fmt"

	"github.com/aws/aws-sdk-go-v2/service/dynamodb/types"
	"github.com/outboundani/wattdb/internal/engine"
	"github.com/outboundani/wattdb/internal/model"
)

// ErrConversion is returned when a conversion's input doesn't fit the
// entity.
var ErrConversion = engine.ErrConversion

// Strict is how strictly a conversion needs composites (ElectroDB's
// "strict" option).
type Strict string

const (
	// StrictNone builds whatever keys the composites allow (the default).
	StrictNone Strict = ""
	// StrictPK needs every partition-key composite.
	StrictPK Strict = "pk"
	// StrictAll needs every composite.
	StrictAll Strict = "all"
)

// Conv converts between composites, keys and cursors (decision D45).
// Generated Conv types wrap it; P is the entity's Partial, which holds
// composites decoded from keys.
type Conv[E, P any] struct {
	t *Table[E, P]
}

// Conv returns the table's conversions; generated code wraps it.
func (t *Table[E, P]) Conv() Conv[E, P] { return Conv[E, P]{t: t} }

func (c Conv[E, P]) index(accessPattern string) (*indexRef, error) {
	ix, ok := c.t.def.model.AccessPattern(accessPattern)
	if !ok {
		return nil, fmt.Errorf("wattdb: unknown access pattern %q", accessPattern)
	}
	return &indexRef{ix}, nil
}

func keysOut(keys map[string]any, err error) (map[string]types.AttributeValue, error) {
	if err != nil {
		return nil, err
	}
	return engine.ToAttributeMap(keys)
}

func (c Conv[E, P]) partial(attrs engine.Attrs, err error) (P, error) {
	var zero P
	if err != nil {
		return zero, err
	}
	return c.t.decodePartialItem(attrs)
}

// KeysFromComposite builds every index's keys from composite attributes;
// generated code passes the encoded composites.
func (c Conv[E, P]) KeysFromComposite(composite map[string]types.AttributeValue, err error, strict Strict) (map[string]types.AttributeValue, error) {
	if err != nil {
		return nil, err
	}
	return keysOut(engine.CompositeToKeys(c.t.def.model, engine.FromAttributeMap(composite), engine.Strict(strict)))
}

// CursorFromComposite builds a cursor holding every index's keys; all
// composites are needed.
func (c Conv[E, P]) CursorFromComposite(composite map[string]types.AttributeValue, err error) (Cursor, error) {
	if err != nil {
		return "", err
	}
	s, err := engine.CompositeToCursor(c.t.def.model, engine.FromAttributeMap(composite))
	return Cursor(s), err
}

// CursorFromKeys encodes keys as a cursor.
func (c Conv[E, P]) CursorFromKeys(keys map[string]types.AttributeValue) (Cursor, error) {
	s, err := engine.KeysToCursor(engine.FromAttributeMap(keys))
	return Cursor(s), err
}

// CompositeFromKeys reads composite attributes back out of keys, which
// must include every index's key fields.
func (c Conv[E, P]) CompositeFromKeys(keys map[string]types.AttributeValue) (P, error) {
	return c.partial(engine.KeysToComposite(c.t.def.model, engine.FromAttributeMap(keys)))
}

// KeysFromCursor decodes a cursor's keys.
func (c Conv[E, P]) KeysFromCursor(cursor Cursor) (map[string]types.AttributeValue, error) {
	return keysOut(engine.CursorToKeys(string(cursor)))
}

// CompositeFromCursor reads composite attributes back out of a cursor.
func (c Conv[E, P]) CompositeFromCursor(cursor Cursor) (P, error) {
	return c.partial(engine.CursorToComposite(c.t.def.model, string(cursor)))
}

// IndexKeysFromComposite builds an access pattern's keys (and the table's)
// from composite attributes.
func (c Conv[E, P]) IndexKeysFromComposite(accessPattern string, composite map[string]types.AttributeValue, err error, strict Strict) (map[string]types.AttributeValue, error) {
	if err != nil {
		return nil, err
	}
	ix, err := c.index(accessPattern)
	if err != nil {
		return nil, err
	}
	return keysOut(engine.CompositeToKeysByIndex(c.t.def.model, ix.ix, engine.FromAttributeMap(composite), engine.Strict(strict)))
}

// IndexCursorFromComposite builds a cursor for an access pattern from
// composite attributes; all composites are needed.
func (c Conv[E, P]) IndexCursorFromComposite(accessPattern string, composite map[string]types.AttributeValue, err error) (Cursor, error) {
	if err != nil {
		return "", err
	}
	ix, err := c.index(accessPattern)
	if err != nil {
		return "", err
	}
	s, err := engine.CompositeToCursorByIndex(c.t.def.model, ix.ix, engine.FromAttributeMap(composite))
	return Cursor(s), err
}

// IndexCursorFromKeys encodes an access pattern's keys as a cursor,
// checking they include its key fields and the table's.
func (c Conv[E, P]) IndexCursorFromKeys(accessPattern string, keys map[string]types.AttributeValue) (Cursor, error) {
	ix, err := c.index(accessPattern)
	if err != nil {
		return "", err
	}
	s, err := engine.KeysToCursorByIndex(c.t.def.model, ix.ix, engine.FromAttributeMap(keys))
	return Cursor(s), err
}

// IndexCompositeFromKeys reads an access pattern's composite attributes
// (and the table's) back out of keys.
func (c Conv[E, P]) IndexCompositeFromKeys(accessPattern string, keys map[string]types.AttributeValue) (P, error) {
	var zero P
	ix, err := c.index(accessPattern)
	if err != nil {
		return zero, err
	}
	return c.partial(engine.KeysToCompositeByIndex(c.t.def.model, ix.ix, engine.FromAttributeMap(keys)))
}

// IndexCompositeFromCursor reads an access pattern's composite attributes
// back out of a cursor.
func (c Conv[E, P]) IndexCompositeFromCursor(accessPattern string, cursor Cursor) (P, error) {
	var zero P
	ix, err := c.index(accessPattern)
	if err != nil {
		return zero, err
	}
	return c.partial(engine.CursorToCompositeByIndex(c.t.def.model, ix.ix, string(cursor)))
}

type indexRef struct{ ix *model.Index }
