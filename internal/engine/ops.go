package engine

import (
	"fmt"
	"strings"

	"github.com/outboundani/wattdb/internal/model"
)

// Path addresses an attribute: a root attribute name, then map property
// names or list indexes ("0"), as in ElectroDB's attribute proxies.
type Path []string

// ParsePath mirrors ElectroDB's parseJSONPath: "a.b[0].c" → [a b 0 c].
func ParsePath(s string) Path {
	s = strings.ReplaceAll(s, "[", ".")
	s = strings.ReplaceAll(s, "]", "")
	var out Path
	for _, p := range strings.Split(s, ".") {
		if p != "" {
			out = append(out, p)
		}
	}
	return out
}

// String renders the path the way ElectroDB's JSON paths look.
func (p Path) String() string {
	var b strings.Builder
	for i, part := range p {
		switch {
		case i == 0:
			b.WriteString(part)
		case !isNaN(part):
			b.WriteString("[" + part + "]")
		default:
			b.WriteString("." + part)
		}
	}
	return b.String()
}

// resolved is a path resolved against the model, mirroring the chain of
// pathProxy builds.
type resolved struct {
	target *model.Attribute
	steps  []step
}

type step struct {
	prop, field string
}

// resolve walks a path to its attribute. Unknown root attributes return a
// nil target (ElectroDB skips them); unknown nested ones are an error.
func resolve(m *model.Model, p Path) (resolved, error) {
	if len(p) == 0 {
		return resolved{}, fmt.Errorf("empty attribute path")
	}
	root, ok := m.Attribute(p[0])
	if !ok {
		return resolved{}, nil
	}
	r := resolved{target: root, steps: []step{{prop: root.Name, field: root.Field}}}
	for _, prop := range p[1:] {
		child := childAttribute(r.target, prop)
		if child == nil {
			return resolved{}, fmt.Errorf("invalid attribute %q at path %q", prop, Path(p).String())
		}
		field := child.Field
		if child.Type == model.TypeAny && child.Name != prop {
			field = prop // nested under an "any" attribute
		}
		r.steps = append(r.steps, step{prop: prop, field: field})
		r.target = child
	}
	return r, nil
}

// childAttribute mirrors Attribute.getChild.
func childAttribute(a *model.Attribute, prop string) *model.Attribute {
	switch {
	case a.Type == model.TypeAny:
		return a
	case !isNaN(prop) && (a.Type == model.TypeList || a.Type == model.TypeSet):
		return a.Items
	default:
		p, _ := a.Property(prop)
		return p
	}
}

// commit registers the path's names, as the proxy's commit() does.
func (r resolved) commit(s *exprState) paths {
	var cur *paths
	var out paths
	for i, st := range r.steps {
		if i == 0 {
			out = s.setName(nil, st.prop, st.field)
		} else {
			out = s.setName(cur, st.prop, st.field)
		}
		c := out
		cur = &c
	}
	return out
}

// acceptable mirrors Attribute.acceptable (and SetAttribute's).
func acceptable(a *model.Attribute, v any) bool {
	if a.Type == model.TypeSet {
		if list, ok := v.([]any); ok {
			return len(list) > 0
		}
		if set, ok := v.(Set); ok {
			return len(set) > 0
		}
		return !isUndefined(v)
	}
	return !isUndefined(v)
}

// applyFixings mirrors Attribute.applyFixings: values compared with a key
// field attribute get the key template's prefix/postfix and casing.
func applyFixings(a *model.Attribute, v any) any {
	if isUndefined(v) {
		return v
	}
	if a.Type == model.TypeString || a.Type == model.TypeEnum {
		if a.KeyPrefix != "" || a.KeyPostfix != "" {
			v = a.KeyPrefix + JSString(v) + a.KeyPostfix
		}
	}
	if s, ok := v.(string); ok && a.KeyCasing != "" {
		return model.FormatCasing(s, a.KeyCasing)
	}
	return v
}

