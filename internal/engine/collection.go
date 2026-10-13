package engine

import (
	"fmt"
	"strings"

	"github.com/outboundani/wattdb/internal/model"
)

// This file ports service collections (service.js _makeCollectionChain,
// the collection clauses and cleanseRetrievedData).

// CollectionMember is one entity of a collection, under its service alias.
type CollectionMember struct {
	Alias string
	Model *model.Model
}

type collection struct {
	name    string
	members []CollectionMember
	// sk is the collection's sort-key prefix (_getCollectionSk).
	sk string
}

// Query types of collection queries.
const (
	queryCollection QueryType = 200 + iota
	queryClusteredCollection
)

// NewCollection starts a collection query. The members are in service
// order; the first one's index builds the keys.
func NewCollection(name string, members []CollectionMember, facets []Entry) *Query {
	if len(members) == 0 {
		return &Query{err: fmt.Errorf("collection %q has no entities", name)}
	}
	m := members[0].Model
	ix, path, ok := m.CollectionIndex(name)
	if !ok {
		return &Query{err: fmt.Errorf("unknown collection %q", name)}
	}
	models := make([]*model.Model, len(members))
	for i, mem := range members {
		models[i] = mem.Model
	}
	q := &Query{
		m: m, ix: ix, filter: newFilterExpr(), given: facets,
		filterModel: model.Merged(models...),
		coll:        &collection{name: name, members: members},
	}
	if !ix.SK.Custom {
		q.coll.sk = model.FormatCasing("$"+strings.Join(path, "#"), ix.SK.Casing)
	}
	pk, err := expectFacets(entriesToAttrs(facets), ix.PK.Composites)
	if err != nil {
		return q.fail(err)
	}
	q.pk = pk
	switch ix.Type {
	case model.Isolated, "":
		q.typ = queryCollection
	default:
		q.typ = queryClusteredCollection
		q.setSK(queryClusteredCollection, q.skComposites(facets))
	}
	return q
}

// collectionFilter mirrors the identifiers custom expression of collection
// queries: one identifier check per member, ORed.
func (q *Query) collectionFilter() (*exprState, string) {
	es := newExprState("_c")
	parts := make([]string, len(q.coll.members))
	for i, mem := range q.coll.members {
		parts[i] = identifierExpr(mem.Model, es, mem.Alias)
	}
	expr := strings.Join(parts, " OR ")
	if len(parts) > 1 {
		expr = "(" + expr + ")"
	}
	return es, expr
}

// IsCollection reports whether the request is a collection query.
func (r *QueryRequest) IsCollection() bool { return r.q.coll != nil }

// Members returns a collection query's members.
func (r *QueryRequest) Members() []CollectionMember { return r.q.coll.members }

// memberOf mirrors matchToEntityAlias for collection items: the first member
// whose identifiers match, or that otherwise owns the item; -1 if none.
func (r *QueryRequest) memberOf(item map[string]any, ignore, hydrate bool) int {
	for i, mem := range r.q.coll.members {
		m := mem.Model
		e, eok := item[m.Identifiers.Entity]
		v, vok := item[m.Identifiers.Version]
		if eok && vok && e == m.Entity && v == m.Version {
			return i
		}
		if ownsAs(m, item, ignore, hydrate, r.opts.Attributes) {
			return i
		}
	}
	return -1
}

func ownsAs(m *model.Model, item map[string]any, ignore, hydrate bool, attributes []string) bool {
	if ignore && len(attributes) > 0 && !itemIncludesKeys(m, item) {
		return true
	}
	if (ignore || hydrate) && ownsKeys(m, item) {
		return true
	}
	return OwnsItem(m, item)
}

// FormatGroups mirrors cleanseRetrievedData: one page's items, grouped by
// member. With hydrate, items are kept as they are, for hydration.
func (r *QueryRequest) FormatGroups(items []map[string]any) ([][]map[string]any, error) {
	hydrate := r.opts.Hydrate
	ignore := r.ignoresOwnership() || hydrate
	opt := FormatOptions{IncludeKeys: r.opts.IncludeKeys || hydrate, Attributes: r.opts.Attributes}
	groups := make([][]map[string]any, len(r.q.coll.members))
	for i := range groups {
		groups[i] = []map[string]any{}
	}
	for _, item := range items {
		if item == nil {
			continue
		}
		i := r.memberOf(item, ignore, hydrate)
		if i < 0 {
			continue
		}
		if hydrate {
			groups[i] = append(groups[i], item)
			continue
		}
		m := r.q.coll.members[i].Model
		var data map[string]any
		if ownsAs(m, item, ignore, hydrate, r.opts.Attributes) {
			var err error
			if data, err = FormatItem(m, item, opt); err != nil {
				return nil, err
			}
		}
		groups[i] = append(groups[i], data)
	}
	return groups, nil
}

// runCollection mirrors executeQuery for collection queries: pages are
// merged per member; Count doesn't limit them.
func (r *QueryRequest) runCollection(start map[string]any, opt RunOptions, run Runner) (RunResult, error) {
	res := RunResult{Groups: make([][]map[string]any, len(r.q.coll.members))}
	for i := range res.Groups {
		res.Groups[i] = []map[string]any{}
	}
	esk := start
	iterations := 0
	for {
		items, lek, err := run.Page(r.WithStartKey(esk))
		if err != nil {
			return res, err
		}
		esk = lek
		groups, err := r.FormatGroups(items)
		if err != nil {
			return res, err
		}
		for i, g := range groups {
			if r.opts.Hydrate && len(g) > 0 {
				data, unprocessed, err := r.hydrateFor(r.q.coll.members[i].Model, g, opt, run)
				if err != nil {
					return res, err
				}
				g = data
				res.Unprocessed = append(res.Unprocessed, unprocessed...)
			}
			res.Groups[i] = append(res.Groups[i], g...)
		}
		iterations++
		if !(len(esk) > 0 && (opt.Pages == 0 || opt.Count > 0 || iterations < opt.Pages)) {
			break
		}
	}
	res.Next = esk
	return res, nil
}
