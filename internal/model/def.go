package model

// Definition is an entity schema as written: the Go equivalent of an
// ElectroDB v1 schema object. The schema DSL and the vector loader both
// produce Definitions, and New normalizes one into a Model.
//
// Attributes and Indexes are slices because their order is significant in
// ElectroDB (it follows object key order).
type Definition struct {
	Entity  string
	Version string
	Service string

	Attributes []AttributeDef
	Indexes    []IndexDef

	// Identifiers overrides the identifier field names. Empty fields keep the
	// defaults, __edb_e__ and __edb_v__.
	Identifiers Identifiers
}

// AttributeDef is one attribute as written.
type AttributeDef struct {
	Name string
	// Type is one of the AttrType values. For enums, Type is TypeEnum and
	// Enum lists the allowed values.
	Type AttrType
	Enum []string

	Field    string
	Label    *string
	Required bool
	Hidden   bool
	ReadOnly bool
	Padding  *Padding

	// Items describes list items. For sets, SetItems names the item type.
	Items    *AttributeDef
	SetItems AttrType
	// Properties lists the attributes of a map, in order.
	Properties []AttributeDef
}

// IndexDef is one access pattern as written.
type IndexDef struct {
	AccessPattern string
	// Index is the DynamoDB index name; empty means the table's own index.
	Index string
	// Type is "isolated" (the default), "clustered" or "composite".
	Type IndexType
	// Collection lists the collections the index belongs to. CollectionArray
	// records whether it was written as an array, which ElectroDB treats as
	// sub-collections.
	Collection      []string
	CollectionArray bool
	Scope           string
	// Condition decides whether a write keeps this index's keys (a sparse
	// index). Nil means no condition is defined.
	Condition func(attrs map[string]any) bool
	PK        KeyDef
	// SK is nil when the index has no sort key.
	SK *KeyDef
	// Projection is "all", "keys_only", or an attribute list. Nil means unset.
	Projection *Projection
}

// KeyDef is a pk or sk definition as written.
type KeyDef struct {
	Field     string
	Composite []string
	// Template is the key template, e.g. "user#${userId}". Nil means none.
	Template *string
	// Casing is "lower", "upper", "none" or "default". Empty means default.
	Casing string
	// Cast is "string" or "number". Empty means unset.
	Cast string
}

// Projection is an index projection.
type Projection struct {
	// Kind is "all" or "keys_only" when Attributes is empty.
	Kind       string
	Attributes []string
}

// Padding pads string forms of key composites, like String.padStart.
type Padding struct {
	Length int
	Char   string
}

// Identifiers names the fields holding the entity name and version.
type Identifiers struct {
	Entity  string
	Version string
}
