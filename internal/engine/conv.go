package engine

import (
	"errors"
	"fmt"
	"strings"

	"github.com/outboundani/wattdb/internal/model"
)

// This file ports ElectroDB's conversions (conversions.js and the
// _from*To* helpers in entity.js), find/match index selection, and parse.

// ErrConversion is returned when a conversion's input doesn't fit.
var ErrConversion = errors.New("wattdb: invalid conversion input")

func convErr(format string, args ...any) error {
	return fmt.Errorf("%w: %s", ErrConversion, fmt.Sprintf(format, args...))
}

// Strict is a conversion strictness: "", "pk" or "all".
type Strict string

// pagerIndex mirrors _constructPagerIndex.
func pagerIndex(m *model.Model, ix *model.Index, item Attrs, relaxedPK, relaxedSK bool) (map[string]any, error) {
	pick := func(names []string, relaxed bool) (Attrs, error) {
		if relaxed {
			out := Attrs{}
			for _, n := range names {
				if v, ok := item[n]; ok {
					out[n] = v
				}
			}
			return out, nil
		}
		return expectFacets(item, names)
	}
	pk, err := pick(ix.PK.Composites, relaxedPK)
	if err != nil {
		return nil, err
	}
	sk, err := pick(ix.SK.Composites, relaxedSK)
	if err != nil {
		return nil, err
	}
	if ix.Type == model.Composite {
		out := map[string]any{}
		for k, v := range mergeAttrs(pk, sk) {
			if a, ok := m.Attribute(k); ok {
				out[a.Field] = v
			}
		}
		return out, nil
	}
	keys, err := indexKeys(m, ix, pk, []Attrs{sk}, keyOptions{})
	if err != nil {
		return nil, err
	}
	out := map[string]any{ix.PK.Field: keys.PK}
	if ix.HasSK && len(keys.SK) > 0 {
		out[ix.SK.Field] = keys.SK[0]
	}
	return out, nil
}

// suppliedPager mirrors _formatSuppliedPager.
func suppliedPager(m *model.Model, ix *model.Index, item Attrs, relaxedPK, relaxedSK bool) (map[string]any, error) {
	if len(item) == 0 {
		return nil, nil
	}
	out, err := pagerIndex(m, ix, item, relaxedPK, relaxedSK)
	if err != nil {
		return nil, err
	}
	if ix != m.TableIndex() {
		t, err := pagerIndex(m, m.TableIndex(), item, relaxedPK, relaxedSK)
		if err != nil {
			return nil, err
		}
		for k, v := range t {
			out[k] = v
		}
	}
	return out, nil
}

// CompositeToKeysByIndex mirrors byAccessPattern.<ap>.fromComposite.toKeys.
func CompositeToKeysByIndex(m *model.Model, ix *model.Index, provided Attrs, strict Strict) (map[string]any, error) {
	return suppliedPager(m, ix, provided, strict != "pk" && strict != "all", strict != "all")
}

// CompositeToKeys mirrors fromComposite.toKeys.
func CompositeToKeys(m *model.Model, provided Attrs, strict Strict) (map[string]any, error) {
	if len(provided) == 0 {
		return nil, convErr("composite is empty")
	}
	secondary := Strict("")
	if strict == "all" || strict == "pk" {
		secondary = "pk"
	}
	out := map[string]any{}
	for _, ix := range m.Indexes {
		s := secondary
		if ix.Name == "" {
			s = strict
		}
		keys, err := CompositeToKeysByIndex(m, ix, provided, s)
		if err != nil {
			return nil, err
		}
		for k, v := range keys {
			out[k] = v
		}
	}
	if len(out) == 0 {
		return nil, convErr("composite didn't form any keys")
	}
	return out, nil
}

// CompositeToCursor mirrors fromComposite.toCursor (strict "all").
func CompositeToCursor(m *model.Model, provided Attrs) (string, error) {
	keys, err := CompositeToKeys(m, provided, "all")
	if err != nil {
		return "", err
	}
	return EncodeCursor(keys)
}

// verifyKeys mirrors _verifyKeys.
func verifyKeys(m *model.Model, ix *model.Index, provided map[string]any) bool {
	if ix.Type == model.Composite {
		item := translateFromFields(m, provided)
		for _, n := range ix.PK.Composites {
			if _, ok := item[n]; !ok {
				return false
			}
		}
		return true
	}
	if _, ok := provided[ix.PK.Field]; !ok {
		return false
	}
	if ix.HasSK {
		_, ok := provided[ix.SK.Field]
		return ok
	}
	return true
}

