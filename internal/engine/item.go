package engine

import (
	"fmt"
	"strings"
	"unicode/utf16"

	"github.com/outboundani/wattdb/internal/model"
)

// Undefined is JS's undefined; see model.Undefined.
var Undefined = model.Undefined

// Set is a DynamoDB set value: string, Number or []byte items, deduplicated.
type Set []any

func isUndefined(v any) bool { return v == Undefined }

// lookup returns attrs[name], or Undefined when it's missing.
func lookup(attrs map[string]any, name string) any {
	if v, ok := attrs[name]; ok {
		return v
	}
	return Undefined
}

// jsTypeof mirrors JS typeof for engine values.
func jsTypeof(v any) string {
	switch v.(type) {
	case string:
		return "string"
	case Number:
		return "number"
	case bool:
		return "boolean"
	default:
		if isUndefined(v) {
			return "undefined"
		}
		return "object"
	}
}

// valueType mirrors ElectroDB's getValueType.
func valueType(v any) string {
	switch v.(type) {
	case nil:
		return "null"
	case string:
		return "string"
	case Number:
		return "number"
	case bool:
		return "boolean"
	case []any:
		return "array"
	case Set:
		return "set"
	case map[string]any:
		return "object"
	default:
		if isUndefined(v) {
			return "undefined"
		}
		return "unknown"
	}
}

// JSString mirrors JS String(v) for engine values.
func JSString(v any) string {
	switch v := v.(type) {
	case []any:
		parts := make([]string, len(v))
		for i, e := range v {
			if e != nil && !isUndefined(e) {
				parts[i] = JSString(e)
			}
		}
		return strings.Join(parts, ",")
	case Set:
		return "[object Set]"
	default:
		if isUndefined(v) {
			return "undefined"
		}
		return keyString(v)
	}
}

// JSLength is a string's length in UTF-16 code units, as in JS.
func JSLength(s string) int {
	return len(utf16.Encode([]rune(s)))
}

func fieldError(a *model.Attribute, reason string) FieldError {
	return FieldError{Path: a.Path, Reason: reason}
}

func invalid(a *model.Attribute, reason string) error {
	return &ValidationError{Fields: []FieldError{fieldError(a, reason)}}
}

// defaultValue mirrors Attribute._makeDefault.
func defaultValue(a *model.Attribute) (any, error) {
	if a.Default == nil {
		return Undefined, nil
	}
	return a.Default()
}

// cast mirrors Attribute._makeCast.
func cast(a *model.Attribute, v any) (any, error) {
	switch a.Cast {
	case string(model.CastString):
		if isUndefined(v) {
			return nil, invalid(a, fmt.Sprintf("attribute %s is undefined and cannot be cast to type string", a.Name))
		}
		if s, ok := v.(string); ok {
			return s, nil
		}
		return jsonStringify(v), nil
	case string(model.CastNumber):
		if isUndefined(v) {
			return nil, invalid(a, fmt.Sprintf("attribute %s is undefined and cannot be cast to type number", a.Name))
		}
		if n, ok := v.(Number); ok {
			return n, nil
		}
		if n, ok := jsNumber(v); ok {
			return n, nil
		}
		return nil, invalid(a, fmt.Sprintf("attribute %s cannot be cast to type number; doing so results in NaN", a.Name))
	default:
		return v, nil
	}
}

// val mirrors the attribute classes' val(): casting and defaults, recursively
// for maps, lists and sets.
func val(a *model.Attribute, v any) (any, error) {
	switch a.Type {
	case model.TypeMap:
		data := v
		if isUndefined(v) {
			d, err := defaultValue(a)
			if err != nil {
				return nil, err
			}
			data = d
		}
		if isUndefined(data) {
			return Undefined, nil
		}
		obj, ok := data.(map[string]any)
		if !ok {
			return nil, invalid(a, fmt.Sprintf("Invalid value type at entity path: %q. Expected value to be an object to fulfill attribute type \"map\"", a.Path))
		}
		out := map[string]any{}
		for _, p := range a.Properties {
			r, err := val(p, lookup(obj, p.Name))
			if err != nil {
				return nil, err
			}
			if !isUndefined(r) {
				out[p.Name] = r
			}
		}
		return out, nil

	case model.TypeList:
		data := v
		if isUndefined(v) {
			c, err := cast(a, v)
			if err != nil {
				return nil, err
			}
			data = c
			if isUndefined(data) {
				if data, err = defaultValue(a); err != nil {
					return nil, err
				}
			}
		}
		if isUndefined(data) {
			return Undefined, nil
		}
		list, ok := data.([]any)
		if !ok {
			return nil, invalid(a, fmt.Sprintf("Invalid value type at entity path %q. Received value of type %q, expected value of type \"array\"", a.Path, valueType(v)))
		}
		out := []any{}
		for _, d := range list {
			r, err := val(a.Items, d)
			if err != nil {
				return nil, err
			}
			if !isUndefined(r) {
				out = append(out, r)
			}
		}
		return out, nil

	case model.TypeSet:
		if isUndefined(v) {
			d, err := defaultValue(a)
			if err != nil {
				return nil, err
			}
			v = d
		}
		if isUndefined(v) {
			return Undefined, nil
		}
		return toSet(a, v)

	default:
		v, err := cast(a, v)
		if err != nil {
			return nil, err
		}
		if isUndefined(v) {
			return defaultValue(a)
		}
		return v, nil
	}
}

