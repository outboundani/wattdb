package engine

import (
	"slices"
	"strings"
	"unicode/utf16"

	"github.com/outboundani/wattdb/internal/model"
)

// Attrs maps attribute names to engine values. A missing name is JS's
// undefined.
type Attrs = map[string]any

type keyOptions struct {
	excludeLabelTail bool
	excludePostfix   bool
}

// makeKey mirrors ElectroDB's _makeKey. The key is a string, a Number (for
// numeric cast keys) or nil when a numeric key has no value.
func makeKey(m *model.Model, k *model.Key, supplied Attrs, opt keyOptions) (key any, fulfilled bool, err error) {
	if k.Cast == model.CastNumber {
		var name string
		if len(k.Composites) > 0 {
			name = k.Composites[0]
		}
		return numericCastKey(name, supplied)
	}

	var b strings.Builder
	b.WriteString(k.Prefix)
	found := 0
	for _, l := range k.Labels {
		value, ok := supplied[l.Name]
		if !ok && opt.excludeLabelTail {
			break
		}
		if k.Custom {
			b.WriteString(l.Label)
		} else {
			b.WriteString("#")
			b.WriteString(l.Label)
			b.WriteString("_")
		}
		if !ok {
			break
		}
		found++
		s := keyString(value)
		if a, ok := m.Attribute(l.Name); ok {
			s = pad(s, a.Padding)
		}
		b.WriteString(s)
	}

	fulfilled = found == len(k.Labels)
	if fulfilled && k.HasPostfix && !opt.excludePostfix {
		b.WriteString(k.Postfix)
	}
	return model.FormatCasing(b.String(), k.Casing), fulfilled, nil
}

// numericCastKey mirrors ElectroDB's _formatNumericCastKey.
func numericCastKey(name string, supplied Attrs) (any, bool, error) {
	value, ok := supplied[name]
	if !ok {
		return nil, false, nil
	}
	switch v := value.(type) {
	case Number:
		return v, true, nil
	case string:
		if n, ok := jsParseInt(v); ok {
			return n, true, nil
		}
	case bool:
		if v {
			return Number("1"), true, nil
		}
		return Number("0"), true, nil
	}
	return nil, false, &ValidationError{Fields: []FieldError{{
		Path:   name,
		Reason: "could not cast composite attribute to number for index",
	}}}
}

// pad mirrors String.prototype.padStart, measuring in UTF-16 code units.
func pad(s string, p *model.Padding) string {
	if p == nil || p.Length <= 0 || p.Char == "" {
		return s
	}
	n := p.Length - len(utf16.Encode([]rune(s)))
	if n <= 0 {
		return s
	}
	fill := utf16.Encode([]rune(p.Char))
	out := make([]uint16, 0, n)
	for len(out) < n {
		out = append(out, fill...)
	}
	return string(utf16.Decode(out[:n])) + s
}

// IndexKeys mirrors ElectroDB's _makeIndexKeys: the pk, one sk per entry in
// sks, and whether any sk was fulfilled.
type IndexKeys struct {
	PK        any
	SK        []any
	Fulfilled bool
}

func indexKeys(m *model.Model, ix *model.Index, pk Attrs, sks []Attrs, opt keyOptions) (IndexKeys, error) {
	if len(sks) == 0 {
		sks = []Attrs{{}}
	}
	var out IndexKeys
	pkOpt := keyOptions{}
	key, _, err := makeKey(m, &ix.PK, pk, pkOpt)
	if err != nil {
		return out, err
	}
	out.PK = key
	if ix.HasSK {
		for _, sk := range sks {
			key, fulfilled, err := makeKey(m, &ix.SK, sk, opt)
			if err != nil {
				return out, err
			}
			if key != nil {
				out.SK = append(out.SK, key)
			}
			if fulfilled {
				out.Fulfilled = true
			}
		}
	}
	return out, nil
}

