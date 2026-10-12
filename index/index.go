// Package index provides the index builders used in wattdb schemas. The
// chain mirrors an ElectroDB index object (decision D14):
//
//	index.GSI("byRole", "gsi2").
//		PKField("gsi2pk").PK("OrgID").
//		SKField("gsi2sk").SK("Role", "UserID").
//		Collection("members")
//
// Composites and template variables use attribute Go names.
package index

// Index is an index definition. Schemas return them from Indexes.
type Index interface {
	Descriptor() *Descriptor
}

// Type is an index's key layout.
type Type string

const (
	Isolated  Type = "isolated"
	Clustered Type = "clustered"
	Composite Type = "composite"
)

// Casing is a key casing.
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

// Projection is an index projection.
type Projection struct {
	Kind       string
	Attributes []string
}

var (
	// All projects every attribute.
	All = Projection{Kind: "all"}
	// KeysOnly projects only the keys.
	KeysOnly = Projection{Kind: "keys_only"}
)

// Include projects the named attributes (Go names).
func Include(names ...string) Projection { return Projection{Attributes: names} }

// Key is a recorded pk or sk.
type Key struct {
	Field string
	// Composites are attribute Go names; nil means none were given.
	Composites []string
	Template   *string
	Casing     Casing
	Cast       Cast
}

// Descriptor holds everything a builder recorded. The generator and
// wattdb.Compile read it.
type Descriptor struct {
	AccessPattern string
	// Index is the DynamoDB index name; empty for the table index.
	Index string
	Type  Type
	Scope string
	// Collection holds the collections; SubCollections reports the array form.
	Collection     []string
	SubCollections bool
	// Condition is a func(<entity>.Partial) bool, if any.
	Condition  any
	Projection *Projection
	PK         Key
	// SK is nil when the index has no sort key.
	SK *Key
}

// Builder builds one index.
type Builder struct {
	d Descriptor
}

// Descriptor returns the recorded definition.
func (b *Builder) Descriptor() *Descriptor { return &b.d }

// Primary is the table's own index.
func Primary(accessPattern string) *Builder {
	return &Builder{d: Descriptor{AccessPattern: accessPattern}}
}

// GSI is a secondary index named indexName in DynamoDB.
func GSI(accessPattern, indexName string) *Builder {
	return &Builder{d: Descriptor{AccessPattern: accessPattern, Index: indexName}}
}

func (b *Builder) sk() *Key {
	if b.d.SK == nil {
		b.d.SK = &Key{}
	}
	return b.d.SK
}

// PK sets the partition key composites.
func (b *Builder) PK(composites ...string) *Builder {
	b.d.PK.Composites = append([]string{}, composites...)
	return b
}

// SK sets the sort key composites. Calling it with none gives a sort key
// with no composites; not calling it gives an index without a sort key.
func (b *Builder) SK(composites ...string) *Builder {
	b.sk().Composites = append([]string{}, composites...)
	return b
}

// PKField sets the partition key's DynamoDB field.
func (b *Builder) PKField(field string) *Builder { b.d.PK.Field = field; return b }

// SKField sets the sort key's DynamoDB field.
func (b *Builder) SKField(field string) *Builder { b.sk().Field = field; return b }

// PKTemplate sets a partition key template, e.g. "user#${UserID}".
func (b *Builder) PKTemplate(t string) *Builder { b.d.PK.Template = &t; return b }

// SKTemplate sets a sort key template.
func (b *Builder) SKTemplate(t string) *Builder { b.sk().Template = &t; return b }

// PKCasing sets the partition key casing (default lower).
func (b *Builder) PKCasing(c Casing) *Builder { b.d.PK.Casing = c; return b }

// SKCasing sets the sort key casing (default lower).
func (b *Builder) SKCasing(c Casing) *Builder { b.sk().Casing = c; return b }

// PKCast sets how the partition key is stored.
func (b *Builder) PKCast(c Cast) *Builder { b.d.PK.Cast = c; return b }

// SKCast sets how the sort key is stored.
func (b *Builder) SKCast(c Cast) *Builder { b.sk().Cast = c; return b }

// Collection puts the index in a collection (ElectroDB's string form).
func (b *Builder) Collection(name string) *Builder {
	b.d.Collection, b.d.SubCollections = []string{name}, false
	return b
}

// SubCollections puts the index in nested collections (ElectroDB's array
// form).
func (b *Builder) SubCollections(names ...string) *Builder {
	b.d.Collection, b.d.SubCollections = append([]string{}, names...), true
	return b
}

// Type sets the key layout (default Isolated).
func (b *Builder) Type(t Type) *Builder { b.d.Type = t; return b }

// Scope adds a scope to the partition key prefix.
func (b *Builder) Scope(scope string) *Builder { b.d.Scope = scope; return b }

// Condition makes the index sparse: func(<entity>.Partial) bool decides
// whether a write keeps the index's keys.
func (b *Builder) Condition(fn any) *Builder { b.d.Condition = fn; return b }

// Projection records the index's projection.
func (b *Builder) Projection(p Projection) *Builder { b.d.Projection = &p; return b }