// toSet mirrors SetAttribute.toDDBSet with the v3 DocumentClient: arrays and
// sets are deduplicated, and a single string or number becomes a one-item set.
func toSet(a *model.Attribute, v any) (any, error) {
	switch v := v.(type) {
	case Set:
		return dedupe(v), nil
	case []any:
		return dedupe(v), nil
	case string, Number:
		if _, err := getValidate(a.Items, v); err != nil {
			return nil, err
		}
		return Set{v}, nil
	default:
		return nil, invalid(a, fmt.Sprintf("Invalid attribute value supplied to \"set\" attribute %q. Received value of type %q. Set values must be supplied as either Arrays, native JavaScript Set objects, DocumentClient Set objects, strings, or numbers.", a.Path, valueType(v)))
	}
}

// dedupe keeps the first occurrence of each value, like new Set(array).
func dedupe(items []any) Set {
	out := Set{}
	seen := map[any]bool{}
	for _, it := range items {
		key := it
		if b, ok := it.([]byte); ok {
			key = "b:" + string(b)
		}
		if _, hashable := key.(map[string]any); hashable {
			out = append(out, it)
			continue
		}
		if _, isList := key.([]any); isList {
			out = append(out, it)
			continue
		}
		if seen[key] {
			continue
		}
		seen[key] = true
		out = append(out, it)
	}
	return out
}

// isValid mirrors Attribute.isValid: the type check, then validate when the
// type is right.
func isValid(a *model.Attribute, v any) (bool, []FieldError) {
	typed, reasons := isType(a, v)
	if !typed {
		return false, reasons
	}
	ok, verrs := validate(a, v)
	return ok, append(reasons, verrs...)
}

func isType(a *model.Attribute, v any) (bool, []FieldError) {
	if isUndefined(v) {
		if a.Required {
			return false, []FieldError{fieldError(a, fmt.Sprintf("Invalid value type at entity path: %q. Value is required.", a.Path))}
		}
		return true, nil
	}
	switch a.Type {
	case model.TypeEnum:
		s, ok := v.(string)
		if ok {
			for _, e := range a.Enum {
				if e == s {
					return true, nil
				}
			}
		}
		return false, []FieldError{fieldError(a, fmt.Sprintf("Invalid value type at entity path: %q. Value not found in set of acceptable values: %s", a.Path, quoteList(a.Enum)))}
	case model.TypeAny:
		return true, nil
	case model.TypeMap:
		obj, ok := v.(map[string]any)
		if !ok {
			return false, []FieldError{fieldError(a, fmt.Sprintf("Invalid value type at entity path %q. Received value of type %q, expected value of type \"object\"", a.Path, valueType(v)))}
		}
		var errs []FieldError
		for _, p := range a.Properties {
			if ok, e := isValid(p, lookup(obj, p.Name)); !ok {
				errs = append(errs, e...)
			}
		}
		return len(errs) == 0, errs
	case model.TypeList:
		list, ok := v.([]any)
		if !ok {
			return false, []FieldError{fieldError(a, fmt.Sprintf("Invalid value type at entity path %q. Received value of type %q, expected value of type \"array\"", a.Path, valueType(v)))}
		}
		var errs []FieldError
		for i, item := range list {
			if ok, e := isValid(a.Items, item); !ok {
				for _, fe := range e {
					idx := i
					fe.Index = &idx
					errs = append(errs, fe)
				}
			}
		}
		return len(errs) == 0, errs
	case model.TypeSet:
		var items []any
		switch v := v.(type) {
		case Set:
			items = v
		case []any:
			items = v
		default:
			return false, []FieldError{fieldError(a, fmt.Sprintf("Invalid value type at attribute path: %q. Expected value to be an Array, native JavaScript Set objects, or DocumentClient Set objects to fulfill attribute type \"set\"", a.Path))}
		}
		var errs []FieldError
		for _, item := range items {
			if ok, e := isValid(a.Items, item); !ok {
				errs = append(errs, e...)
			}
		}
		return len(errs) == 0, errs
	default:
		if got := jsTypeof(v); got != string(a.Type) {
			return false, []FieldError{fieldError(a, fmt.Sprintf("Invalid value type at entity path: %q. Received value of type %q, expected value of type %q", a.Path, got, a.Type))}
		}
		return true, nil
	}
}

