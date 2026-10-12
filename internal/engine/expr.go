package engine

import (
	"fmt"
	"regexp"
	"strconv"
	"strings"
)

// This file ports ElectroDB's expression builders (operations.js, update.js,
// where.js): placeholder naming, update expressions and condition/filter
// expressions. Placeholders are generated exactly as ElectroDB generates
// them, so wattdb's requests can be compared with recorded ones.

var nonWord = regexp.MustCompile(`[^\w]`)

// isNaN mirrors JS isNaN(string).
func isNaN(s string) bool {
	_, ok := jsNumber(s)
	return !ok
}

// formatExpressionName mirrors formatExpressionName in operations.js.
func formatExpressionName(name string, seen map[string]string) string {
	nameWasNotANumber := isNaN(name)
	formatted := nonWord.ReplaceAllString(name, "")
	if formatted == "" {
		formatted = "p"
	} else if nameWasNotANumber != isNaN(formatted) {
		formatted = "p" + formatted
	}
	original := formatted
	suffix := 1
	for {
		prev, ok := seen[formatted]
		if !ok || prev == name {
			break
		}
		suffix++
		formatted = original + "_" + strconv.Itoa(suffix)
	}
	seen[formatted] = name
	return formatted
}

// paths is a name registered for an attribute path: its JSON path and its
// expression form.
type paths struct {
	json, expression, prop string
	root                   bool
}

// pathValue records a value placeholder for an attribute path.
type pathValue struct {
	value any
	name  string
}

// exprState mirrors ExpressionState.
type exprState struct {
	prefix    string
	names     map[string]string
	nameOrder []string
	values    map[string]any
	valOrder  []string
	paths     map[string]pathValue
	pathOrder []string
	counts    map[string]int
	impacted  map[string]string
	refs      map[string]any
	seenNames map[string]string
}

func newExprState(prefix string) *exprState {
	return &exprState{
		prefix:    prefix,
		names:     map[string]string{},
		values:    map[string]any{},
		paths:     map[string]pathValue{},
		counts:    map[string]int{},
		impacted:  map[string]string{},
		refs:      map[string]any{},
		seenNames: map[string]string{},
	}
}

func (s *exprState) setName(parent *paths, name, value string) paths {
	name = formatExpressionName(name, s.seenNames)
	prop := "#" + name
	switch {
	case parent == nil:
		s.addName(prop, value)
		return paths{json: name, expression: prop, prop: prop}
	case isNaN(name):
		s.addName(prop, value)
		return paths{json: parent.json + "." + name, expression: parent.expression + "." + prop, prop: prop}
	default:
		return paths{json: parent.json + "[" + name + "]", expression: parent.expression + "[" + name + "]", prop: prop}
	}
}

func (s *exprState) addName(prop, value string) {
	if _, ok := s.names[prop]; !ok {
		s.nameOrder = append(s.nameOrder, prop)
	}
	s.names[prop] = value
}

func (s *exprState) setValue(name string, value any) string {
	formatted := formatExpressionName(name, s.seenNames)
	n := s.counts[formatted]
	s.counts[formatted] = n + 1
	expr := ":" + formatted + s.prefix + strconv.Itoa(n)
	s.putValue(expr, value)
	return expr
}

func (s *exprState) putValue(expr string, value any) {
	if _, ok := s.values[expr]; !ok {
		s.valOrder = append(s.valOrder, expr)
	}
	s.values[expr] = value
}

func (s *exprState) setPath(path string, v pathValue) {
	if _, ok := s.paths[path]; !ok {
		s.pathOrder = append(s.pathOrder, path)
	}
	s.paths[path] = v
}

// updateExpr mirrors UpdateExpression.
type updateExpr struct {
	*exprState
	ops        map[string][]string // set, remove, add, subtract, delete
	composites map[string]any
	compOrder  []string
	seen       map[string]seenSet
}

type seenSet struct {
	name       paths
	value      string
	expression string
}

var updateOpOrder = []string{"set", "remove", "add", "subtract", "delete"}

func newUpdateExpr() *updateExpr {
	return &updateExpr{
		exprState:  newExprState("_u"),
		ops:        map[string][]string{},
		composites: map[string]any{},
		seen:       map[string]seenSet{},
	}
}

func (u *updateExpr) add(op, expression string) {
	for _, e := range u.ops[op] {
		if e == expression {
			return
		}
	}
	u.ops[op] = append(u.ops[op], expression)
}

