// Package gen is wattdb's code generator (decisions D10, D22). Phase 1
// reads schema source and writes each entity's types; phase 2 runs the
// schemas (a small generated program calls Main) and writes everything
// else, including db/runtime.
//
// The wattdb command drives it; applications don't import it.
package gen

import (
	"fmt"
	"go/token"
	"sort"
	"strings"
	"unicode"

	"github.com/outboundani/wattdb/attr"
)

// entity is the generator's view of one schema.
type entity struct {
	SchemaPkg  string // import path of the schema package
	SchemaType string // schema type name
	// SchemaPtr is true when the schema's methods have pointer receivers.
	SchemaPtr bool
	Name      string // ElectroDB entity name
	Version   string
	Service   string
	Pkg       string // generated package name
	Attrs     []*attribute

	// Filled in phase 2.
	Key        []*attribute // primary composites, pk first
	Conditions []condition
	Plural     string
	Patterns   []pattern
}

// pattern is an access pattern, for queries and scans.
type pattern struct {
	Name      string // access pattern
	GoName    string // exported method name
	PK, SK    []*attribute
	HasSK     bool
	Composite bool   // multi-attribute (composite) index
	Type      string // isolated, clustered or composite
	// Collections are the index's collections; with sub-collections, each
	// one's name in order.
	Collections []string
}

type condition struct {
	AccessPattern string
	Index         int // position in Indexes()
}

// attribute is the generator's view of one attribute.
type attribute struct {
	GoName   string
	Field    string
	JSON     string
	Kind     attr.Kind
	Required bool

	HasDefault, HasDefaultFunc  bool
	HasGet, HasSet, HasValidate bool
	HasCodec                    bool

	Enum  []string
	Items *attribute
	Props []*attribute
	// Elem is the type argument of Set, AnyOf and Custom.
	Elem typeRef

	// Path is the hook path ("Profile.Address", "Steps[*]").
	Path string
	// Desc reaches the attribute's descriptor from the Attributes() slice,
	// e.g. "[3].Descriptor().Props[0]".
	Desc string
	// TypeName is the generated type for enums and maps.
	TypeName string
	// Siblings is the Go type callbacks receive as siblings.
	Siblings string
	// CodecVar names the attribute's codec in the generated codecs struct.
	CodecVar string
}

// typeRef is a Go type expression and the import it needs.
type typeRef struct {
	Expr   string
	Import string // empty for predeclared types
	// Numeric is set for Set element types with a numeric underlying type.
	Numeric bool
}

// packageName derives a generated package name from an entity name and
// version (decision D47): "user" v1 → user, v2 → userv2.
func packageName(entity, version string) string {
	var b strings.Builder
	for _, r := range strings.ToLower(entity) {
		if r < unicode.MaxASCII && (unicode.IsLetter(r) || unicode.IsDigit(r)) {
			b.WriteRune(r)
		}
	}
	name := b.String()
	if name == "" || unicode.IsDigit(rune(name[0])) {
		name = "e" + name
	}
	if version != "" && version != "1" {
		name += "v" + strings.Map(func(r rune) rune {
			if unicode.IsLetter(r) || unicode.IsDigit(r) {
				return r
			}
			return -1
		}, version)
	}
	if token.IsKeyword(name) {
		name += "entity"
	}
	return name
}