// validate mirrors Attribute._makeValidate (and SetAttribute's, which passes
// the items as an array).
func validate(a *model.Attribute, v any) (bool, []FieldError) {
	if a.Type == model.TypeSet {
		if s, ok := v.(Set); ok {
			v = []any(s)
		}
	}
	switch {
	case a.Validate != nil:
		ok, err := a.Validate(v)
		if err != nil {
			fe := fieldError(a, err.Error())
			fe.Cause = err
			return false, []FieldError{fe}
		}
		if !ok {
			return false, []FieldError{fieldError(a, "Invalid value provided")}
		}
		return true, nil
	case a.ValidateRegexp != nil:
		if isUndefined(v) {
			return true, nil
		}
		if !a.ValidateRegexp.MatchString(JSString(v)) {
			return false, []FieldError{fieldError(a, fmt.Sprintf("Invalid value for attribute %q: Failed model defined regex", a.Path))}
		}
		return true, nil
	default:
		return true, nil
	}
}

// getValidate mirrors Attribute.getValidate: val, then isValid.
func getValidate(a *model.Attribute, v any) (any, error) {
	v, err := val(a, v)
	if err != nil {
		return nil, err
	}
	if ok, errs := isValid(a, v); !ok {
		return nil, &ValidationError{Fields: errs}
	}
	return v, nil
}

// CheckCreate mirrors Schema.checkCreate: every root attribute goes through
// cast, default and validation, stopping at the first invalid one. The
// record has an entry for every root attribute, Undefined when missing.
func CheckCreate(m *model.Model, payload map[string]any) (map[string]any, error) {
	record := make(map[string]any, len(m.Attributes))
	for _, a := range m.Attributes {
		v, err := getValidate(a, lookup(payload, a.Name))
		if err != nil {
			return nil, err
		}
		record[a.Name] = v
	}
	return record, nil
}

type method int

const (
	methodGet method = iota
	methodSet
)

func hasHandler(a *model.Attribute, mth method) bool {
	if mth == methodGet {
		return a.Get != nil
	}
	return a.Set != nil
}

// mutate runs an attribute's get or set, including the built-in handling of
// maps, lists and sets.
func mutate(a *model.Attribute, mth method, v any, siblings map[string]any) (any, error) {
	if mth == methodGet {
		return get(a, v, siblings)
	}
	return set(a, v, siblings)
}

func copyMap(m map[string]any) map[string]any {
	out := make(map[string]any, len(m))
	for k, v := range m {
		out[k] = v
	}
	return out
}

// set mirrors the attribute classes' _makeSet.
func set(a *model.Attribute, v any, siblings map[string]any) (any, error) {
	custom := func(v any) (any, error) {
		if a.Set == nil {
			return v, nil
		}
		return a.Set(v, siblings)
	}
	switch a.Type {
	case model.TypeMap:
		if isUndefined(v) {
			if a.Set == nil {
				return Undefined, nil
			}
			return custom(v)
		}
		obj, _ := v.(map[string]any)
		data := map[string]any{}
		for _, p := range a.Properties {
			pv := lookup(obj, p.Name)
			if isUndefined(pv) {
				continue
			}
			r, err := set(p, pv, copyMap(obj))
			if err != nil {
				return nil, err
			}
			if !isUndefined(r) {
				data[p.Field] = r
			}
		}
		return custom(data)

	case model.TypeList:
		if isUndefined(v) {
			return custom(v)
		}
		list, _ := v.([]any)
		data := []any{}
		for _, item := range list {
			r, err := set(a.Items, item, listSiblings(list))
			if err != nil {
				return nil, err
			}
			if !isUndefined(r) {
				data = append(data, r)
			}
		}
		return custom(data)

	case model.TypeSet:
		r, err := custom(fromSet(v))
		if err != nil || isUndefined(r) {
			return Undefined, err
		}
		return toSet(a, r)

	default:
		return custom(v)
	}
}

