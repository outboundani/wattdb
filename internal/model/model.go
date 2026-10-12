package model

// AttrType is an attribute's type.
type AttrType string

const (
	TypeString  AttrType = "string"
	TypeNumber  AttrType = "number"
	TypeBoolean AttrType = "boolean"
	TypeEnum    AttrType = "enum"
	TypeMap     AttrType = "map"
	TypeList    AttrType = "list"
	TypeSet     AttrType = "set"
	TypeAny     AttrType = "any"
)

// IndexType is an index's key layout.
type IndexType string

const (
	Isolated  IndexType = "isolated"
	Clustered IndexType = "clustered"
	Composite IndexType = "composite"
)

// Casing is a resolved key casing.
type Casing string

const (
	Lower Casing = "lower"
	Upper Casing = "upper"
	None  Casing = "none"
)

// Cast is how a key value is stored.
type Cast string

const (
	CastString Cast = "string"
	CastNumber Cast = "number"
)

// KeyType distinguishes partition and sort keys.
type KeyType string

const (
	PK KeyType = "pk"
	SK KeyType = "sk"
)

// DefaultIdentifiers are ElectroDB's identifier field names.
var DefaultIdentifiers = Identifiers{Entity: "__edb_e__", Version: "__edb_v__"}

// Model is a normalized entity, the equivalent of ElectroDB's parsed model.
type Model struct {
	Entity      string
	Version     string
	Service     string
	Identifiers Identifiers

	// Attributes are the root attributes in definition order.
	Attributes []*Attribute
	// Indexes are the access patterns in definition order.
	Indexes []*Index

	attrs     map[string]*Attribute
	byName    map[string]*Index
	byPattern map[string]*Index

	// facetOrder lists composite attribute names in the order ElectroDB first
	// sees them (index order, pk before sk). facets maps each to its uses.
	facetOrder []string
	facets     map[string][]Facet
}

// Attribute is a normalized attribute.
type Attribute struct {
	Name     string
	Field    string
	Type     AttrType
	Enum     []string
	Label    *string
	Required bool
	Hidden   bool
	ReadOnly bool
	Padding  *Padding

	Items      *Attribute
	SetItems   AttrType
	Properties []*Attribute
}

// Index is a normalized access pattern.
type Index struct {
	AccessPattern string
	// Name is the DynamoDB index name; empty for the table index.
	Name  string
	Type  IndexType
	Scope string
	// Collections lists the index's collections; SubCollections is true when
	// they were written as an array.
	Collections    []string
	SubCollections bool
	HasSK          bool
	PK             Key
	SK             Key

	// Condition is the sparse-index callback; nil when none is defined.
	Condition  func(attrs map[string]any) bool
	Projection *Projection
}

// Key is a normalized pk or sk.
type Key struct {
	Field string
	// Composites are the key's attribute names, in order.
	Composites []string
	// Labels are the composite segments: for standard keys, one per composite
	// with its label; for templates, the literal text before each attribute
	// (and a trailing entry with an empty Name for text after the last one).
	Labels []Label
	// Template is the template as written, if any.
	Template *string
	// FieldRef is true when the key field is also an attribute's field.
	FieldRef bool

	// The fixings below come from ElectroDB's _makeKeyFixings. When another
	// index already uses the same field, they are copied from that index.
	Custom     bool
	Casing     Casing
	Cast       Cast
	Prefix     string
	Postfix    string
	HasPostfix bool
}

// Label is one composite segment of a key.
type Label struct {
	Name  string
	Label string
}

// Facet is one use of an attribute as a key composite.
type Facet struct {
	Index *Index
	Type  KeyType
}

// Attribute returns the root attribute with the given name.
func (m *Model) Attribute(name string) (*Attribute, bool) {
	a, ok := m.attrs[name]
	return a, ok
}

// Index returns the index with the given DynamoDB name ("" for the table).
func (m *Model) Index(name string) (*Index, bool) {
	ix, ok := m.byName[name]
	return ix, ok
}

// AccessPattern returns the index for the given access pattern.
func (m *Model) AccessPattern(name string) (*Index, bool) {
	ix, ok := m.byPattern[name]
	return ix, ok
}

// TableIndex returns the table's own index.
func (m *Model) TableIndex() *Index {
	return m.byName[""]
}

// FacetAttributes returns the names of attributes used as key composites, in
// the order ElectroDB iterates them.
func (m *Model) FacetAttributes() []string {
	return m.facetOrder
}

// Facets returns every key use of the named attribute.
func (m *Model) Facets(name string) []Facet {
	return m.facets[name]
}

// Composites returns all of the index's composites, pk first.
func (ix *Index) Composites() []string {
	out := make([]string, 0, len(ix.PK.Composites)+len(ix.SK.Composites))
	out = append(out, ix.PK.Composites...)
	return append(out, ix.SK.Composites...)
}
