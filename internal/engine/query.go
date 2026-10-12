package engine

import (
	"fmt"
	"slices"
	"strings"

	"github.com/outboundani/wattdb/internal/model"
)

// QueryOptions are the options that shape query and scan requests and their
// results.
type QueryOptions struct {
	Table           string
	Limit           int
	Order           string // "", "asc", "desc"
	Consistent      bool
	Attributes      []string
	IgnoreOwnership bool
	// Compare is "keys" (default) or "attributes" (decision D51).
	Compare     string
	Raw         bool
	IncludeKeys bool
	Hydrate     bool
	// Exec is true when the request is sent rather than rendered.
	Exec bool
}

type skEntry struct {
	typ    QueryType
	facets []Entry
}

// Query builds a query or scan, mirroring ElectroDB's query clauses.
type Query struct {
	m      *model.Model
	ix     *model.Index
	scan   bool
	typ    QueryType
	pk     Attrs
	sk     []skEntry
	given  []Entry // the facets passed to the access pattern
	filter *filterExpr
	before []func(QueryOptions)
	err    error
	// provided lists the sk composites given, for "is" fulfilment.
	provided []string
}

func (q *Query) fail(err error) *Query {
	if q.err == nil {
		q.err = err
	}
	return q
}

func entriesToAttrs(es []Entry) Attrs {
	out := Attrs{}
	for _, e := range es {
		if !isUndefined(e.Value) {
			out[e.Path] = e.Value
		}
	}
	return out
}

// skComposites keeps the entries naming sk composites, in the given order
// (identifyCompositeAttributes' "composites").
func (q *Query) skComposites(es []Entry) []Entry {
	var out []Entry
	for _, e := range es {
		if slices.Contains(q.ix.SK.Composites, e.Path) {
			out = append(out, e)
		}
	}
	return out
}

// skOrdered keeps the defined sk composites in sk order
// (buildQueryComposites).
func (q *Query) skOrdered(es []Entry) []Entry {
	given := entriesToAttrs(es)
	var out []Entry
	for _, name := range q.ix.SK.Composites {
		if v, ok := given[name]; ok {
			out = append(out, Entry{Path: name, Value: v})
		}
	}
	return out
}

func (q *Query) setSK(typ QueryType, es []Entry) {
	if !q.ix.HasSK {
		return
	}
	q.sk = append(q.sk, skEntry{typ: typ, facets: es})
	for _, e := range es {
		if !slices.Contains(q.provided, e.Path) {
			q.provided = append(q.provided, e.Path)
		}
	}
}

// NewQuery starts a query on an access pattern with the given composites,
// in the order given.
func NewQuery(m *model.Model, accessPattern string, facets []Entry) *Query {
	ix, ok := m.AccessPattern(accessPattern)
	q := &Query{m: m, ix: ix, filter: newFilterExpr(), typ: QueryIs, given: facets}
	if !ok {
		return q.fail(fmt.Errorf("unknown access pattern %q", accessPattern))
	}
	pk, err := expectFacets(entriesToAttrs(facets), ix.PK.Composites)
	if err != nil {
		return q.fail(err)
	}
	q.pk = pk
	q.setSK(QueryIs, q.skOrdered(facets))
	return q
}

// NewScan starts a scan of the table, or of an access pattern's index.
func NewScan(m *model.Model, accessPattern string) *Query {
	ix := m.TableIndex()
	if accessPattern != "" {
		var ok bool
		if ix, ok = m.AccessPattern(accessPattern); !ok {
			return &Query{err: fmt.Errorf("unknown access pattern %q", accessPattern)}
		}
	}
	return &Query{m: m, ix: ix, scan: true, filter: newFilterExpr()}
}

func (q *Query) filterProps(op string, es []Entry, asPrefix bool) {
	for _, e := range es {
		if isUndefined(e.Value) {
			continue
		}
		if a, ok := q.m.Attribute(e.Path); ok {
			q.filter.unsafeSet(asPrefix, op, a.Field, e.Value)
		}
	}
}