// assignNames fills paths, generated type names and sibling types, and
// checks that names are usable and unique.
func (e *entity) assignNames() error {
	types := map[string]string{}
	claim := func(name, path string) error {
		if prev, ok := types[name]; ok {
			return fmt.Errorf("entity %q: %s and %s both generate type %s", e.Name, prev, path, name)
		}
		types[name] = path
		return nil
	}
	for _, reserved := range []string{e.structName(), "Input", "Partial", "Key", "Filter",
		"Client", "Conv", "IndexConv", "New", "Register", "Definition", "Decode", "Facets"} {
		types[reserved] = "(generated)"
	}
	codecs := map[string]bool{}

	var walk func(a *attribute, path, desc, mapPath, siblings, listName string) error
	walk = func(a *attribute, path, desc, mapPath, siblings, listName string) error {
		if a.Field == "" {
			a.Field = a.GoName
		}
		if a.JSON == "" {
			a.JSON = a.Field
		}
		a.Path, a.Desc, a.Siblings = path, desc, siblings
		own := a.GoName
		if own == "" {
			own = listName + "Item"
		}
		switch a.Kind {
		case attr.KindEnum:
			a.TypeName = mapPath + own + "Enum"
			if err := claim(a.TypeName, path); err != nil {
				return err
			}
			seen := map[string]bool{}
			for _, v := range a.Enum {
				c := enumConst(a.TypeName, v)
				if seen[c] {
					return fmt.Errorf("entity %q: enum %s values collide as constant %s", e.Name, path, c)
				}
				seen[c] = true
				if err := claim(c, path); err != nil {
					return err
				}
			}
		case attr.KindMap:
			if listName != "" {
				a.TypeName = own + "Map"
			} else {
				a.TypeName = mapPath + own + "Map"
			}
			if err := claim(a.TypeName, path); err != nil {
				return err
			}
			for i, p := range a.Props {
				if err := checkIdent(e.Name, p.GoName, path); err != nil {
					return err
				}
				prefix := strings.TrimSuffix(a.TypeName, "Map")
				if err := walk(p, path+"."+p.GoName, fmt.Sprintf("%s.Props[%d]", desc, i), prefix, a.TypeName, ""); err != nil {
					return err
				}
			}
		case attr.KindList:
			if a.Items == nil {
				return fmt.Errorf("entity %q: list %s has no items", e.Name, path)
			}
			if err := walk(a.Items, path+"[*]", desc+".Items", mapPath, "", own); err != nil {
				return err
			}
			a.Items.Siblings = "[]" + a.Items.goType()
		case attr.KindTime, attr.KindDuration, attr.KindUUID, attr.KindCustom:
			a.CodecVar = codecVar(path)
			if codecs[a.CodecVar] {
				return fmt.Errorf("entity %q: codec name %s is used twice", e.Name, a.CodecVar)
			}
			codecs[a.CodecVar] = true
		}
		return nil
	}
	// Descriptors: one per root attribute, plus flattened names for map
	// properties (ProfileDisplayName), and a descriptor struct per map type.
	var claimDescs func(a *attribute, flat string) error
	claimDescs = func(a *attribute, flat string) error {
		if err := claim(flat, a.Path+" (descriptor)"); err != nil {
			return err
		}
		if a.Kind == attr.KindMap {
			if err := claim(a.TypeName+"Attr", a.Path+" (descriptor type)"); err != nil {
				return err
			}
			for _, p := range a.Props {
				if p.GoName == "Attr" {
					return fmt.Errorf("entity %q: map property %s can't be named Attr", e.Name, p.Path)
				}
				if err := claimDescs(p, flat+p.GoName); err != nil {
					return err
				}
			}
		}
		if a.Kind == attr.KindList && a.Items != nil && a.Items.Kind == attr.KindMap {
			if err := claimItemMaps(claim, a.Items); err != nil {
				return err
			}
		}
		return nil
	}
	fields := map[string]bool{}
	for i, a := range e.Attrs {
		if err := checkIdent(e.Name, a.GoName, ""); err != nil {
			return err
		}
		if err := walk(a, a.GoName, fmt.Sprintf("[%d].Descriptor()", i), "", "Partial", ""); err != nil {
			return err
		}
		if fields[a.GoName] {
			return fmt.Errorf("entity %q: attribute %s is defined twice", e.Name, a.GoName)
		}
		fields[a.GoName] = true
	}
	for _, a := range e.Attrs {
		if err := claimDescs(a, a.GoName); err != nil {
			return err
		}
	}
	return nil
}

// claimItemMaps claims the descriptor struct names of a list item map and
// the maps nested in it.
func claimItemMaps(claim func(name, path string) error, a *attribute) error {
	if err := claim(a.TypeName+"Attr", a.Path+" (descriptor type)"); err != nil {
		return err
	}
	for _, p := range a.Props {
		if p.Kind == attr.KindMap {
			if err := claimItemMaps(claim, p); err != nil {
				return err
			}
		}
	}
	return nil
}

func (e *entity) structName() string {
	return exportName(e.Name)
}

func checkIdent(entity, name, parent string) error {
	if name == "" || !token.IsIdentifier(name) || !token.IsExported(name) {
		where := "attribute"
		if parent != "" {
			where = "property of " + parent
		}
		return fmt.Errorf("entity %q: %s name %q must be an exported Go identifier", entity, where, name)
	}
	return nil
}

