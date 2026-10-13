package engine

import (
	"bytes"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"regexp"
	"sort"
	"strconv"
	"strings"

	"github.com/outboundani/wattdb/internal/model"
)

// ErrInvalidCursor is returned for a cursor that can't be decoded.
var ErrInvalidCursor = errors.New("wattdb: invalid cursor")

// EncodeCursor mirrors ElectroDB's cursorFormatter.serialize: base64url of
// the JSON key. Keys are written sorted (decision D32). A nil key is "".
func EncodeCursor(key map[string]any) (string, error) {
	if len(key) == 0 {
		return "", nil
	}
	names := make([]string, 0, len(key))
	for k := range key {
		names = append(names, k)
	}
	sort.Strings(names)
	var b bytes.Buffer
	b.WriteByte('{')
	for i, k := range names {
		if i > 0 {
			b.WriteByte(',')
		}
		kb, _ := json.Marshal(k)
		b.Write(kb)
		b.WriteByte(':')
		switch v := key[k].(type) {
		case string:
			vb, _ := json.Marshal(v)
			b.Write(vb)
		case Number:
			b.WriteString(string(v))
		default:
			return "", fmt.Errorf("wattdb: can't put %T in a cursor", v)
		}
	}
	b.WriteByte('}')
	return base64.RawURLEncoding.EncodeToString(b.Bytes()), nil
}

// DecodeCursor mirrors cursorFormatter.deserialize.
func DecodeCursor(cursor string) (map[string]any, error) {
	if cursor == "" {
		return nil, nil
	}
	raw, err := base64.RawURLEncoding.DecodeString(strings.TrimRight(cursor, "="))
	if err != nil {
		return nil, fmt.Errorf("%w: %v", ErrInvalidCursor, err)
	}
	dec := json.NewDecoder(bytes.NewReader(raw))
	dec.UseNumber()
	var m map[string]any
	if err := dec.Decode(&m); err != nil {
		return nil, fmt.Errorf("%w: %v", ErrInvalidCursor, err)
	}
	out := map[string]any{}
	for k, v := range m {
		switch v := v.(type) {
		case string:
			out[k] = v
		case json.Number:
			out[k] = Number(v.String())
		default:
			return nil, fmt.Errorf("%w: unsupported key value for %q", ErrInvalidCursor, k)
		}
	}
	return out, nil
}

// trimKeys mirrors _trimKeysToIndex: keep the index's and the table's key
// fields.
func trimKeys(m *model.Model, ix *model.Index, keys map[string]any) map[string]any {
	if keys == nil {
		return nil
	}
	table := m.TableIndex()
	out := map[string]any{}
	for _, f := range []string{ix.PK.Field, ix.SK.Field, table.PK.Field, table.SK.Field} {
		if f == "" {
			continue
		}
		if v, ok := keys[f]; ok {
			out[f] = v
		}
	}
	return out
}

// StartKey turns a cursor into the request's ExclusiveStartKey.
func (r *QueryRequest) StartKey(cursor string) (map[string]any, error) {
	keys, err := DecodeCursor(cursor)
	if err != nil || keys == nil {
		return nil, err
	}
	return trimKeys(r.q.m, r.q.ix, keys), nil
}

// keyDeconstructor mirrors _createKeyDeconstructor.
type keyDeconstructor struct {
	re      *regexp.Regexp
	names   []string
	types   []model.AttrType
	numeric bool
}

func newDeconstructor(m *model.Model, k *model.Key) *keyDeconstructor {
	d := &keyDeconstructor{numeric: k.Cast == model.CastNumber}
	pattern := "^" + regexp.QuoteMeta(k.Prefix)
	for _, l := range k.Labels {
		switch {
		case k.Custom && l.Name == "" && l.Label != "":
			pattern += regexp.QuoteMeta(l.Label)
		case k.Custom:
			pattern += regexp.QuoteMeta(l.Label) + "(.+)"
		default:
			pattern += "#" + regexp.QuoteMeta(l.Label) + "_(.+)"
		}
		d.names = append(d.names, l.Name)
		if a, ok := m.Attribute(l.Name); ok {
			d.types = append(d.types, a.Type)
		} else {
			d.types = append(d.types, "")
		}
	}
	if k.HasPostfix {
		pattern += regexp.QuoteMeta(k.Postfix)
	}
	d.re = regexp.MustCompile("(?i)" + pattern + "$")
	return d
}