func (u *updateExpr) unadd(op, expression string) {
	list := u.ops[op]
	for i, e := range list {
		if e == expression {
			u.ops[op] = append(list[:i:i], list[i+1:]...)
			return
		}
	}
}

func (u *updateExpr) addComposite(name string, value any) bool {
	if isUndefined(value) {
		return false
	}
	prev, ok := u.composites[name]
	if !ok {
		u.composites[name] = value
		u.compOrder = append(u.compOrder, name)
		return true
	}
	return prev == value
}

// set mirrors UpdateExpression.set: a SET on a top-level field, replacing an
// earlier set of the same field.
func (u *updateExpr) set(name string, value any, ifNotExists bool) {
	var n paths
	var v string
	if s, ok := u.seen[name]; ok {
		n, v = s.name, s.value
		u.unadd("set", s.expression)
	} else {
		n = u.setName(nil, name, name)
		v = u.setValue(name, value)
	}
	expression := n.prop + " = " + v
	if ifNotExists {
		expression = n.prop + " = if_not_exists(" + n.prop + ", " + v + ")"
	}
	u.seen[name] = seenSet{name: n, value: v, expression: expression}
	u.add("set", expression)
}

func (u *updateExpr) remove(name string) {
	n := u.setName(nil, name, name)
	u.add("remove", n.prop)
}

func (u *updateExpr) build() string {
	var parts []string
	for _, op := range updateOpOrder {
		if len(u.ops[op]) > 0 {
			parts = append(parts, strings.ToUpper(op)+" "+strings.Join(u.ops[op], ", "))
		}
	}
	return strings.Join(parts, " ")
}

// filterExpr mirrors FilterExpression (where.js).
type filterExpr struct {
	*exprState
	expression string
}

func newFilterExpr() *filterExpr {
	return &filterExpr{exprState: newExprState("")}
}

var newlines = regexp.MustCompile(`\n|\r`)
var emptyChars = regexp.MustCompile(`\n|\r|\w`)

func (f *filterExpr) addExpr(expression string, asPrefix bool) {
	existing := f.expression
	if existing == "" {
		f.expression = strings.TrimSpace(newlines.ReplaceAllString(expression, ""))
		return
	}
	expression = strings.TrimSpace(newlines.ReplaceAllString(expression, ""))
	if strings.TrimSpace(emptyChars.ReplaceAllString(expression, "")) == "" {
		return
	}
	if !asPrefix && !strings.HasPrefix(existing, "(") && !strings.HasSuffix(existing, ")") {
		existing = "(" + existing + ")"
	}
	if asPrefix {
		f.expression = "(" + expression + ") AND " + existing
	} else {
		f.expression = existing + " AND " + expression
	}
}

// unsafeSet mirrors FilterExpression.unsafeSet: a condition on a top-level
// field, without attribute checks.
func (f *filterExpr) unsafeSet(asPrefix bool, op, name string, values ...any) {
	n := f.setName(nil, name, name)
	refs := make([]string, len(values))
	for i, v := range values {
		refs[i] = f.setValue(name, v)
	}
	f.addExpr(filterTemplate(op, n.prop, refs...), asPrefix)
}

// filterTemplate mirrors the templates in filterOperations.js.
func filterTemplate(op, name string, values ...string) string {
	v := func(i int) string {
		if i < len(values) {
			return values[i]
		}
		return "undefined"
	}
	switch op {
	case "size":
		return "size(" + name + ")"
	case "type":
		return "attribute_type(" + name + ", " + v(0) + ")"
	case "ne":
		return name + " <> " + v(0)
	case "eq":
		return name + " = " + v(0)
	case "gt":
		return name + " > " + v(0)
	case "lt":
		return name + " < " + v(0)
	case "gte":
		return name + " >= " + v(0)
	case "lte":
		return name + " <= " + v(0)
	case "between":
		return "(" + name + " between " + v(0) + " and " + v(1) + ")"
	case "begins":
		return "begins_with(" + name + ", " + v(0) + ")"
	case "exists":
		return "attribute_exists(" + name + ")"
	case "notExists":
		return "attribute_not_exists(" + name + ")"
	case "contains":
		return "contains(" + name + ", " + v(0) + ")"
	case "notContains":
		return "not contains(" + name + ", " + v(0) + ")"
	case "eqOrNotExists":
		return "(" + name + " = " + v(0) + " OR attribute_not_exists(" + name + "))"
	case "value":
		return v(0)
	case "name":
		return name
	default:
		panic(fmt.Sprintf("engine: unknown filter operation %q", op))
	}
}
