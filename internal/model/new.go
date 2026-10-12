package model

import (
	"fmt"
	"slices"
	"strings"
)

// New normalizes a Definition the way ElectroDB's _parseModel does, returning
// an error wherever ElectroDB throws on the same model.
func New(def Definition) (*Model, error) {
	if def.Entity == "" || def.Service == "" || def.Version == "" {
		return nil, fmt.Errorf("model requires entity, service and version")
	}
	m := &Model{
		Entity:      def.Entity,
		Version:     def.Version,
		Service:     def.Service,
		Identifiers: DefaultIdentifiers,
		attrs:       map[string]*Attribute{},
		byName:      map[string]*Index{},
		byPattern:   map[string]*Index{},
		facets:      map[string][]Facet{},
	}
	if def.Identifiers.Entity != "" {
		m.Identifiers.Entity = def.Identifiers.Entity
	}
	if def.Identifiers.Version != "" {
		m.Identifiers.Version = def.Identifiers.Version
	}

	for _, ad := range def.Attributes {
		if _, dup := m.attrs[ad.Name]; dup {
			return nil, fmt.Errorf("duplicate attribute %q", ad.Name)
		}
		a := newAttribute(ad)
		m.Attributes = append(m.Attributes, a)
		m.attrs[a.Name] = a
	}

	if err := m.normalizeIndexes(def.Indexes); err != nil {
		return nil, err
	}
	if err := m.checkKeyFieldAttributes(); err != nil {
		return nil, err
	}
	if err := m.makeKeyFixings(); err != nil {
		return nil, err
	}
	m.applyLabels()
	return m, nil
}

func newAttribute(ad AttributeDef) *Attribute {
	a := &Attribute{
		Name:     ad.Name,
		Field:    ad.Field,
		Type:     ad.Type,
		Enum:     ad.Enum,
		Label:    ad.Label,
		Required: ad.Required,
		Hidden:   ad.Hidden,
		ReadOnly: ad.ReadOnly,
		Padding:  ad.Padding,
		SetItems: ad.SetItems,
	}
	if a.Field == "" {
		a.Field = a.Name
	}
	if ad.Items != nil {
		a.Items = newAttribute(*ad.Items)
	}
	for _, p := range ad.Properties {
		a.Properties = append(a.Properties, newAttribute(p))
	}
	return a
}

// keyComposites resolves a key's composites the way ElectroDB's
// _applyCompositeToFacetConversion, _optimizeIndexKey and
// _parseTemplateAttributes do, in that order.
func (m *Model) keyComposites(pattern string, kd KeyDef, kt KeyType) (Key, error) {
	// Cast holds the written value until makeKeyFixings resolves it.
	k := Key{Field: kd.Field, Template: kd.Template, Cast: Cast(kd.Cast)}
	if kd.Composite == nil && kd.Template == nil {
		return k, fmt.Errorf("access pattern %q: %s has no composite or template", pattern, kt)
	}

	template := kd.Template
	if template == nil && len(kd.Composite) == 1 && kd.Composite[0] == kd.Field && m.isAttributeField(kd.Field) {
		// _optimizeIndexKey: an attribute used directly as the key field.
		t := "${" + kd.Field + "}"
		template = &t
	}

	if template != nil && *template != "" {
		k.Custom = true
		k.Labels = parseTemplate(*template)
		for _, l := range k.Labels {
			if l.Name != "" {
				k.Composites = append(k.Composites, l.Name)
			}
		}
		if kd.Template != nil && kd.Composite != nil && !stringArrayMatch(kd.Composite, k.Composites) {
			return k, fmt.Errorf("access pattern %q: %s template attributes %v don't match composite %v", pattern, kt, k.Composites, kd.Composite)
		}
		return k, nil
	}
	if template != nil {
		// An empty template is treated like an empty composite.
		return k, nil
	}
	k.Composites = slices.Clone(kd.Composite)
	for _, name := range k.Composites {
		k.Labels = append(k.Labels, Label{Name: name})
	}
	return k, nil
}

func (m *Model) isAttributeField(field string) bool {
	for _, a := range m.Attributes {
		if a.Field == field {
			return true
		}
	}
	return false
}

