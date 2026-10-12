package wattdb

import (
	"github.com/outboundani/wattdb/attr"
	"github.com/outboundani/wattdb/index"
)

// ServiceDef is a service declared with Service. Its zero value is invalid,
// so a Model can't name a service by accident (decision D15).
type ServiceDef struct {
	name string
}

// Service declares a service. Declare each once, as a package-level var:
//
//	var TaskApp = wattdb.Service("taskapp")
func Service(name string) ServiceDef {
	return ServiceDef{name: name}
}

// Name returns the service name.
func (s ServiceDef) Name() string { return s.name }

// Model names an entity, like an ElectroDB model block.
type Model struct {
	Entity  string
	Version string
	Service ServiceDef
	// Plural overrides the generated plural name (decision D21).
	Plural string
}

// Attribute is an attribute definition; build one with package attr.
type Attribute = attr.Attribute

// Index is an index definition; build one with package index.
type Index = index.Index

// Schema is an entity schema (decision D9). Any type with these methods is
// one; the generator finds them in the schema packages.
type Schema interface {
	Model() Model
	Attributes() []Attribute
	Indexes() []Index
}
