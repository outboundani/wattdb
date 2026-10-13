package engine

import (
	"fmt"
	"slices"
	"strconv"
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
	// coll is set for collection queries; filterModel then merges the
	// members' attributes for filters.
	coll        *collection
	filterModel *model.Model
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
	m := q.m
	if q.filterModel != nil {
		m = q.filterModel
	}
	w := &Write{m: m, cond: q.filter}
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
			if q.coll == nil && q.ix.Type == model.Clustered && found < len(q.ix.SK.Composites) && !opt.IgnoreOwnership {
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
	custom := ""
	if q.coll != nil && !opt.IgnoreOwnership {
		es, expr := q.collectionFilter()
		custom = expr
		for _, k := range es.nameOrder {
			names.set(k, es.names[k])
		}
		for k, v := range es.values {
			values[k] = v
		}
	}
	var err error
	if q.scan {
		r.Target = "Scan"
		err = q.buildScan(r, names, values)
	} else if q.ix.Type == model.Composite {
		r.Target = "Query"
		err = q.buildComposite(r, names, values, opt, custom)
	} else {
		r.Target = "Query"
		err = q.buildQuery(r, names, values, custom)
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

func (q *Query) buildQuery(r *QueryRequest, names *orderedNames, values map[string]any, custom string) error {
	ix := q.ix
	sk1, sk2 := q.consolidate()
	var keys IndexKeys
	var err error
	if q.typ == QueryIs {
		keys, err = indexKeys(q.m, ix, q.pk, []Attrs{sk1, sk2}, keyOptions{})
	} else {
		keys, err = indexKeys(q.m, ix, q.pk, []Attrs{sk1, sk2}, keyOptions{
			excludeLabelTail: true,
			excludePostfix:   q.coll != nil && ix.Type == model.Clustered,
		})
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
		if q.typ == queryCollection {
			v = q.coll.sk
		}
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
	case QueryBegins, queryCollection, queryClusteredCollection:
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
	if f := joinFilters(custom, q.filter.expression); f != "" {
		r.Params["FilterExpression"] = f
	}
	return nil
}

func joinFilters(exprs ...string) string {
	var out []string
	for _, e := range exprs {
		if e = strings.TrimSpace(e); e != "" {
			out = append(out, e)
		}
	}
	return strings.Join(out, " AND ")
}

// buildComposite mirrors _compositeQueryParams: key conditions on the
// composite attributes themselves.
func (q *Query) buildComposite(r *QueryRequest, names *orderedNames, values map[string]any, opt QueryOptions, custom string) error {
	ix := q.ix
	es := newExprState("k_")
	var exprs []string
	field := func(name string) string {
		if a, ok := q.m.Attribute(name); ok {
			return a.Field
		}
		return name
	}
	eq := func(name string, v any) {
		ref := es.setName(nil, name, field(name)).expression
		exprs = append(exprs, ref+" = "+es.setValue(name, v))
	}
	if q.typ == QueryBetween {
		for _, name := range ix.PK.Composites {
			eq(name, q.pk[name])
		}
		is, start, end := Attrs{}, Attrs{}, Attrs{}
		for _, e := range q.sk {
			switch e.typ {
			case QueryIs:
				is = entriesToAttrs(e.facets)
			case QueryBetween:
				start = entriesToAttrs(e.facets)
			case queryAnd:
				end = entriesToAttrs(e.facets)
			}
		}
		last := ""
		for _, name := range ix.SK.Composites {
			_, inIs := is[name]
			_, inStart := start[name]
			_, inEnd := end[name]
			switch {
			case inIs, inStart && inEnd:
				last = name
			case inStart || inEnd:
				return fmt.Errorf("between on a composite index needs both a start and an end value for %s", name)
			}
			if !inIs && !inStart && !inEnd {
				break
			}
		}
		for _, name := range ix.SK.Composites {
			_, inIs := is[name]
			_, inStart := start[name]
			_, inEnd := end[name]
			switch {
			case name == last:
				ref := es.setName(nil, name, field(name)).expression
				lo := es.setValue(name, start[name])
				hi := es.setValue(name, end[name])
				exprs = append(exprs, ref+" BETWEEN "+lo+" AND "+hi)
			case inIs:
				eq(name, is[name])
			case inStart && inEnd:
				if !sameValue(start[name], end[name]) {
					return fmt.Errorf("between on a composite index needs equal start and end values for %s until the last sort key attribute", name)
				}
				eq(name, start[name])
			}
		}
	} else {
		sk1, _ := q.consolidate()
		type part struct {
			sk    bool
			name  string
			value any
		}
		var parts []part
		for i, name := range ix.Composites() {
			isSK := i >= len(ix.PK.Composites)
			v, ok := q.pk[name]
			if isSK {
				v, ok = sk1[name]
			}
			if !ok {
				break
			}
			parts = append(parts, part{isSK, name, v})
		}
		for i, p := range parts {
			if !p.sk || i < len(parts)-1 {
				eq(p.name, p.value)
				continue
			}
			ref := es.setName(nil, p.name, field(p.name)).expression
			val := es.setValue(p.name, p.value)
			switch q.typ {
			case QueryIs, queryCollection, queryClusteredCollection:
				exprs = append(exprs, ref+" = "+val)
			case QueryBegins:
				exprs = append(exprs, "begins_with("+ref+", "+val+")")
			case QueryGt, QueryGte, QueryLt, QueryLte:
				op := map[QueryType]string{QueryGt: ">", QueryGte: ">=", QueryLt: "<", QueryLte: "<="}[q.typ]
				exprs = append(exprs, ref+" "+op+" "+val)
			}
		}
	}
	identifiers := ""
	if !opt.IgnoreOwnership && custom == "" {
		identifiers = identifierExpr(q.m, es, "")
	}
	for _, k := range es.nameOrder {
		names.set(k, es.names[k])
	}
	for k, v := range es.values {
		values[k] = v
	}
	r.Params["KeyConditionExpression"] = strings.Join(exprs, " AND ")
	if f := joinFilters(custom, q.filter.expression, identifiers); f != "" {
		r.Params["FilterExpression"] = f
	}
	return nil
}

// identifierExpr mirrors applyIdentifierExpressionState.
func identifierExpr(m *model.Model, es *exprState, alias string) string {
	if alias == "" {
		alias = m.Entity
	}
	e := es.setName(nil, m.Identifiers.Entity, m.Identifiers.Entity).expression
	v := es.setName(nil, m.Identifiers.Version, m.Identifiers.Version).expression
	ev := es.setValue(m.Identifiers.Entity+"_"+alias, m.Entity)
	vv := es.setValue(m.Identifiers.Version+"_"+alias, m.Version)
	return "(" + e + " = " + ev + " AND " + v + " = " + vv + ")"
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
	r.Params["ProjectionExpression"] = projection(q.m, names, opt.Attributes, !opt.Raw && opt.Exec && !opt.IgnoreOwnership)
}

// projection renders a ProjectionExpression over the existing names, the
// table key fields, the attributes and optionally the identifiers.
func projection(m *model.Model, names *orderedNames, attributes []string, identifiers bool) string {
	table := m.TableIndex()
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
	for _, name := range attributes {
		field := name
		if a, ok := m.Attribute(name); ok {
			field = a.Field
		}
		set(formatExpressionName(field, seen), field)
	}
	if identifiers {
		set(m.Identifiers.Entity, m.Identifiers.Entity)
		set(m.Identifiers.Version, m.Identifiers.Version)
	}
	refs := make([]string, len(order))
	for i, k := range order {
		names.set("#"+k, seen[k])
		refs[i] = "#" + k
	}
	return strings.Join(refs, ", ")
}

// sameValue mirrors JS strict equality on composite values.
func sameValue(a, b any) bool {
	an, aok := a.(Number)
	bn, bok := b.(Number)
	if aok && bok {
		x, _ := strconv.ParseFloat(string(an), 64)
		y, _ := strconv.ParseFloat(string(bn), 64)
		return x == y
	}
	return a == b
}