// normalizeIndexes mirrors ElectroDB's _normalizeIndexes.
func (m *Model) normalizeIndexes(defs []IndexDef) error {
	collections := map[string]string{}
	type fieldUse struct {
		ix  *Index
		key *Key
	}
	byField := map[string][]fieldUse{}

	for _, d := range defs {
		pattern := d.AccessPattern
		if _, dup := m.byPattern[pattern]; dup {
			return fmt.Errorf("duplicate access pattern %q", pattern)
		}
		ix := &Index{
			AccessPattern: pattern,
			Name:          d.Index,
			Type:          d.Type,
			Scope:         d.Scope,
			Condition:     d.Condition,
			HasSK:         d.SK != nil,
		}
		if ix.Type == "" {
			ix.Type = Isolated
		}
		display := displayIndex(ix.Name)

		if ix.Name == "" && d.Condition != nil {
			return fmt.Errorf("access pattern %q: 'condition' is only allowed on secondary indexes", pattern)
		}
		if ix.Type == Composite {
			switch {
			case ix.Name == "":
				return fmt.Errorf("access pattern %q: the table index can't be a composite index", pattern)
			case d.Condition != nil:
				return fmt.Errorf("access pattern %q: composite indexes don't support a condition", pattern)
			case d.Scope != "":
				return fmt.Errorf("access pattern %q: composite indexes don't support scope", pattern)
			case d.PK.Field != "" || (d.SK != nil && d.SK.Field != ""):
				return fmt.Errorf("access pattern %q: composite indexes don't take a field", pattern)
			}
		} else if d.PK.Field == "" || (d.SK != nil && d.SK.Field == "") {
			return fmt.Errorf("access pattern %q: pk and sk need a field", pattern)
		}
		if _, dup := m.byName[ix.Name]; dup {
			return fmt.Errorf("access pattern %q: duplicate index %s", pattern, display)
		}
		if !ix.HasSK && len(d.Collection) > 0 {
			return fmt.Errorf("access pattern %q: collections require an sk", pattern)
		}

		pk, err := m.keyComposites(pattern, d.PK, PK)
		if err != nil {
			return err
		}
		pk.Casing = resolveCasing(d.PK.Casing)
		ix.PK = pk
		if ix.HasSK {
			sk, err := m.keyComposites(pattern, *d.SK, SK)
			if err != nil {
				return err
			}
			sk.Casing = resolveCasing(d.SK.Casing)
			ix.SK = sk
			if len(sk.Composites) > 1 {
				for _, c := range pk.Composites {
					if slices.Contains(sk.Composites, c) {
						return fmt.Errorf("access pattern %q: composite %q is used in both pk and sk", pattern, c)
					}
				}
			}
		} else {
			ix.SK = Key{Casing: Lower}
		}

		if d.Projection != nil {
			p := *d.Projection
			if len(p.Attributes) == 0 {
				kind := strings.ToLower(p.Kind)
				if kind != "all" && kind != "keys_only" {
					return fmt.Errorf("access pattern %q: invalid projection %q", pattern, p.Kind)
				}
				p.Kind = kind
			}
			ix.Projection = &p
		}

		if len(d.Collection) > 0 {
			ix.Collections = slices.Clone(d.Collection)
			ix.SubCollections = d.CollectionArray
			for _, c := range ix.Collections {
				if c == "" {
					return fmt.Errorf("access pattern %q: invalid collection", pattern)
				}
				if prev, dup := collections[c]; dup {
					return fmt.Errorf("collection %q is defined on both %q and %q", c, prev, pattern)
				}
				collections[c] = pattern
			}
		}

		m.Indexes = append(m.Indexes, ix)
		m.byName[ix.Name] = ix
		m.byPattern[pattern] = ix

		if ix.Type != Composite {
			keys := []*Key{&ix.PK}
			if ix.HasSK {
				if ix.SK.Field == ix.PK.Field {
					return fmt.Errorf("access pattern %q uses field %q for both pk and sk", pattern, ix.PK.Field)
				}
				keys = append(keys, &ix.SK)
			}
			for _, k := range keys {
				for _, use := range byField[k.Field] {
					if use.ix == ix {
						continue
					}
					if !stringArrayMatch(k.Composites, use.key.Composites) {
						return fmt.Errorf("access pattern %q: field %q is defined with composites %v, but %q uses %v", pattern, k.Field, k.Composites, use.ix.AccessPattern, use.key.Composites)
					}
					if !sameTemplate(k.Template, use.key.Template) {
						return fmt.Errorf("access pattern %q: field %q has a different template than on %q", pattern, k.Field, use.ix.AccessPattern)
					}
					break
				}
				byField[k.Field] = append(byField[k.Field], fieldUse{ix, k})
			}
		}

		for _, kt := range []KeyType{PK, SK} {
			k := &ix.PK
			if kt == SK {
				k = &ix.SK
			}
			for _, name := range k.Composites {
				if _, seen := m.facets[name]; !seen {
					m.facetOrder = append(m.facetOrder, name)
				}
				m.facets[name] = append(m.facets[name], Facet{Index: ix, Type: kt})
			}
		}
	}

	if _, ok := m.byName[""]; !ok {
		return fmt.Errorf("model is missing the table index (an access pattern without an index name)")
	}
	return nil
}