// parse returns the composites in a key, or nil when it doesn't match.
func (d *keyDeconstructor) parse(key any) Attrs {
	var s string
	isNum := false
	switch k := key.(type) {
	case string:
		s = k
	case Number:
		s, isNum = string(k), true
	default:
		return nil
	}
	numeric := d.numeric && isNum
	match := d.re.FindStringSubmatch(s)
	if match == nil && !numeric {
		return nil
	}
	out := Attrs{}
	for i, name := range d.names {
		var v string
		if numeric {
			v = s
		} else if i+1 < len(match) {
			v = match[i+1]
		} else {
			continue
		}
		var value any = v
		switch d.types[i] {
		case model.TypeNumber:
			value = parseFloatNumber(v)
		case model.TypeBoolean:
			value = v == "true"
		}
		if name != "" {
			out[name] = value
		}
	}
	return out
}

// parseFloatNumber mirrors parseFloat for numeric key composites.
func parseFloatNumber(s string) any {
	end := 0
	for end < len(s) && (s[end] >= '0' && s[end] <= '9' || s[end] == '.' || s[end] == '-' || s[end] == '+' || s[end] == 'e' || s[end] == 'E') {
		end++
	}
	f, err := strconv.ParseFloat(s[:end], 64)
	if err != nil {
		return nil // NaN
	}
	return FloatNumber(f)
}

// deconstructIndex mirrors _deconstructIndex.
func deconstructIndex(m *model.Model, ix *model.Index, keys map[string]any) Attrs {
	pk, ok := keys[ix.PK.Field]
	if !ok {
		return nil
	}
	out := newDeconstructor(m, &ix.PK).parse(pk)
	if out == nil {
		return nil
	}
	if ix.HasSK {
		sk, ok := keys[ix.SK.Field]
		if !ok {
			return nil
		}
		skParts := newDeconstructor(m, &ix.SK).parse(sk)
		if skParts == nil {
			return nil
		}
		for k, v := range skParts {
			out[k] = v
		}
	}
	return out
}

// KeysToItem mirrors _formatKeysToItem: the composites in an item's keys, or
// nil when they don't form a complete table key.
func KeysToItem(m *model.Model, ix *model.Index, keys map[string]any) Attrs {
	if len(keys) == 0 {
		return nil
	}
	parts := deconstructIndex(m, ix, keys)
	if parts == nil {
		return nil
	}
	table := m.TableIndex()
	if ix != table {
		tableParts := deconstructIndex(m, table, keys)
		if tableParts == nil {
			return nil
		}
		for k, v := range tableParts {
			parts[k] = v
		}
	}
	for _, name := range table.Composites() {
		if _, ok := parts[name]; !ok {
			return nil
		}
	}
	return parts
}

// ownsKeys mirrors ownsKeys: the item's table keys have this entity's shape.
func ownsKeys(m *model.Model, item map[string]any) bool {
	table := m.TableIndex()
	pk := table.PK
	pkMatch := false
	switch v := item[pk.Field].(type) {
	case string:
		pkMatch = strings.HasPrefix(v, pk.Prefix)
	case Number:
		pkMatch = pk.Cast == model.CastNumber
	}
	skMatch := pkMatch && !table.HasSK
	if pkMatch && table.HasSK {
		sk := table.SK
		switch v := item[sk.Field].(type) {
		case Number:
			skMatch = sk.Cast == model.CastNumber
		case string:
			skMatch = (sk.Prefix == "" || strings.HasPrefix(v, sk.Prefix)) && (sk.Postfix == "" || strings.HasSuffix(v, sk.Postfix))
		}
	}
	return pkMatch && skMatch && KeysToItem(m, table, item) != nil
}

func itemIncludesKeys(m *model.Model, item map[string]any) bool {
	table := m.TableIndex()
	if _, ok := item[table.PK.Field]; !ok {
		return false
	}
	if !table.HasSK {
		return true
	}
	_, ok := item[table.SK.Field]
	return ok
}

// ignoresOwnership mirrors how ElectroDB decides ignoreOwnership: an index
// projecting only keys, or attributes without the identifiers, can't be
// checked by identifiers.
func (r *QueryRequest) ignoresOwnership() bool {
	if r.opts.IgnoreOwnership {
		return true
	}
	return projectionHidesIdentifiers(r.q.m, r.q.ix)
}