// PrimaryKey builds the table key used by get, delete and update, as
// ElectroDB's _makeSimpleIndexParams does. Every primary composite must be
// supplied.
func PrimaryKey(m *model.Model, attrs Attrs) (map[string]any, error) {
	ix := m.TableIndex()
	var missing []string
	for _, name := range ix.Composites() {
		if _, ok := attrs[name]; !ok {
			missing = append(missing, name)
		}
	}
	if len(missing) > 0 {
		return nil, &IncompleteCompositesError{Missing: missing}
	}
	keys, err := indexKeys(m, ix, attrs, []Attrs{attrs}, keyOptions{})
	if err != nil {
		return nil, err
	}
	out := map[string]any{ix.PK.Field: keys.PK}
	if ix.HasSK && len(keys.SK) > 0 {
		out[ix.SK.Field] = keys.SK[0]
	}
	return out, nil
}

// QueryType is the sort-key operation of a query.
type QueryType int

const (
	// QueryIs is a plain access-pattern query, with no sort-key operation.
	QueryIs QueryType = iota
	QueryBegins
	QueryBetween
	QueryGt
	QueryGte
	QueryLt
	QueryLte
)

// QueryKeys builds a query's key values, as ElectroDB's _makeQueryKeys does.
// sks holds the sort-key composites for each key condition: one map, or two
// for between (start and end). Each must already include the composites
// passed to the access pattern.
func QueryKeys(m *model.Model, ix *model.Index, typ QueryType, pk Attrs, sks ...Attrs) (IndexKeys, error) {
	var missing []string
	for _, name := range ix.PK.Composites {
		if _, ok := pk[name]; !ok {
			missing = append(missing, name)
		}
	}
	if len(missing) > 0 {
		return IndexKeys{}, &IncompleteCompositesError{AccessPatterns: []string{ix.AccessPattern}, Missing: missing}
	}
	opt := keyOptions{excludeLabelTail: typ != QueryIs}
	return indexKeys(m, ix, pk, sks, opt)
}

// PutKeys returns the key fields a put or create writes, mirroring
// ElectroDB's _getPutKeys. set holds the attribute values after defaults and
// setters; primary holds the primary-key composites as passed to put.
func PutKeys(m *model.Model, set, primary Attrs) (map[string]any, error) {
	keys, _, err := putKeys(m, set, primary)
	return keys, err
}

// putKeys mirrors _getPutKeys, also returning the key fields a sparse index
// condition removed (used by upsert).
func putKeys(m *model.Model, set, primary Attrs) (map[string]any, []string, error) {
	imp, err := expectIndexFacets(m, set, primary, impactOptions{})
	if err != nil {
		return nil, nil, err
	}
	var deleted []string
	for _, ix := range m.Indexes {
		if keep, ok := imp.conditions[ix]; ok && !keep {
			deleted = append(deleted, ix.PK.Field)
			if ix.HasSK {
				deleted = append(deleted, ix.SK.Field)
			}
		}
	}
	completed := imp.completed
	table := m.TableIndex()
	if !slices.Contains(completed, table) {
		completed = append(completed, table)
	}

	merged := mergeAttrs(primary, set)
	out := map[string]any{}
	for _, ix := range completed {
		if ix.Condition != nil {
			keep, err := ix.Condition(merged)
			if err != nil {
				return nil, nil, err
			}
			if !keep {
				continue
			}
		}
		keys, err := indexKeys(m, ix, merged, []Attrs{merged}, keyOptions{})
		if err != nil {
			return nil, nil, err
		}
		if keys.PK != nil && keys.PK != "" {
			out[ix.PK.Field] = keys.PK
		}
		if ix.HasSK && len(keys.SK) > 0 && keys.SK[0] != nil && keys.SK[0] != "" {
			out[ix.SK.Field] = keys.SK[0]
		}
	}
	return out, deleted, nil
}

// impactOptions mirrors the options of ElectroDB's _getIndexImpact.
type impactOptions struct {
	// includedOnly also counts sparse indexes whose composites are all in
	// included (update "set").
	includedOnly bool
	// skipConditions treats every index as unconditional (update "remove").
	skipConditions bool
}