// listSiblings mirrors passing [...values] as a list item's siblings.
func listSiblings(list []any) map[string]any {
	out := make(map[string]any, len(list))
	for i, v := range list {
		out[fmt.Sprint(i)] = v
	}
	return out
}

// fromSet mirrors SetAttribute.fromDDBSet.
func fromSet(v any) any {
	if s, ok := v.(Set); ok {
		return []any(s)
	}
	return v
}

// get mirrors the attribute classes' _makeGet.
func get(a *model.Attribute, v any, siblings map[string]any) (any, error) {
	if a.Hidden {
		return Undefined, nil
	}
	custom := func(v any) (any, error) {
		if a.Get == nil {
			return v, nil
		}
		return a.Get(v, siblings)
	}
	switch a.Type {
	case model.TypeMap:
		if isUndefined(v) {
			if a.Get == nil {
				return Undefined, nil
			}
			return custom(map[string]any{})
		}
		obj, _ := v.(map[string]any)
		data := map[string]any{}
		for _, p := range a.Properties {
			pv := lookup(obj, p.Field)
			if isUndefined(pv) {
				continue
			}
			r, err := get(p, pv, copyMap(obj))
			if err != nil {
				return nil, err
			}
			if !isUndefined(r) {
				data[p.Name] = r
			}
		}
		return custom(data)

	case model.TypeList:
		if isUndefined(v) {
			return custom([]any{})
		}
		list, ok := v.([]any)
		if !ok {
			return custom(v)
		}
		data := []any{}
		for _, item := range list {
			r, err := get(a.Items, item, listSiblings(list))
			if err != nil {
				return nil, err
			}
			if !isUndefined(r) {
				data = append(data, r)
			}
		}
		return custom(data)

	case model.TypeSet:
		if !isUndefined(v) {
			return custom(fromSet(v))
		}
		r, err := custom(Undefined)
		if err != nil || isUndefined(r) {
			return Undefined, err
		}
		return fromSet(r), nil

	default:
		return custom(unformat(a, v))
	}
}

// unformat mirrors Attribute._makeDestructureKey: strings stored in a key
// field lose the key template's prefix and postfix.
func unformat(a *model.Attribute, v any) any {
	s, ok := v.(string)
	if !ok || (a.Type != model.TypeString && a.Type != model.TypeEnum) {
		return v
	}
	if len(s) <= len(a.KeyPrefix) {
		return s
	}
	return removeFixings(a.KeyPrefix, a.KeyPostfix, s)
}

func removeFixings(prefix, postfix, value string) string {
	if prefix == "" && postfix == "" {
		return value
	}
	lower := model.FormatCasing(value, model.Lower)
	start := 0
	if strings.HasPrefix(lower, model.FormatCasing(prefix, model.Lower)) {
		start = len(prefix)
	}
	end := len(value)
	if strings.HasSuffix(lower, model.FormatCasing(postfix, model.Lower)) {
		end -= len(postfix)
	}
	if end < start {
		return ""
	}
	return value[start:end]
}

// rootAttribute resolves a root payload key the way ElectroDB's traverser
// does: by attribute name, or by field.
func rootAttribute(m *model.Model, key string) *model.Attribute {
	if a, ok := m.Attribute(key); ok {
		return a
	}
	for _, a := range m.Attributes {
		if a.Field == key {
			return a
		}
	}
	return nil
}

// fulfill mirrors Schema._fulfillAttributeMutationMethod: non-watchers first,
// then the watchers of whatever is present, with the first pass's results
// as their siblings.
func fulfill(m *model.Model, mth method, payload map[string]any, order []string) (map[string]any, []string, error) {
	watchers := map[string]bool{}
	watchAll := []string{}
	toWatchers := map[string][]string{}
	hasWatchers := false
	for _, a := range m.Attributes {
		switch {
		case a.IsWatcher() && hasHandler(a, mth):
			hasWatchers = true
			watchers[a.Name] = true
		case a.WatchAll && hasHandler(a, mth):
			hasWatchers = true
			watchers[a.Name] = true
			watchAll = append(watchAll, a.Name)
		default:
			var ws []string
			for _, w := range a.WatchedBy {
				if wa, ok := m.Attribute(w); ok && hasHandler(wa, mth) {
					ws = append(ws, w)
				}
			}
			toWatchers[a.Name] = ws
		}
	}

	apply := func(include []string, avoid func(string) bool, payload map[string]any) (map[string]any, []string, error) {
		data := copyMap(payload)
		for _, key := range include {
			a := rootAttribute(m, key)
			if a == nil || avoid(key) {
				continue
			}
			r, err := mutate(a, mth, lookup(payload, key), copyMap(payload))
			if err != nil {
				return nil, nil, err
			}
			data[key] = r
		}
		keys := append([]string{}, order...)
		for _, k := range include {
			if !contains(keys, k) {
				keys = append(keys, k)
			}
		}
		return data, keys, nil
	}

	data, keys, err := apply(order, func(k string) bool { return watchers[k] }, payload)
	if err != nil || !hasWatchers {
		return data, keys, err
	}

	include := append([]string{}, keys...)
	for _, k := range keys {
		for _, w := range toWatchers[k] {
			if !contains(include, w) {
				include = append(include, w)
			}
		}
	}
	for _, w := range watchAll {
		if !contains(include, w) {
			include = append(include, w)
		}
	}
	return apply(include, func(k string) bool { _, ok := toWatchers[k]; return ok }, data)
}