func translateFromFields(m *model.Model, item map[string]any) Attrs {
	out := Attrs{}
	for _, a := range m.Attributes {
		if v, ok := item[a.Field]; ok {
			out[a.Name] = v
		}
	}
	return out
}

// KeysToCursorByIndex mirrors byAccessPattern.<ap>.fromKeys.toCursor.
func KeysToCursorByIndex(m *model.Model, ix *model.Index, provided map[string]any) (string, error) {
	if !verifyKeys(m, m.TableIndex(), provided) {
		return "", convErr("keys don't include the primary index's key fields")
	}
	if !verifyKeys(m, ix, provided) {
		return "", convErr("keys don't include index %s's key fields", displayIndexName(ix))
	}
	return EncodeCursor(provided)
}

// KeysToCursor mirrors fromKeys.toCursor.
func KeysToCursor(provided map[string]any) (string, error) {
	if len(provided) == 0 {
		return "", convErr("keys are empty")
	}
	return EncodeCursor(provided)
}

// KeysToCompositeByIndex mirrors byAccessPattern.<ap>.fromKeys.toComposite.
func KeysToCompositeByIndex(m *model.Model, ix *model.Index, provided map[string]any) (Attrs, error) {
	all := Attrs{}
	if ix.Type == model.Composite {
		item := translateFromFields(m, provided)
		for _, n := range ix.Composites() {
			if v, ok := item[n]; ok {
				all[n] = v
			}
		}
	} else {
		parts := deconstructIndex(m, ix, provided)
		if parts == nil {
			return nil, convErr("keys don't include valid values for index %s", displayIndexName(ix))
		}
		all = parts
	}
	if ix != m.TableIndex() {
		table := deconstructIndex(m, m.TableIndex(), provided)
		if table == nil {
			return all, nil
		}
		for k, v := range table {
			all[k] = v
		}
	}
	if len(all) == 0 {
		return nil, convErr("keys didn't form any composite attributes")
	}
	return all, nil
}

// KeysToComposite mirrors fromKeys.toComposite.
func KeysToComposite(m *model.Model, provided map[string]any) (Attrs, error) {
	if len(provided) == 0 {
		return nil, convErr("keys are empty")
	}
	out := Attrs{}
	for _, ix := range m.Indexes {
		c, err := KeysToCompositeByIndex(m, ix, provided)
		if err != nil {
			return nil, err
		}
		for k, v := range c {
			if _, ok := out[k]; !ok {
				out[k] = v
			}
		}
	}
	if len(out) == 0 {
		return nil, convErr("keys didn't form any composite attributes")
	}
	return out, nil
}

// CursorToKeys mirrors fromCursor.toKeys.
func CursorToKeys(cursor string) (map[string]any, error) {
	if cursor == "" {
		return nil, convErr("cursor is empty")
	}
	return DecodeCursor(cursor)
}

// CursorToComposite mirrors fromCursor.toComposite.
func CursorToComposite(m *model.Model, cursor string) (Attrs, error) {
	keys, err := CursorToKeys(cursor)
	if err != nil {
		return nil, err
	}
	return KeysToComposite(m, keys)
}

// CursorToCompositeByIndex mirrors byAccessPattern.<ap>.fromCursor.toComposite.
func CursorToCompositeByIndex(m *model.Model, ix *model.Index, cursor string) (Attrs, error) {
	keys, err := CursorToKeys(cursor)
	if err != nil {
		return nil, err
	}
	return KeysToCompositeByIndex(m, ix, keys)
}

// CompositeToCursorByIndex mirrors byAccessPattern.<ap>.fromComposite.toCursor.
func CompositeToCursorByIndex(m *model.Model, ix *model.Index, provided Attrs) (string, error) {
	if len(provided) == 0 {
		return "", convErr("composite is empty")
	}
	keys, err := suppliedPager(m, ix, provided, false, false)
	if err != nil {
		return "", err
	}
	return KeysToCursorByIndex(m, ix, keys)
}

func displayIndexName(ix *model.Index) string {
	if ix.Name == "" {
		return "(Primary Index)"
	}
	return fmt.Sprintf("%q", ix.Name)
}