// compare runs fn before the params are built, as beforeBuildParams does.
func (q *Query) compareFilter(fn func(QueryOptions)) {
	q.before = append(q.before, fn)
}

// Begins mirrors begins().
func (q *Query) Begins(facets []Entry) *Query {
	if q.err != nil {
		return q
	}
	q.typ = QueryBegins
	q.setSK(QueryBegins, q.skOrdered(facets))
	return q
}

// Compare mirrors gt/gte/lt/lte.
func (q *Query) Compare(typ QueryType, facets []Entry) *Query {
	if q.err != nil {
		return q
	}
	q.typ = typ
	composites := q.skComposites(facets)
	switch typ {
	case QueryGte:
		q.setSK(typ, q.skOrdered(facets))
	default:
		q.setSK(typ, composites)
	}
	op := map[QueryType]string{QueryGt: "gt", QueryGte: "gte", QueryLt: "lt", QueryLte: "lte"}[typ]
	q.compareFilter(func(o QueryOptions) {
		if o.Compare != "attributes" || q.ix.SK.FieldRef {
			return
		}
		if typ == QueryGte && len(q.ix.SK.Composites) <= 1 {
			return
		}
		q.filterProps(op, composites, true)
	})
	return q
}

// Between mirrors between().
func (q *Query) Between(start, end []Entry) *Query {
	if q.err != nil {
		return q
	}
	q.typ = QueryBetween
	endSK := q.skComposites(end)
	startSK := q.skComposites(start)
	q.setSK(queryAnd, endSK)
	q.setSK(QueryBetween, startSK)
	q.compareFilter(func(o QueryOptions) {
		if o.Compare != "attributes" || q.ix.SK.FieldRef {
			return
		}
		q.filterProps("lte", endSK, true)
		q.filterProps("gte", startSK, true)
	})
	return q
}

// queryAnd is the end half of a between, as in ElectroDB.
const queryAnd QueryType = 100

// Where adds a filter.
func (q *Query) Where(c Cond) *Query {
	if q.err != nil {
		return q
	}
	w := &Write{m: q.m, cond: q.filter}
	expr, err := w.renderCond(c)
	if err != nil {
		return q.fail(err)
	}
	q.filter.addExpr(expr, false)
	return q
}

// consolidate mirrors _consolidateQueryFacets.
func (q *Query) consolidate() (Attrs, Attrs) {
	sk1, sk2 := Attrs{}, Attrs{}
	for _, e := range q.sk {
		for _, f := range e.facets {
			switch e.typ {
			case QueryBetween:
				sk1[f.Path] = f.Value
			case queryAnd:
				sk2[f.Path] = f.Value
			default:
				sk1[f.Path] = f.Value
				sk2[f.Path] = f.Value
			}
		}
	}
	return sk1, sk2
}

// orderedNames tracks expression attribute names in insertion order, as a
// JS object does.
type orderedNames struct {
	keys []string
	m    map[string]string
}

func (n *orderedNames) set(k, v string) {
	if n.m == nil {
		n.m = map[string]string{}
	}
	if _, ok := n.m[k]; !ok {
		n.keys = append(n.keys, k)
	}
	n.m[k] = v
}

// QueryRequest is a built query or scan.
type QueryRequest struct {
	Target string // Query or Scan
	Params map[string]any

	q     *Query
	opts  QueryOptions
	names []string
}