// exportName turns an ElectroDB name into an exported Go identifier.
func exportName(s string) string {
	var b strings.Builder
	upper := true
	for _, r := range s {
		if !unicode.IsLetter(r) && !unicode.IsDigit(r) {
			upper = true
			continue
		}
		if upper {
			b.WriteRune(unicode.ToUpper(r))
			upper = false
		} else {
			b.WriteRune(r)
		}
	}
	out := b.String()
	if out == "" || unicode.IsDigit(rune(out[0])) {
		out = "X" + out
	}
	return out
}

// enumConst names an enum constant: RoleEnum + "admin" → RoleAdmin.
func enumConst(typeName, value string) string {
	return strings.TrimSuffix(typeName, "Enum") + exportName(value)
}

func codecVar(path string) string {
	return exportName(strings.NewReplacer(".", "_", "[*]", "_Item").Replace(path))
}

// goType is the attribute's Go type, before optional pointers.
func (a *attribute) goType() string { return a.goTypeQ("") }

// goTypeQ is goType with generated type names qualified by pkg (for
// comparing against reflect type strings).
func (a *attribute) goTypeQ(pkg string) string {
	q := func(name string) string {
		if pkg == "" {
			return name
		}
		return pkg + "." + name
	}
	switch a.Kind {
	case attr.KindString:
		return "string"
	case attr.KindInt:
		return "int"
	case attr.KindInt64:
		return "int64"
	case attr.KindFloat64:
		return "float64"
	case attr.KindBool:
		return "bool"
	case attr.KindEnum, attr.KindMap:
		return q(a.TypeName)
	case attr.KindTime:
		return "time.Time"
	case attr.KindDuration:
		return "time.Duration"
	case attr.KindUUID:
		if pkg != "" {
			return "[16]uint8" // reflect's spelling
		}
		return "[16]byte"
	case attr.KindSet:
		return "[]" + a.Elem.Expr
	case attr.KindList:
		return "[]" + a.Items.goTypeQ(pkg)
	case attr.KindAny:
		if pkg != "" {
			return "interface {}" // reflect's spelling
		}
		return "any"
	case attr.KindAnyOf, attr.KindCustom:
		return a.Elem.Expr
	default:
		return "any"
	}
}

// nilable reports whether the Go type already has a "missing" value (nil),
// so optional fields don't need a pointer.
func (a *attribute) nilable() bool {
	switch a.Kind {
	case attr.KindSet, attr.KindList, attr.KindAny:
		return true
	default:
		return false
	}
}

type variant int

const (
	readVariant variant = iota
	inputVariant
	partialVariant
)

// fieldType is the attribute's Go type in a generated struct (D16, D17).
func (a *attribute) fieldType(v variant) string { return a.fieldTypeQ(v, "") }

func (a *attribute) fieldTypeQ(v variant, pkg string) string {
	t := a.goTypeQ(pkg)
	if a.nilable() {
		return t
	}
	switch v {
	case readVariant:
		if a.Required {
			return t
		}
	case inputVariant:
		if a.Required && !a.HasDefault && !a.HasDefaultFunc {
			return t
		}
	}
	return "*" + t
}

// imports collects the imports the entity's types need.
func (e *entity) imports() []string {
	set := map[string]bool{}
	var walk func(a *attribute)
	walk = func(a *attribute) {
		switch a.Kind {
		case attr.KindTime, attr.KindDuration:
			set["time"] = true
		}
		if a.Elem.Import != "" {
			set[a.Elem.Import] = true
		}
		if a.Items != nil {
			walk(a.Items)
		}
		for _, p := range a.Props {
			walk(p)
		}
	}
	for _, a := range e.Attrs {
		walk(a)
	}
	out := make([]string, 0, len(set))
	for k := range set {
		out = append(out, k)
	}
	sort.Strings(out)
	return out
}

// enums and maps return the generated enum and map attributes, in order.
func (e *entity) collect(kind attr.Kind) []*attribute {
	var out []*attribute
	var walk func(a *attribute)
	walk = func(a *attribute) {
		if a.Kind == kind {
			out = append(out, a)
		}
		if a.Items != nil {
			walk(a.Items)
		}
		for _, p := range a.Props {
			walk(p)
		}
	}
	for _, a := range e.Attrs {
		walk(a)
	}
	return out
}