// BestIndex mirrors _findBestIndexKeyMatch: the access pattern a find or
// match should query, or "" (with scan true) when none fits.
func BestIndex(m *model.Model, attrs Attrs) (pattern string, scan bool) {
	type slot struct {
		ix   *model.Index
		name string
		kt   model.KeyType
		next bool
	}
	// slots[j][s]: composite j of index s.
	var slots [][]*slot
	for s, ix := range m.Indexes {
		comps := append(append([]string{}, ix.PK.Composites...), ix.SK.Composites...)
		for j, name := range comps {
			for len(slots) <= j {
				slots = append(slots, make([]*slot, len(m.Indexes)))
			}
			kt := model.PK
			if j >= len(ix.PK.Composites) {
				kt = model.SK
			}
			slots[j][s] = &slot{ix: ix, name: name, kt: kt, next: j+1 < len(comps)}
		}
	}
	type match struct {
		ix                   *model.Index
		allKeys, hasSK, done bool
		count                int
	}
	matches := make([]*match, len(m.Indexes))
	for j := range slots {
		for s := range m.Indexes {
			sl := slots[j][s]
			if matches[s] == nil {
				if sl == nil {
					continue
				}
				matches[s] = &match{ix: sl.ix}
			}
			mt := matches[s]
			provided := false
			if sl != nil {
				_, provided = attrs[sl.name]
			}
			if mt.done || sl == nil || !provided {
				mt.done = true
				continue
			}
			if sl.next && j+1 < len(slots) && slots[j+1][s] != nil && slots[j+1][s].kt == model.SK {
				mt.hasSK = true
			} else if !sl.next {
				mt.allKeys = true
			}
			mt.count++
		}
	}
	max := 0
	var eligible []*match
	for _, mt := range matches {
		if mt != nil && (mt.hasSK || mt.allKeys) {
			eligible = append(eligible, mt)
			if mt.count > max {
				max = mt.count
			}
		}
	}
	var ranked [4]*match
	for _, mt := range eligible {
		isTable := mt.ix.Name == ""
		switch {
		case isTable && mt.allKeys:
			ranked[0] = mt
		case isTable && mt.count == max:
			ranked[1] = mt
		case mt.allKeys:
			ranked[2] = mt
		case mt.count == max:
			ranked[3] = mt
		}
	}
	for _, mt := range ranked {
		if mt != nil {
			return mt.ix.AccessPattern, false
		}
	}
	return "", true
}

// Parse mirrors entity.parse: format a raw response item ("Item", "Items"
// or "Attributes"), ignoring ownership unless asked not to.
func Parse(m *model.Model, resp map[string]any, opt FormatOptions, ignoreOwnership bool) (any, error) {
	owns := func(item map[string]any) bool {
		if ignoreOwnership && len(opt.Attributes) > 0 && !itemIncludesKeys(m, item) {
			return true
		}
		if ignoreOwnership && ownsKeys(m, item) {
			return true
		}
		return OwnsItem(m, item)
	}
	if item, ok := resp["Item"].(map[string]any); ok {
		if !owns(item) {
			return nil, nil
		}
		return FormatItem(m, item, opt)
	}
	if items, ok := resp["Items"].([]map[string]any); ok {
		out := []map[string]any{}
		for _, it := range items {
			if owns(it) {
				f, err := FormatItem(m, it, opt)
				if err != nil {
					return nil, err
				}
				out = append(out, f)
			}
		}
		return out, nil
	}
	if attrs, ok := resp["Attributes"].(map[string]any); ok {
		data, err := FormatItem(m, attrs, opt)
		if err != nil || len(data) == 0 {
			return nil, err
		}
		return data, nil
	}
	return nil, nil
}

// NewFind mirrors find: a query on the best-matching access pattern, or a
// scan of the table when none fits.
func NewFind(m *model.Model, facets []Entry) *Query {
	pattern, scan := BestIndex(m, entriesToAttrs(facets))
	if scan {
		return NewScan(m, "")
	}
	return NewQuery(m, pattern, facets)
}

// NewMatch mirrors match: find, filtered to items equal to every given
// attribute.
func NewMatch(m *model.Model, facets []Entry) *Query {
	q := NewFind(m, facets)
	if q.err != nil {
		return q
	}
	w := &Write{m: m, cond: q.filter}
	var parts []string
	for _, e := range facets {
		if isUndefined(e.Value) {
			continue
		}
		if _, ok := m.Attribute(e.Path); !ok {
			continue
		}
		expr, err := w.renderCond(CondOp{Op: "eq", Path: Path{e.Path}, Values: []any{e.Value}})
		if err != nil {
			return q.fail(err)
		}
		parts = append(parts, expr)
	}
	q.filter.addExpr(strings.Join(parts, " AND "), false)
	return q
}