func projectionHidesIdentifiers(m *model.Model, ix *model.Index) bool {
	p := ix.Projection
	if p == nil {
		return false
	}
	if len(p.Attributes) == 0 {
		return p.Kind == "keys_only"
	}
	hasE, hasV := false, false
	for _, a := range p.Attributes {
		hasE = hasE || a == m.Identifiers.Entity
		hasV = hasV || a == m.Identifiers.Version
	}
	return !(hasE && hasV)
}

// owns mirrors Entity.is.
func (r *QueryRequest) owns(item map[string]any, ignore, hydrate bool) bool {
	m := r.q.m
	if ignore && len(r.opts.Attributes) > 0 && !itemIncludesKeys(m, item) {
		return true
	}
	if (ignore || hydrate) && ownsKeys(m, item) {
		return true
	}
	return OwnsItem(m, item)
}

// FormatItems formats one page's items (formatResponse for Items), dropping
// items this entity doesn't own. With hydrate, items keep their keys.
func (r *QueryRequest) FormatItems(items []map[string]any) ([]map[string]any, error) {
	ignore := r.ignoresOwnership() || r.opts.Hydrate
	opt := FormatOptions{IncludeKeys: r.opts.IncludeKeys || r.opts.Hydrate, Attributes: r.opts.Attributes}
	out := []map[string]any{}
	for _, item := range items {
		if !r.owns(item, ignore, r.opts.Hydrate) {
			continue
		}
		data, err := FormatItem(r.q.m, item, opt)
		if err != nil {
			return nil, err
		}
		out = append(out, data)
	}
	return out, nil
}

