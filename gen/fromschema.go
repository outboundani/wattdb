package gen

import (
	"fmt"
	"reflect"
	"regexp"
	"strings"

	"github.com/outboundani/wattdb"
	"github.com/outboundani/wattdb/attr"
	"github.com/outboundani/wattdb/index"
)

// fromSchema builds an entity from a running schema (phase 2).
func fromSchema(pkg, typeName string, ptr bool, s wattdb.Schema) (*entity, error) {
	m := s.Model()
	e := &entity{
		SchemaPkg:  pkg,
		SchemaType: typeName,
		SchemaPtr:  ptr,
		Name:       m.Entity,
		Version:    m.Version,
		Service:    m.Service.Name(),
		Plural:     m.Plural,
		Pkg:        packageName(m.Entity, m.Version),
	}
	for _, a := range s.Attributes() {
		e.Attrs = append(e.Attrs, fromDescriptor(a.Descriptor()))
	}
	if err := e.assignNames(); err != nil {
		return nil, err
	}

	byName := map[string]*attribute{}
	for _, a := range e.Attrs {
		byName[a.GoName] = a
	}
	for i, ix := range s.Indexes() {
		d := ix.Descriptor()
		if d.Index == "" {
			seen := map[string]bool{}
			names := keyComposites(d.PK)
			if d.SK != nil {
				names = append(names, keyComposites(*d.SK)...)
			}
			for _, n := range names {
				a, ok := byName[n]
				if !ok || seen[n] {
					continue
				}
				seen[n] = true
				e.Key = append(e.Key, a)
			}
		}
		if d.Condition != nil {
			e.Conditions = append(e.Conditions, condition{AccessPattern: d.AccessPattern, Index: i})
		}
		p := pattern{Name: d.AccessPattern, GoName: exportName(d.AccessPattern), HasSK: d.SK != nil,
			Composite: d.Type == "composite", Type: string(d.Type), Collections: d.Collection}
		if p.Type == "" {
			p.Type = "isolated"
		}
		for _, n := range keyComposites(d.PK) {
			if a, ok := byName[n]; ok {
				p.PK = append(p.PK, a)
			}
		}
		if d.SK != nil {
			for _, n := range keyComposites(*d.SK) {
				if a, ok := byName[n]; ok {
					p.SK = append(p.SK, a)
				}
			}
		}
		e.Patterns = append(e.Patterns, p)
	}
	if err := e.checkPatternNames(); err != nil {
		return nil, err
	}
	return e, nil
}

var templateVar = regexp.MustCompile(`\$\{([^}]*)\}`)

// keyComposites returns a key's composites (Go names): the composite list,
// or the template's variables when only a template was given.
func keyComposites(k index.Key) []string {
	if k.Composites != nil || k.Template == nil {
		return k.Composites
	}
	var out []string
	for _, m := range templateVar.FindAllStringSubmatch(*k.Template, -1) {
		out = append(out, m[1])
	}
	return out
}

// checkPatternNames rejects access patterns whose generated names collide.
func (e *entity) checkPatternNames() error {
	reserved := map[string]bool{"Get": true, "Put": true, "Create": true, "Delete": true, "Remove": true,
		"Update": true, "Patch": true, "Upsert": true, "Scan": true, "BatchGet": true, "BatchPut": true,
		"BatchDelete": true, "Check": true, "Find": true, "Match": true, "Conv": true, "Parse": true}
	seen := map[string]string{}
	for _, p := range e.Patterns {
		if reserved[p.GoName] {
			return fmt.Errorf("entity %q: access pattern %q would generate method %s, which the client already has", e.Name, p.Name, p.GoName)
		}
		if prev, ok := seen[p.GoName]; ok {
			return fmt.Errorf("entity %q: access patterns %q and %q both generate %s", e.Name, prev, p.Name, p.GoName)
		}
		seen[p.GoName] = p.Name
	}
	return nil
}

func fromDescriptor(d *attr.Descriptor) *attribute {
	a := &attribute{
		GoName:         d.Name,
		Field:          d.Field,
		JSON:           d.JSONName,
		Kind:           d.Kind,
		Required:       d.Required,
		HasDefault:     d.HasDefault,
		HasDefaultFunc: d.DefaultFunc != nil,
		HasGet:         d.Get != nil,
		HasSet:         d.Set != nil,
		HasValidate:    d.Validate != nil,
		HasCodec:       d.Codec != nil,
		Enum:           d.Enum,
	}
	if d.GoType != nil {
		a.Elem = typeRefOf(d.GoType)
	}
	if d.Items != nil {
		a.Items = fromDescriptor(d.Items)
		a.Items.GoName = itemName(a.Items)
	}
	for _, p := range d.Props {
		a.Props = append(a.Props, fromDescriptor(p))
	}
	return a
}

// typeRefOf renders a reflect type as Go source, with the import its named
// type needs.
func typeRefOf(t reflect.Type) typeRef {
	r := typeRef{Expr: t.String(), Import: t.PkgPath()}
	switch t.Kind() {
	case reflect.Int, reflect.Int8, reflect.Int16, reflect.Int32, reflect.Int64,
		reflect.Uint, reflect.Uint8, reflect.Uint16, reflect.Uint32, reflect.Uint64,
		reflect.Float32, reflect.Float64:
		r.Numeric = true
	}
	return r
}

// checkCallbacks verifies each callback's Go type against the generated
// types, so mismatches fail at generation instead of at registration.
func checkCallbacks(e *entity, s wattdb.Schema) error {
	attrs := s.Attributes()
	var errs []string
	var walk func(a *attribute, d *attr.Descriptor)
	walk = func(a *attribute, d *attr.Descriptor) {
		t := a.goTypeQ(e.Pkg)
		p := a.fieldTypeQ(partialVariant, e.Pkg)
		sib := qualifySiblings(a, e.Pkg)
		check := func(name string, fn any, want string) {
			if fn == nil {
				return
			}
			if got := reflect.TypeOf(fn).String(); got != want {
				errs = append(errs, fmt.Sprintf("attribute %s: %s is %s, want %s", a.Path, name, got, want))
			}
		}
		check("Get", d.Get, fmt.Sprintf("func(%s, %s) %s", p, sib, p))
		check("Set", d.Set, fmt.Sprintf("func(%s, %s) %s", p, sib, p))
		check("Validate", d.Validate, fmt.Sprintf("func(%s) error", p))
		check("DefaultFunc", d.DefaultFunc, fmt.Sprintf("func() %s", t))
		if a.Items != nil {
			walk(a.Items, d.Items)
		}
		for i, prop := range a.Props {
			walk(prop, d.Props[i])
		}
	}
	for i, a := range e.Attrs {
		walk(a, attrs[i].Descriptor())
	}
	for _, c := range e.Conditions {
		fn := s.Indexes()[c.Index].Descriptor().Condition
		want := fmt.Sprintf("func(%s.Partial) bool", e.Pkg)
		if got := reflect.TypeOf(fn).String(); got != want {
			errs = append(errs, fmt.Sprintf("access pattern %s: Condition is %s, want %s", c.AccessPattern, got, want))
		}
	}
	if len(errs) > 0 {
		return fmt.Errorf("entity %q: %s", e.Name, strings.Join(errs, "; "))
	}
	return nil
}

func qualifySiblings(a *attribute, pkg string) string {
	switch {
	case a.Siblings == "Partial":
		return pkg + ".Partial"
	case strings.HasPrefix(a.Siblings, "[]"):
		return "[]" + a.goTypeQ(pkg)
	default:
		return pkg + "." + a.Siblings
	}
}