// Build renders the request (_queryParams / _makeScanParam).
func (q *Query) Build(table string, opt QueryOptions) (*QueryRequest, error) {
	if q.err != nil {
		return nil, q.err
	}
	if opt.Table != "" {
		table = opt.Table
	}
	// Options-dependent filters, in ElectroDB's order: whenOptions (from
	// the access pattern and scan), then beforeBuildParams (sort-key ops).
	if q.scan {
		if !opt.IgnoreOwnership {
			q.filter.unsafeSet(false, "eq", q.m.Identifiers.Entity, q.m.Entity)
			q.filter.unsafeSet(false, "eq", q.m.Identifiers.Version, q.m.Version)
		}
	} else {
		given := entriesToAttrs(q.given)
		var composites, unused []Entry
		for _, e := range q.given {
			switch {
			case slices.Contains(q.ix.SK.Composites, e.Path):
				composites = append(composites, e)
			case slices.Contains(q.ix.PK.Composites, e.Path):
			default:
				unused = append(unused, e)
			}
		}
		if q.ix.HasSK {
			if opt.Compare == "attributes" && len(q.ix.SK.Composites) > 1 {
				q.filterProps("eq", append(append([]Entry{}, unused...), composites...), false)
			}
			found := 0
			for _, n := range q.ix.SK.Composites {
				if _, ok := given[n]; ok {
					found++
				}
			}
			if q.ix.Type == model.Clustered && found < len(q.ix.SK.Composites) && !opt.IgnoreOwnership {
				q.filter.unsafeSet(false, "eq", q.m.Identifiers.Entity, q.m.Entity)
				q.filter.unsafeSet(false, "eq", q.m.Identifiers.Version, q.m.Version)
			}
		}
		for _, fn := range q.before {
			fn(opt)
		}
	}

	r := &QueryRequest{q: q, opts: opt, Params: map[string]any{}}
	names := &orderedNames{}
	values := map[string]any{}
	for _, k := range q.filter.nameOrder {
		names.set(k, q.filter.names[k])
	}
	for k, v := range q.filter.values {
		values[k] = v
	}
	var err error
	if q.scan {
		r.Target = "Scan"
		err = q.buildScan(r, names, values)
	} else {
		r.Target = "Query"
		err = q.buildQuery(r, names, values)
	}
	if err != nil {
		return nil, err
	}
	r.Params["TableName"] = table
	if q.ix.Name != "" {
		r.Params["IndexName"] = q.ix.Name
	}
	if opt.Limit > 0 {
		r.Params["Limit"] = opt.Limit
	}
	switch opt.Order {
	case "asc":
		r.Params["ScanIndexForward"] = true
	case "desc":
		r.Params["ScanIndexForward"] = false
	}
	if opt.Consistent {
		r.Params["ConsistentRead"] = true
	}
	q.applyProjection(r, names, opt)
	if len(names.keys) > 0 {
		r.Params["ExpressionAttributeNames"] = names.m
	}
	if len(values) > 0 {
		r.Params["ExpressionAttributeValues"] = values
	}
	r.names = names.keys
	return r, nil
}

func (q *Query) buildQuery(r *QueryRequest, names *orderedNames, values map[string]any) error {
	ix := q.ix
	sk1, sk2 := q.consolidate()
	var keys IndexKeys
	var err error
	if q.typ == QueryIs {
		keys, err = indexKeys(q.m, ix, q.pk, []Attrs{sk1, sk2}, keyOptions{})
	} else {
		keys, err = indexKeys(q.m, ix, q.pk, []Attrs{sk1, sk2}, keyOptions{excludeLabelTail: true})
	}
	if err != nil {
		return err
	}
	names.set("#pk", ix.PK.Field)
	values[":pk"] = keys.PK
	sk := func(i int) any {
		if i < len(keys.SK) {
			return keys.SK[i]
		}
		return Undefined
	}

	condition := "#pk = :pk"
	inclusive := func(is bool) {
		v := sk(0)
		if ix.HasSK && (isNumber(v) || (isString(v) && v.(string) != "")) {
			names.set("#sk1", ix.SK.Field)
			values[":sk1"] = v
			if is {
				condition += " and #sk1 = :sk1"
			} else {
				condition += " and begins_with(#sk1, :sk1)"
			}
		}
	}
	switch q.typ {
	case QueryIs:
		fulfilled := len(q.provided) == len(ix.SK.Composites)
		for _, n := range ix.SK.Composites {
			if !slices.Contains(q.provided, n) {
				fulfilled = false
			}
		}
		inclusive(fulfilled)
	case QueryBegins:
		inclusive(false)
	case QueryBetween:
		names.set("#sk1", ix.SK.Field)
		values[":sk1"] = sk(0)
		values[":sk2"] = sk(1)
		condition += " and #sk1 BETWEEN :sk1 AND :sk2"
	case QueryGt, QueryGte, QueryLt, QueryLte:
		op := map[QueryType]string{QueryGt: ">", QueryGte: ">=", QueryLt: "<", QueryLte: "<="}[q.typ]
		names.set("#sk1", ix.SK.Field)
		values[":sk1"] = sk(0)
		condition += " and #sk1 " + op + " :sk1"
	}
	r.Params["KeyConditionExpression"] = condition
	if f := q.filter.expression; f != "" {
		r.Params["FilterExpression"] = f
	}
	return nil
}