// checkKeyFieldAttributes mirrors the checks ElectroDB's Schema makes on
// attributes whose field is also a key field.
func (m *Model) checkKeyFieldAttributes() error {
	for _, a := range m.Attributes {
		for _, ix := range m.Indexes {
			if ix.Type == Composite {
				continue
			}
			for _, k := range []*Key{&ix.PK, &ix.SK} {
				if k.Field == "" || k.Field != a.Field {
					continue
				}
				k.FieldRef = true
				display := displayIndex(ix.Name)
				if len(k.Composites) > 1 {
					return fmt.Errorf("index %s: key field %q is an attribute's field, so it can't have other composites", display, k.Field)
				}
				if !k.Custom {
					return fmt.Errorf("index %s: key field %q is an attribute's field, so it needs a template", display, k.Field)
				}
				if len(k.Labels) > 2 {
					return fmt.Errorf("index %s: key field %q is an attribute's field, so its template can have at most a prefix and a postfix", display, k.Field)
				}
				hasFixings := false
				for _, l := range k.Labels {
					if l.Label != "" {
						hasFixings = true
					}
				}
				if hasFixings && a.Type != TypeString && a.Type != TypeEnum {
					return fmt.Errorf("index %s: key field %q has a prefix or postfix, so attribute %q must be a string or enum", display, k.Field, a.Name)
				}
				if len(ix.Collections) > 0 {
					return fmt.Errorf("index %s: key field %q is an attribute's field, so the index can't be in a collection", display, k.Field)
				}
				if a.Padding != nil {
					return fmt.Errorf("attribute %q: padding isn't supported on attributes used as key fields", a.Name)
				}
			}
		}
	}
	return nil
}

// makeKeyFixings mirrors ElectroDB's _normalizeKeyFixings/_makeKeyFixings.
func (m *Model) makeKeyFixings() error {
	var done []*Index
	for _, ix := range m.Indexes {
		var prevPK, prevSK *Key
		for _, p := range done {
			if p.PK.Field == ix.PK.Field {
				prevPK = &p.PK
			} else if p.HasSK && p.SK.Field == ix.PK.Field {
				prevPK = &p.SK
			}
			if ix.HasSK {
				if p.PK.Field == ix.SK.Field {
					prevSK = &p.PK
				} else if p.HasSK && p.SK.Field == ix.SK.Field {
					prevSK = &p.SK
				}
			}
			if prevPK != nil && (prevSK != nil || !ix.HasSK) {
				break
			}
		}

		entityKeys := ""
		sk := makeCollectionPrefix(ix.Collections)
		if sk != "" {
			entityKeys = "#" + m.Entity
		} else {
			entityKeys = "$" + m.Entity
		}
		entityKeys += "_" + m.Version

		pk := "$" + m.Service
		postfix := ""
		clustered := ix.Type == Clustered
		if clustered {
			postfix = entityKeys
		} else {
			sk += entityKeys
		}
		if !ix.HasSK {
			pk += sk
			if clustered {
				pk += postfix
			}
		}
		if !ix.PK.Custom {
			if ix.Scope != "" {
				pk += "_" + ix.Scope
			}
			ix.PK.Prefix = FormatCasing(pk, ix.PK.Casing)
		}
		if !ix.SK.Custom {
			ix.SK.Prefix = FormatCasing(sk, ix.SK.Casing)
			ix.SK.Postfix = FormatCasing(postfix, ix.SK.Casing)
			ix.SK.HasPostfix = true
		}

		keys := []*Key{&ix.PK}
		if ix.HasSK {
			keys = append(keys, &ix.SK)
		}
		for _, k := range keys {
			cast, err := m.resolveCast(ix, k, string(k.Cast))
			if err != nil {
				return err
			}
			k.Cast = cast
		}

		if prevPK != nil {
			if prevPK.Casing != ix.PK.Casing {
				return fmt.Errorf("access pattern %q: pk field %q has a different casing than on another index", ix.AccessPattern, ix.PK.Field)
			}
			copyFixings(&ix.PK, prevPK)
		}
		if prevSK != nil {
			if prevSK.Casing != ix.SK.Casing {
				return fmt.Errorf("access pattern %q: sk field %q has a different casing than on another index", ix.AccessPattern, ix.SK.Field)
			}
			copyFixings(&ix.SK, prevSK)
		}
		done = append(done, ix)
	}
	return nil
}

func copyFixings(dst, src *Key) {
	dst.Custom = src.Custom
	dst.Casing = src.Casing
	dst.Cast = src.Cast
	dst.Prefix = src.Prefix
	dst.Postfix = src.Postfix
	dst.HasPostfix = src.HasPostfix
}