// impact is the result of indexImpact.
type impact struct {
	// completed lists indexes whose impacted keys are complete.
	completed []*model.Index
	// types maps each impacted index to its impacted key fields; order
	// lists those indexes in the order ElectroDB first saw them.
	types map[*model.Index]map[model.KeyType]string
	order []*model.Index
	// conditions holds the evaluated sparse-index conditions.
	conditions map[*model.Index]bool
	// incomplete lists the indexes left half-built.
	incomplete []incompleteIndex
}

type incompleteIndex struct {
	index   *model.Index
	missing []string
}

// indexImpact mirrors ElectroDB's _getIndexImpact.
func indexImpact(m *model.Model, attrs, included Attrs, opt impactOptions) (impact, error) {
	type keyImpact struct {
		names    []string
		provided bool
	}
	impacted := map[*model.Index]map[model.KeyType]*keyImpact{}
	out := impact{types: map[*model.Index]map[model.KeyType]string{}, conditions: map[*model.Index]bool{}}
	mark := func(ix *model.Index, kt model.KeyType, names []string, provided bool) {
		if impacted[ix] == nil {
			impacted[ix] = map[model.KeyType]*keyImpact{}
		}
		ki := impacted[ix][kt]
		if ki == nil {
			ki = &keyImpact{}
			impacted[ix][kt] = ki
		}
		ki.names = append(ki.names, names...)
		ki.provided = ki.provided || provided
		if out.types[ix] == nil {
			out.types[ix] = map[model.KeyType]string{}
			out.order = append(out.order, ix)
		}
		if kt == model.PK {
			out.types[ix][kt] = ix.PK.Field
		} else {
			out.types[ix][kt] = ix.SK.Field
		}
	}
	for _, name := range m.FacetAttributes() {
		if _, ok := attrs[name]; !ok {
			continue
		}
		for _, f := range m.Facets(name) {
			if f.Index.Type == model.Composite {
				continue
			}
			mark(f.Index, f.Type, []string{name}, true)
		}
	}

	if opt.includedOnly {
		for _, ix := range m.Indexes {
			if ix.Type == model.Composite || ix.Name == "" || ix.Condition == nil {
				continue
			}
			for _, kt := range []model.KeyType{model.PK, model.SK} {
				k := &ix.PK
				if kt == model.SK {
					k = &ix.SK
				}
				if len(k.Composites) == 0 || !allIn(k.Composites, included) {
					continue
				}
				if kt == model.SK && !ix.HasSK {
					continue
				}
				if impacted[ix] == nil {
					impacted[ix] = map[model.KeyType]*keyImpact{}
				}
				// Replace the impacted list with the full composite list, as
				// ElectroDB does, keeping any "provided" source.
				prev := impacted[ix][kt]
				provided := prev != nil && prev.provided
				impacted[ix][kt] = &keyImpact{names: append([]string{}, k.Composites...), provided: provided}
				if out.types[ix] == nil {
					out.types[ix] = map[model.KeyType]string{}
					out.order = append(out.order, ix)
				}
				out.types[ix][kt] = k.Field
			}
		}
	}

	type pending struct {
		ix      *model.Index
		missing []string
	}
	var all []pending
	for _, ix := range m.Indexes {
		if ix.Type == model.Composite {
			continue
		}
		p := pending{ix: ix}
		if imp := impacted[ix]; imp != nil {
			pkImp, skImp := imp[model.PK], imp[model.SK]
			missingPK := pkImp != nil && len(pkImp.names) != len(ix.PK.Composites)
			missingSK := skImp != nil && len(skImp.names) != len(ix.SK.Composites)
			if missingPK {
				p.missing = append(p.missing, notIn(ix.PK.Composites, pkImp.names, included)...)
			}
			if missingSK {
				p.missing = append(p.missing, notIn(ix.SK.Composites, skImp.names, included)...)
			}
			if !missingPK && !missingSK {
				out.completed = append(out.completed, ix)
			}
		}
		all = append(all, p)
	}

	for _, p := range all {
		ix := p.ix
		if opt.skipConditions || ix.Name == "" || ix.Condition == nil {
			if len(p.missing) > 0 {
				out.incomplete = append(out.incomplete, incompleteIndex{ix, p.missing})
			}
			out.conditions[ix] = true
			continue
		}
		imp := impacted[ix]
		memberProvided := imp != nil && ((imp[model.PK] != nil && imp[model.PK].provided) || (imp[model.SK] != nil && imp[model.SK].provided))
		allIncluded := allIn(ix.Composites(), included)
		if memberProvided || allIncluded {
			var absent []string
			for _, name := range ix.Composites() {
				_, inAttrs := attrs[name]
				_, inIncluded := included[name]
				if (!inAttrs && !inIncluded) || slices.Contains(p.missing, name) {
					absent = append(absent, name)
				}
			}
			if len(absent) > 0 {
				return out, &IncompleteCompositesError{AccessPatterns: []string{ix.AccessPattern}, Missing: absent, Conditional: true}
			}
			merged := mergeAttrs(attrs, included)
			keep, err := ix.Condition(merged)
			if err != nil {
				return out, err
			}
			out.conditions[ix] = keep
		} else if len(p.missing) > 0 {
			out.incomplete = append(out.incomplete, incompleteIndex{ix, p.missing})
		}
	}
	return out, nil
}