func contains(list []string, s string) bool {
	for _, e := range list {
		if e == s {
			return true
		}
	}
	return false
}

// rootNames lists the root attribute names in order.
func rootNames(m *model.Model) []string {
	out := make([]string, len(m.Attributes))
	for i, a := range m.Attributes {
		out[i] = a.Name
	}
	return out
}

// PutItem builds the item a put or create writes, mirroring the put clause
// and _makePutParams: checkCreate, the primary key from the validated
// record, setters (watchers last), keys, field translation and identifiers.
func PutItem(m *model.Model, payload map[string]any) (map[string]any, error) {
	record, err := CheckCreate(m, payload)
	if err != nil {
		return nil, err
	}
	primary := Attrs{}
	var missing []string
	for _, name := range m.TableIndex().Composites() {
		v := record[name]
		if isUndefined(v) {
			missing = append(missing, name)
			continue
		}
		primary[name] = v
	}
	if len(missing) > 0 {
		return nil, &IncompleteCompositesError{Missing: missing}
	}

	applied, _, err := fulfill(m, methodSet, record, rootNames(m))
	if err != nil {
		return nil, err
	}
	defined := Attrs{}
	for k, v := range applied {
		if !isUndefined(v) {
			defined[k] = v
		}
	}
	keys, err := PutKeys(m, defined, primary)
	if err != nil {
		return nil, err
	}

	item := map[string]any{}
	for _, a := range m.Attributes {
		if v, ok := defined[a.Name]; ok {
			item[a.Field] = v
		}
	}
	for k, v := range keys {
		item[k] = v
	}
	item[m.Identifiers.Entity] = m.Entity
	item[m.Identifiers.Version] = m.Version
	return item, nil
}

// FormatOptions control read formatting.
type FormatOptions struct {
	// IncludeKeys keeps fields that aren't attributes (keys, identifiers).
	IncludeKeys bool
	// Attributes, when set, limits the result to these attributes.
	Attributes []string
}

// FormatItem mirrors Schema.formatItemForRetrieval: fields to attribute names,
// getters (watchers last), then hidden and unrequested attributes removed.
func FormatItem(m *model.Model, item map[string]any, opt FormatOptions) (map[string]any, error) {
	remapped := map[string]any{}
	var order []string
	byField := map[string]string{}
	for _, a := range m.Attributes {
		byField[a.Field] = a.Name
	}
	for field, v := range item {
		if name, ok := byField[field]; ok {
			remapped[name] = v
			order = append(order, name)
		} else if opt.IncludeKeys {
			remapped[field] = v
			order = append(order, field)
		}
	}
	data, _, err := fulfill(m, methodGet, remapped, order)
	if err != nil {
		return nil, err
	}
	out := map[string]any{}
	for k, v := range data {
		if isUndefined(v) {
			continue
		}
		if a, ok := m.Attribute(k); ok && a.Hidden {
			continue
		}
		if len(opt.Attributes) > 0 && !contains(opt.Attributes, k) {
			continue
		}
		out[k] = v
	}
	return out, nil
}

// OwnsItem reports whether an item's identifiers name this entity and version.
func OwnsItem(m *model.Model, item map[string]any) bool {
	return item[m.Identifiers.Entity] == m.Entity && item[m.Identifiers.Version] == m.Version
}

func quoteList(values []string) string {
	q := make([]string, len(values))
	for i, v := range values {
		q[i] = fmt.Sprintf("%q", v)
	}
	return strings.Join(q, ", ")
}