// takesValue mirrors "template.length > 3" for each operation.
var takesValue = map[string]bool{
	"ifNotExists": true, "value": true, "append": true, "add": true, "subtract": true,
	"set": true, "del": true, "delete": true,
	"type": true, "ne": true, "eq": true, "gt": true, "lt": true, "gte": true, "lte": true,
	"between": true, "begins": true, "contains": true, "notContains": true, "eqOrNotExists": true,
}

// applyOp mirrors the operation functions built by AttributeOperationProxy:
// it registers names and values and returns the rendered expression. For
// update operations it also adds the expression to the update; it returns
// "" when ElectroDB would skip the operation (e.g. an empty set).
func applyOp(m *model.Model, s *exprState, upd *updateExpr, op string, p Path, values ...any) (string, error) {
	r, err := resolve(m, p)
	if err != nil {
		return "", err
	}
	if r.target == nil {
		return "", nil
	}
	target := r.target
	var fixed []any
	for _, v := range values {
		if f := applyFixings(target, v); !isUndefined(f) {
			fixed = append(fixed, f)
		}
	}
	isFilter := upd == nil
	accept := true
	for _, v := range fixed {
		accept = accept && acceptable(target, v)
	}
	if !isFilter && takesValue[op] && (!accept || len(fixed) == 0) {
		return "", nil
	}

	ps := r.commit(s)
	refs := make([]string, len(fixed))
	for i, v := range fixed {
		refs[i] = s.setValue(target.Name, v)
		s.setPath(ps.json, pathValue{value: v, name: refs[i]})
	}
	createValue := func(name string, v any) string {
		return s.setValue(target.Name+"_"+name, v)
	}

	if isFilter {
		s.impacted[ps.json] = op
		s.refs[ps.json] = target
		return filterTemplate(op, ps.expression, refs...), nil
	}

	kind, expression, err := updateTemplate(op, target, ps.expression, refs, createValue)
	if err != nil {
		return "", err
	}
	s.impacted[ps.json] = op
	s.refs[ps.json] = target
	if kind != "" {
		upd.add(kind, expression)
	}
	return expression, nil
}

// updateTemplate mirrors updateOperations.js, returning the update section
// ("set", "add", ...) and the expression.
func updateTemplate(op string, a *model.Attribute, path string, values []string, createValue func(string, any) string) (string, string, error) {
	v := func(i int) string {
		if i < len(values) {
			return values[i]
		}
		return "undefined"
	}
	switch op {
	case "ifNotExists":
		return "set", path + " = if_not_exists(" + path + ", " + v(0) + ")", nil
	case "append":
		if a.Type != model.TypeAny && a.Type != model.TypeList {
			return "", "", fmt.Errorf(`invalid update attribute operation: "APPEND" can only be performed on attributes with type "list" or "any"`)
		}
		dv := createValue("default_value", []any{})
		return "set", path + " = list_append(if_not_exists(" + path + ", " + dv + "), " + v(0) + ")", nil
	case "add":
		switch a.Type {
		case model.TypeAny, model.TypeSet, model.TypeNumber:
			// The value is a placeholder string, so ElectroDB's "any holding
			// a number" check never matches: any always ADDs.
			return "add", path + " " + v(0), nil
		default:
			return "", "", fmt.Errorf(`invalid update attribute operation: "ADD" can only be performed on attributes with type "number", "set", or "any"`)
		}
	case "subtract":
		if a.Type != model.TypeAny && a.Type != model.TypeNumber {
			return "", "", fmt.Errorf(`invalid update attribute operation: "SUBTRACT" can only be performed on attributes with type "number" or "any"`)
		}
		dv := createValue("default_value", Number("0"))
		return "set", path + " = (if_not_exists(" + path + ", " + dv + ") - " + v(0) + ")", nil
	case "set":
		return "set", path + " = " + v(0), nil
	case "remove":
		return "remove", path, nil
	case "delete", "del":
		if a.Type != model.TypeAny && a.Type != model.TypeSet {
			return "", "", fmt.Errorf(`invalid update attribute operation: "DELETE" can only be performed on attributes with type "set" or "any"`)
		}
		return "delete", path + " " + v(0), nil
	case "name":
		return "", path, nil
	case "value":
		return "", v(0), nil
	default:
		return "", "", fmt.Errorf("unknown update operation %q", op)
	}
}
