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
	completed, err := putImpact(m, set, primary)
	if err != nil {
		return nil, err
	}
	table := m.TableIndex()
	if !slices.Contains(completed, table) {
		completed = append(completed, table)
	}

	merged := make(Attrs, len(set)+len(primary))
	for k, v := range primary {
		merged[k] = v
	}
	for k, v := range set {
		merged[k] = v
	}

	out := map[string]any{}
	for _, ix := range completed {
		if ix.Condition != nil {
			keep, err := ix.Condition(merged)
			if err != nil {
				return nil, err
			}
			if !keep {
				continue
			}
		}
		keys, err := indexKeys(m, ix, merged, []Attrs{merged}, keyOptions{})
		if err != nil {
			return nil, err
		}
		if keys.PK != nil && keys.PK != "" {
			out[ix.PK.Field] = keys.PK
		}
		if ix.HasSK && len(keys.SK) > 0 && keys.SK[0] != nil && keys.SK[0] != "" {
			out[ix.SK.Field] = keys.SK[0]
		}
	}
	return out, nil
}

// putImpact mirrors ElectroDB's _getIndexImpact as put uses it. It returns
// the indexes whose provided composites are complete, in index order.
func putImpact(m *model.Model, attrs, included Attrs) ([]*model.Index, error) {
	type impact struct {
		pk, sk       []string
		pkSet, skSet bool
	}
	impacted := map[*model.Index]*impact{}
	for _, name := range m.FacetAttributes() {
		if _, ok := attrs[name]; !ok {
			continue
		}
		for _, f := range m.Facets(name) {
			if f.Index.Type == model.Composite {
				continue
			}
			imp := impacted[f.Index]
			if imp == nil {
				imp = &impact{}
				impacted[f.Index] = imp
			}
			if f.Type == model.PK {
				imp.pk, imp.pkSet = append(imp.pk, name), true
			} else {
				imp.sk, imp.skSet = append(imp.sk, name), true
			}
		}
	}

	var completed []*model.Index
	var incomplete []string
	var incompletePatterns []string
	for _, ix := range m.Indexes {
		if ix.Type == model.Composite {
			continue
		}
		var missing []string
		if imp := impacted[ix]; imp != nil {
			missingPK := imp.pkSet && len(imp.pk) != len(ix.PK.Composites)
			missingSK := imp.skSet && len(imp.sk) != len(ix.SK.Composites)
			if missingPK {
				missing = append(missing, notIn(ix.PK.Composites, imp.pk, included)...)
			}
			if missingSK {
				missing = append(missing, notIn(ix.SK.Composites, imp.sk, included)...)
			}
			if !missingPK && !missingSK {
				completed = append(completed, ix)
			}
		}

		if ix.Name == "" || ix.Condition == nil {
			if len(missing) > 0 {
				incomplete = append(incomplete, missing...)
				incompletePatterns = append(incompletePatterns, ix.AccessPattern)
			}
			continue
		}

		allIncluded := true
		for _, name := range ix.Composites() {
			if _, ok := included[name]; !ok {
				allIncluded = false
			}
		}
		if impacted[ix] == nil && !allIncluded {
			if len(missing) > 0 {
				incomplete = append(incomplete, missing...)
				incompletePatterns = append(incompletePatterns, ix.AccessPattern)
			}
			continue
		}

		var absent []string
		for _, name := range ix.Composites() {
			_, inAttrs := attrs[name]
			_, inIncluded := included[name]
			if (!inAttrs && !inIncluded) || slices.Contains(missing, name) {
				absent = append(absent, name)
			}
		}
		if len(absent) > 0 {
			return nil, &IncompleteCompositesError{AccessPatterns: []string{ix.AccessPattern}, Missing: absent}
		}
		// ElectroDB evaluates the condition here as well; PutKeys evaluates it
		// again before building keys, so the result isn't needed.
	}

	if len(incomplete) > 0 {
		return nil, &IncompleteCompositesError{AccessPatterns: incompletePatterns, Missing: incomplete}
	}
	return completed, nil
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