// resolveCast mirrors the cast rules in _makeKeyFixings.
func (m *Model) resolveCast(ix *Index, k *Key, written string) (Cast, error) {
	switch {
	case written == string(CastString):
		return CastString, nil
	case written == "" && k.Custom && len(k.Composites) == 1 && allLabelsEmpty(k.Labels) && m.attrType(k.Composites[0]) == TypeNumber:
		// Custom keys with one numeric composite and no labels are numeric.
		return CastNumber, nil
	case written == string(CastNumber) && len(k.Composites) == 1 && slices.Contains([]AttrType{TypeNumber, TypeString, TypeBoolean}, m.attrType(k.Composites[0])):
		return CastNumber, nil
	case written == string(CastNumber) && len(k.Composites) > 1:
		return "", fmt.Errorf("index %s: keys can only be cast to number if they are a composite of one numeric attribute", displayIndex(ix.Name))
	default:
		return CastString, nil
	}
}

func (m *Model) attrType(name string) AttrType {
	if a, ok := m.attrs[name]; ok {
		return a.Type
	}
	return ""
}

func allLabelsEmpty(labels []Label) bool {
	for _, l := range labels {
		if l.Label != "" {
			return false
		}
	}
	return true
}

// applyLabels mirrors ElectroDB's _mergeKeyDefinitions: standard keys take
// each attribute's label, falling back to its name. The pk treats an empty
// label as missing; the sk keeps it.
func (m *Model) applyLabels() {
	for _, ix := range m.Indexes {
		for _, kt := range []KeyType{PK, SK} {
			k := &ix.PK
			if kt == SK {
				k = &ix.SK
			}
			if k.Custom {
				continue
			}
			for i := range k.Labels {
				name := k.Labels[i].Name
				label := name
				if a, ok := m.attrs[name]; ok && a.Label != nil {
					if kt == SK || *a.Label != "" {
						label = *a.Label
					}
				}
				k.Labels[i].Label = label
			}
		}
	}
}

// parseTemplate mirrors ElectroDB's _parseTemplateKey.
func parseTemplate(template string) []Label {
	var out []Label
	var label, name strings.Builder
	inName := false
	for i := 0; i < len(template); i++ {
		c := template[i]
		var last, next byte
		if i > 0 {
			last = template[i-1]
		}
		if i+1 < len(template) {
			next = template[i+1]
		}
		switch {
		case c == '{' && last == '$' && !inName:
			inName = true
		case c == '}' && inName:
			out = append(out, Label{Name: name.String(), Label: label.String()})
			name.Reset()
			label.Reset()
			inName = false
		case c == '$' && next == '{' && !inName:
			continue
		case inName:
			name.WriteByte(c)
		default:
			label.WriteByte(c)
		}
	}
	if name.Len() > 0 || label.Len() > 0 {
		out = append(out, Label{Name: name.String(), Label: label.String()})
	}
	return out
}

func makeCollectionPrefix(collections []string) string {
	prefix := ""
	for i, c := range collections {
		if i == 0 {
			prefix += "$" + c
		} else {
			prefix += "#" + c
		}
	}
	return prefix
}

func resolveCasing(c string) Casing {
	switch Casing(c) {
	case Upper, None, Lower:
		return Casing(c)
	default:
		return Lower
	}
}

// FormatCasing applies a key casing. Casing is ASCII-only (decision D6).
func FormatCasing(s string, c Casing) string {
	switch c {
	case Upper:
		return asciiMap(s, 'a', 'z', 'A'-'a')
	case None:
		return s
	default:
		return asciiMap(s, 'A', 'Z', 'a'-'A')
	}
}

func asciiMap(s string, lo, hi byte, delta int) string {
	i := 0
	for i < len(s) && (s[i] < lo || s[i] > hi) {
		i++
	}
	if i == len(s) {
		return s
	}
	b := []byte(s)
	for ; i < len(b); i++ {
		if b[i] >= lo && b[i] <= hi {
			b[i] = byte(int(b[i]) + delta)
		}
	}
	return string(b)
}

func stringArrayMatch(a, b []string) bool {
	if len(a) != len(b) {
		return false
	}
	for i := range a {
		if a[i] == "" || a[i] != b[i] {
			return false
		}
	}
	return true
}

func sameTemplate(a, b *string) bool {
	if a == nil || b == nil {
		return a == nil && b == nil
	}
	return *a == *b
}

func displayIndex(name string) string {
	if name == "" {
		return "(Primary Index)"
	}
	return fmt.Sprintf("%q", name)
}