func isNumber(v any) bool { _, ok := v.(Number); return ok }
func isString(v any) bool { _, ok := v.(string); return ok }

func (q *Query) buildScan(r *QueryRequest, names *orderedNames, values map[string]any) error {
	ix := q.ix
	keys, err := indexKeys(q.m, ix, Attrs{}, []Attrs{{}}, keyOptions{})
	if err != nil {
		return err
	}
	var filters []string
	add := func(field string, v any) {
		if v == nil || isUndefined(v) || v == "" {
			return
		}
		names.set("#"+field, field)
		values[":"+field] = v
		filters = append(filters, "begins_with(#"+field+", :"+field+")")
	}
	add(ix.PK.Field, keys.PK)
	if ix.HasSK && len(keys.SK) > 0 {
		add(ix.SK.Field, keys.SK[0])
	}
	if f := q.filter.expression; f != "" {
		filters = append(filters, f)
	}
	if len(filters) > 0 {
		r.Params["FilterExpression"] = strings.Join(filters, " AND ")
	}
	return nil
}

// applyProjection mirrors _applyProjectionExpressions.
func (q *Query) applyProjection(r *QueryRequest, names *orderedNames, opt QueryOptions) {
	if len(opt.Attributes) == 0 {
		return
	}
	table := q.m.TableIndex()
	seen := map[string]string{}
	var order []string
	inOrder := map[string]bool{}
	set := func(k, v string) {
		if !inOrder[k] {
			inOrder[k] = true
			order = append(order, k)
		}
		seen[k] = v
	}
	hasPK, hasSK := false, !table.HasSK
	for _, k := range names.keys {
		v := names.m[k]
		set(strings.TrimPrefix(k, "#"), v)
		if v == table.PK.Field {
			hasPK = true
		}
		if table.HasSK && v == table.SK.Field {
			hasSK = true
		}
	}
	if !hasPK {
		set(formatExpressionName(table.PK.Field, seen), table.PK.Field)
	}
	if !hasSK {
		set(formatExpressionName(table.SK.Field, seen), table.SK.Field)
	}
	for _, name := range opt.Attributes {
		field := name
		if a, ok := q.m.Attribute(name); ok {
			field = a.Field
		}
		set(formatExpressionName(field, seen), field)
	}
	if !opt.Raw && opt.Exec && !opt.IgnoreOwnership {
		set(q.m.Identifiers.Entity, q.m.Identifiers.Entity)
		set(q.m.Identifiers.Version, q.m.Identifiers.Version)
	}
	refs := make([]string, len(order))
	for i, k := range order {
		names.set("#"+k, seen[k])
		refs[i] = "#" + k
	}
	r.Params["ProjectionExpression"] = strings.Join(refs, ", ")
}