// KeysFromItem mirrors _fromCompositeToKeysByIndex (relaxed): the index and
// table keys for a formatted item, used as the cursor when Count stops early.
func (r *QueryRequest) KeysFromItem(item map[string]any) (map[string]any, error) {
	m := r.q.m
	build := func(ix *model.Index) (map[string]any, error) {
		pk, sk := Attrs{}, Attrs{}
		for _, n := range ix.PK.Composites {
			if v, ok := item[n]; ok {
				pk[n] = v
			}
		}
		for _, n := range ix.SK.Composites {
			if v, ok := item[n]; ok {
				sk[n] = v
			}
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
	out, err := build(r.q.ix)
	if err != nil {
		return nil, err
	}
	if r.q.ix != m.TableIndex() {
		t, err := build(m.TableIndex())
		if err != nil {
			return nil, err
		}
		for k, v := range t {
			out[k] = v
		}
	}
	return out, nil
}

// HydrateKeys returns, for each item with keys, the table composites to fetch
// (nil where the keys don't form a table key).
func (r *QueryRequest) HydrateKeys(items []map[string]any) []Attrs {
	out := make([]Attrs, len(items))
	for i, it := range items {
		out[i] = KeysToItem(r.q.m, r.q.m.TableIndex(), it)
	}
	return out
}

// BatchGetKeys builds the table keys for a batch get of composites.
func BatchGetKeys(m *model.Model, composites []Attrs) ([]map[string]any, error) {
	out := make([]map[string]any, len(composites))
	for i, c := range composites {
		k, err := PrimaryKey(m, c)
		if err != nil {
			return nil, err
		}
		out[i] = k
	}
	return out, nil
}

// FormatBatchItem formats one batch get item; ignore is the user's
// ignoreOwnership.
func FormatBatchItem(m *model.Model, item map[string]any, ignore bool, attributes []string) (map[string]any, bool, error) {
	owned := OwnsItem(m, item) || (ignore && ownsKeys(m, item))
	if !owned {
		return nil, false, nil
	}
	data, err := FormatItem(m, item, FormatOptions{Attributes: attributes})
	return data, true, err
}

// IgnoresOwnership reports whether this request formats without checking
// identifiers.
func (r *QueryRequest) IgnoresOwnership() bool { return r.ignoresOwnership() }

// Model returns the request's model.
func (r *QueryRequest) Model() *model.Model { return r.q.m }

// Runner performs the DynamoDB calls of a query, in engine values.
type Runner interface {
	// Page runs the request with the given ExclusiveStartKey (nil for the
	// first page), returning the items and LastEvaluatedKey.
	Page(params map[string]any) (items []map[string]any, lastKey map[string]any, err error)
	// BatchGet fetches table keys (in chunks of up to 100), returning the
	// items found; missing and unprocessed keys are simply absent.
	BatchGet(table string, keys []map[string]any) ([]map[string]any, error)
}

// RunOptions control how many pages Run reads.
type RunOptions struct {
	// Pages is the number of pages to read; 0 means all.
	Pages int
	// Count stops after this many items (0 means no limit).
	Count int
	// IgnoreOwnership is the user's own ignoreOwnership, used when
	// hydrating.
	IgnoreOwnership bool
	Attributes      []string
}

// RunResult is what Run returns.
type RunResult struct {
	Items []map[string]any
	// Next is the LastEvaluatedKey to continue from (nil when done).
	Next map[string]any
	// Unprocessed lists the keys hydrate couldn't fetch.
	Unprocessed []map[string]any
	// Groups holds a collection query's items, per member.
	Groups [][]map[string]any
}

// WithStartKey returns the params with an ExclusiveStartKey.
func (r *QueryRequest) WithStartKey(key map[string]any) map[string]any {
	out := make(map[string]any, len(r.Params)+1)
	for k, v := range r.Params {
		out[k] = v
	}
	if len(key) > 0 {
		out["ExclusiveStartKey"] = key
	}
	return out
}

// Run mirrors executeQuery for queries and scans.
func (r *QueryRequest) Run(start map[string]any, opt RunOptions, run Runner) (RunResult, error) {
	if r.q.coll != nil {
		return r.runCollection(start, opt, run)
	}
	var res RunResult
	esk := start
	iterations, count := 0, 0
	for {
		items, lek, err := run.Page(r.WithStartKey(esk))
		if err != nil {
			return res, err
		}
		esk = lek
		formatted, err := r.FormatItems(items)
		if err != nil {
			return res, err
		}
		prev := count
		if opt.Count > 0 {
			count += len(formatted)
		}
		more := opt.Count > 0 && count > opt.Count
		if more {
			formatted = formatted[:opt.Count-prev]
		}
		if r.opts.Hydrate {
			data, unprocessed, err := r.hydrate(formatted, opt, run)
			if err != nil {
				return res, err
			}
			formatted = data
			res.Unprocessed = append(res.Unprocessed, unprocessed...)
		}
		res.Items = append(res.Items, formatted...)
		if more || (opt.Count > 0 && count == opt.Count) {
			esk = nil
			if n := len(res.Items); n > 0 {
				if esk, err = r.KeysFromItem(res.Items[n-1]); err != nil {
					return res, err
				}
			}
			break
		}
		iterations++
		if !(len(esk) > 0 && (opt.Pages == 0 || opt.Count > 0 || iterations < opt.Pages) && (opt.Count == 0 || count < opt.Count)) {
			break
		}
	}
	res.Next = esk
	return res, nil
}

// hydrate mirrors Entity.hydrate: fetch the full items behind keys.
func (r *QueryRequest) hydrate(items []map[string]any, opt RunOptions, run Runner) ([]map[string]any, []map[string]any, error) {
	return r.hydrateFor(r.q.m, items, opt, run)
}

func (r *QueryRequest) hydrateFor(m *model.Model, items []map[string]any, opt RunOptions, run Runner) ([]map[string]any, []map[string]any, error) {
	var composites []Attrs
	var validKeys []map[string]any
	for _, it := range items {
		it := KeysToItem(m, m.TableIndex(), it)
		if it != nil {
			composites = append(composites, it)
		}
	}
	for i, it := range items {
		if KeysToItem(m, m.TableIndex(), it) != nil {
			validKeys = append(validKeys, items[i])
		}
	}
	if len(composites) == 0 {
		return []map[string]any{}, nil, nil
	}
	keys, err := BatchGetKeys(m, composites)
	if err != nil {
		return nil, nil, err
	}
	table, _ := r.Params["TableName"].(string)
	found, err := run.BatchGet(table, keys)
	if err != nil {
		return nil, nil, err
	}
	// preserveBatchOrder: match results to keys by their table key values.
	keyString := func(k map[string]any) string {
		t := m.TableIndex()
		return keyString(k[t.PK.Field]) + "\x00" + keyString(k[t.SK.Field])
	}
	slots := map[string]map[string]any{}
	for _, it := range found {
		data, ok, err := FormatBatchItem(m, it, opt.IgnoreOwnership, opt.Attributes)
		if err != nil {
			return nil, nil, err
		}
		if ok {
			slots[keyString(it)] = data
		}
	}
	data := []map[string]any{}
	var unprocessed []map[string]any
	for i, k := range keys {
		if d, ok := slots[keyString(k)]; ok {
			data = append(data, d)
		} else {
			unprocessed = append(unprocessed, validKeys[i])
		}
	}
	return data, unprocessed, nil
}