// expectIndexFacets mirrors ElectroDB's _expectIndexFacets.
func expectIndexFacets(m *model.Model, attrs, included Attrs, opt impactOptions) (impact, error) {
	imp, err := indexImpact(m, attrs, included, opt)
	if err != nil {
		return imp, err
	}
	if len(imp.incomplete) > 0 {
		e := &IncompleteCompositesError{}
		for _, inc := range imp.incomplete {
			e.AccessPatterns = append(e.AccessPatterns, inc.index.AccessPattern)
			e.Missing = append(e.Missing, inc.missing...)
		}
		return imp, e
	}
	return imp, nil
}

func allIn(names []string, attrs Attrs) bool {
	for _, n := range names {
		if _, ok := attrs[n]; !ok {
			return false
		}
	}
	return true
}

func mergeAttrs(maps ...Attrs) Attrs {
	out := Attrs{}
	for _, m := range maps {
		for k, v := range m {
			out[k] = v
		}
	}
	return out
}

// notIn returns the composites neither impacted nor included.
func notIn(composites, impacted []string, included Attrs) []string {
	var out []string
	for _, name := range composites {
		if slices.Contains(impacted, name) {
			continue
		}
		if _, ok := included[name]; ok {
			continue
		}
		out = append(out, name)
	}
	return out
}

// jsParseInt mirrors JS parseInt(s) with no radix, returning the integer's
// decimal text.
func jsParseInt(s string) (Number, bool) {
	s = strings.TrimLeft(s, " \t\n\v\f\r\u00a0\ufeff\u2028\u2029")
	neg := false
	if s != "" && (s[0] == '+' || s[0] == '-') {
		neg = s[0] == '-'
		s = s[1:]
	}
	base := 10
	if len(s) >= 2 && s[0] == '0' && (s[1] == 'x' || s[1] == 'X') {
		base = 16
		s = s[2:]
	}
	end := 0
	for end < len(s) && digitValue(s[end]) < base {
		end++
	}
	if end == 0 {
		return "", false
	}
	digits := s[:end]
	if base == 16 {
		// Hex values are converted through float64, as JS does.
		var f float64
		for i := 0; i < len(digits); i++ {
			f = f*16 + float64(digitValue(digits[i]))
		}
		if neg {
			f = -f
		}
		return FloatNumber(f), true
	}
	digits = strings.TrimLeft(digits, "0")
	if digits == "" {
		return Number("0"), true
	}
	if neg {
		digits = "-" + digits
	}
	return Number(digits), true
}

func digitValue(c byte) int {
	switch {
	case c >= '0' && c <= '9':
		return int(c - '0')
	case c >= 'a' && c <= 'z':
		return int(c-'a') + 10
	case c >= 'A' && c <= 'Z':
		return int(c-'A') + 10
	default:
		return 99
	}
}
