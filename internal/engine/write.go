package engine

import (
	"errors"
	"fmt"
	"slices"
	"strings"

	"github.com/outboundani/wattdb/internal/model"
)

// Method is a single-item operation.
type Method string

const (
	MethodGet    Method = "get"
	MethodPut    Method = "put"
	MethodCreate Method = "create"
	MethodDelete Method = "delete"
	MethodRemove Method = "remove"
	MethodUpdate Method = "update"
	MethodPatch  Method = "patch"
	MethodUpsert Method = "upsert"
	MethodCheck  Method = "check"
)

// Entry is one attribute path and value, as in ElectroDB's set({...}) etc.
type Entry struct {
	Path  string
	Value any
}

// DataOp is one operation of a data() callback.
type DataOp struct {
	Op     string // set, remove, add, subtract, append, delete, ifNotExists
	Path   Path
	Values []any
}

// Cond is a condition tree for where().
type Cond interface{ isCond() }

// CondOp is one ElectroDB where operation: eq, ne, gt, gte, lt, lte,
// between, begins, exists, notExists, contains, notContains, size, type.
type CondOp struct {
	Op     string
	Path   Path
	Values []any
}

// CondAnd, CondOr and CondNot combine conditions, rendered as
// "(a) AND (b)", "(a) OR (b)" and "NOT (a)".
type (
	CondAnd []Cond
	CondOr  []Cond
	CondNot struct{ C Cond }
	// CondRaw is a raw expression with its own placeholders.
	CondRaw struct {
		Expression string
		Names      map[string]string
		Values     map[string]any
	}
)

func (CondOp) isCond()  {}
func (CondAnd) isCond() {}
func (CondOr) isCond()  {}
func (CondNot) isCond() {}
func (CondRaw) isCond() {}

// Options are the execution options that shape a request.
type Options struct {
	Table      string
	Response   string // "", none, all_old, updated_old, all_new, updated_new
	Consistent bool
	// ReturnRejected is "", "true" or "all_old" (returnOnConditionCheckFailure).
	ReturnRejected string
	Attributes     []string
	IncludeKeys    bool
	// Exec is true when the request will be sent (".go()"), not just
	// rendered (".params()"); it changes the get projection.
	Exec bool
	// Transaction is set for transaction items: Response then sets
	// ReturnValuesOnConditionCheckFailure.
	Transaction bool
}

// Write builds one single-item operation, mirroring ElectroDB's chain state.
// Errors are kept and returned from Build (decision D49).
type Write struct {
	m      *model.Model
	method Method
	pk, sk Attrs
	record Attrs // put/create record after checkCreate
	upd    *updateExpr
	cond   *filterExpr
	upsert []upsertEntry
	err    error
	// include holds the key composites ElectroDB echoes in some responses.
	include Attrs
}

type upsertEntry struct {
	op    string
	name  string
	value any
}

func newWrite(m *model.Model, method Method) *Write {
	return &Write{m: m, method: method, upd: newUpdateExpr(), cond: newFilterExpr(), include: Attrs{}}
}

func (w *Write) fail(err error) *Write {
	if w.err == nil {
		w.err = err
	}
	return w
}

// expectFacets mirrors _expectFacets.
func expectFacets(obj Attrs, names []string) (Attrs, error) {
	out := Attrs{}
	var missing []string
	for _, n := range names {
		v, ok := obj[n]
		if !ok || isUndefined(v) {
			missing = append(missing, n)
			continue
		}
		out[n] = v
	}
	if len(missing) > 0 {
		return nil, &IncompleteCompositesError{Missing: missing}
	}
	return out, nil
}

func (w *Write) keyFacets(obj Attrs) {
	ix := w.m.TableIndex()
	pk, err := expectFacets(obj, ix.PK.Composites)
	if err != nil {
		w.fail(err)
		return
	}
	w.pk = pk
	sk, err := expectFacets(obj, ix.SK.Composites)
	if err != nil {
		w.fail(err)
		return
	}
	w.sk = sk
	w.include = mergeAttrs(sk, pk)
}

// NewGet starts a get.
func NewGet(m *model.Model, key Attrs) *Write {
	w := newWrite(m, MethodGet)
	w.keyFacets(key)
	w.include = Attrs{}
	return w
}

// NewCheck starts a transaction condition check.
func NewCheck(m *model.Model, key Attrs) *Write {
	w := newWrite(m, MethodCheck)
	w.keyFacets(key)
	w.include = Attrs{}
	return w
}

// NewPut starts a put (create when create is true).
func NewPut(m *model.Model, item Attrs, create bool) *Write {
	method := MethodPut
	if create {
		method = MethodCreate
	}
	w := newWrite(m, method)
	record, err := CheckCreate(m, item)
	if err != nil {
		return w.fail(err)
	}
	w.record = record
	if create {
		w.existsConditions("notExists")
	}
	w.keyFacets(record)
	w.include = Attrs{}
	return w
}

// NewDelete starts a delete (remove when remove is true).
func NewDelete(m *model.Model, key Attrs, remove bool) *Write {
	method := MethodDelete
	if remove {
		method = MethodRemove
		w := newWrite(m, method)
		w.existsConditions("exists")
		w.keyFacets(key)
		return w
	}
	w := newWrite(m, method)
	w.keyFacets(key)
	return w
}

// NewUpdate starts an update (patch when patch is true).
func NewUpdate(m *model.Model, key Attrs, patch bool) *Write {
	method := MethodUpdate
	if patch {
		method = MethodPatch
	}
	w := newWrite(m, method)
	if patch {
		w.existsConditions("exists")
	}
	w.keyFacets(key)
	return w
}

// NewUpsert starts an upsert with its "set" data.
func NewUpsert(m *model.Model, item Attrs, order []string) *Write {
	w := newWrite(m, MethodUpsert)
	w.addUpsert("set", item, order)
	return w
}

func (w *Write) existsConditions(op string) {
	ix := w.m.TableIndex()
	w.cond.unsafeSet(false, op, ix.PK.Field)
	if ix.HasSK {
		w.cond.unsafeSet(false, op, ix.SK.Field)
	}
}

func (w *Write) addUpsert(op string, data Attrs, order []string) {
	for _, name := range order {
		v, ok := data[name]
		if !ok {
			continue
		}
		replaced := false
		for i := range w.upsert {
			if w.upsert[i].name == name {
				w.upsert[i] = upsertEntry{op: op, name: name, value: v}
				replaced = true
			}
		}
		if !replaced {
			w.upsert = append(w.upsert, upsertEntry{op: op, name: name, value: v})
		}
	}
}

// checkUpdate mirrors Schema.checkUpdate: validate each provided value,
// rejecting readOnly attributes unless allowed.
func (w *Write) checkUpdate(entries []Entry, allowReadOnly bool) error {
	for _, e := range entries {
		a, ok := w.m.AttributeAtExactPath(e.Path)
		if !ok {
			continue
		}
		if a.ReadOnly && !allowReadOnly {
			return invalid(a, fmt.Sprintf("Attribute %q is Read-Only and cannot be updated", a.Path))
		}
		if _, err := getValidate(a, e.Value); err != nil {
			return err
		}
	}
	return nil
}

// performOperation mirrors AttributeOperationProxy.performOperation.
func (w *Write) performOperation(op string, path Path, value any, force bool) error {
	if isUndefined(value) {
		return nil
	}
	r, err := resolve(w.m, path)
	if err != nil {
		return err
	}
	if r.target == nil {
		return nil
	}
	if _, err := applyOp(w.m, w.upd.exprState, w.upd, op, path, value); err != nil {
		return err
	}
	if r.target.ReadOnly && !force {
		return invalid(r.target, fmt.Sprintf("Attribute %q is Read-Only and cannot be updated", r.target.Path))
	}
	return nil
}

// Apply runs an update operation over entries: set, add, subtract, append,
// delete or ifNotExists (ElectroDB's chain methods of the same names).
func (w *Write) Apply(op string, entries []Entry) *Write {
	if w.err != nil {
		return w
	}
	if w.method == MethodUpsert {
		if err := w.checkUpdate(entries, true); err != nil {
			return w.fail(err)
		}
		data := Attrs{}
		var order []string
		for _, e := range entries {
			data[e.Path] = e.Value
			order = append(order, e.Path)
		}
		w.addUpsert(op, data, order)
		return w
	}
	if err := w.checkUpdate(entries, false); err != nil {
		return w.fail(err)
	}
	for _, e := range entries {
		if err := w.performOperation(op, ParsePath(e.Path), e.Value, false); err != nil {
			return w.fail(err)
		}
	}
	return w
}

// Remove mirrors the update remove([...]) method.
func (w *Write) Remove(paths []string) *Write {
	if w.err != nil {
		return w
	}
	for _, p := range paths {
		a, ok := w.m.AttributeAtPath(p)
		switch {
		case !ok:
			return w.fail(&ValidationError{Fields: []FieldError{{Path: p, Reason: fmt.Sprintf("Attribute %q does not exist on model.", p)}}})
		case a.ReadOnly:
			return w.fail(invalid(a, fmt.Sprintf("Attribute %q is Read-Only and cannot be removed", a.Path)))
		case a.Required:
			return w.fail(invalid(a, fmt.Sprintf("Attribute %q is Required and cannot be removed", a.Path)))
		}
	}
	for _, p := range paths {
		r, err := resolve(w.m, ParsePath(p))
		if err != nil {
			return w.fail(err)
		}
		if r.target == nil {
			continue
		}
		if _, err := applyOp(w.m, w.upd.exprState, w.upd, "remove", ParsePath(p)); err != nil {
			return w.fail(err)
		}
	}
	return w
}

// Data mirrors data(): raw operations by path, each checked afterwards like
// ElectroDB's checkOperation (raw values skip the type check only when
// given through name/value, which wattdb doesn't expose here).
func (w *Write) Data(ops []DataOp) *Write {
	if w.err != nil {
		return w
	}
	for _, op := range ops {
		r, err := resolve(w.m, op.Path)
		if err != nil {
			return w.fail(err)
		}
		if r.target == nil {
			return w.fail(fmt.Errorf("invalid/unknown property passed to operation %q", op.Op))
		}
		if _, err := applyOp(w.m, w.upd.exprState, w.upd, op.Op, op.Path, op.Values...); err != nil {
			return w.fail(err)
		}
	}
	for _, path := range sortedKeys(w.upd.refs) {
		a := w.upd.refs[path].(*model.Attribute)
		operation := w.upd.impacted[path]
		var value any = Undefined
		if pv, ok := w.upd.paths[path]; ok {
			value = pv.value
		}
		if a.Required && operation == "remove" {
			return w.fail(invalid(a, fmt.Sprintf("Attribute %q is Required and cannot be removed", a.Path)))
		}
		if a.ReadOnly {
			return w.fail(invalid(a, fmt.Sprintf("Attribute %q is Read-Only and cannot be updated", a.Path)))
		}
		if !isUndefined(value) {
			if _, err := getValidate(a, value); err != nil {
				return w.fail(err)
			}
		}
	}
	return w
}

func sortedKeys[V any](m map[string]V) []string {
	out := make([]string, 0, len(m))
	for k := range m {
		out = append(out, k)
	}
	slices.Sort(out)
	return out
}

// Composite mirrors composite({...}): values for key composites that aren't
// being set, added as equality conditions.
func (w *Write) Composite(entries []Entry) *Write {
	if w.err != nil {
		return w
	}
	for _, e := range entries {
		if len(w.m.Facets(e.Path)) == 0 {
			continue
		}
		if !w.upd.addComposite(e.Path, e.Value) {
			return w.fail(fmt.Errorf("the composite attribute %s has been provided more than once with different values", e.Path))
		}
		if a, ok := w.m.Attribute(e.Path); ok {
			w.cond.unsafeSet(false, "eq", a.Field, e.Value)
		}
	}
	return w
}

// Where adds a condition (ElectroDB's where()).
func (w *Write) Where(c Cond) *Write {
	if w.err != nil {
		return w
	}
	expr, err := w.renderCond(c)
	if err != nil {
		return w.fail(err)
	}
	w.cond.addExpr(expr, false)
	return w
}

func (w *Write) renderCond(c Cond) (string, error) {
	switch c := c.(type) {
	case CondOp:
		return applyOp(w.m, w.cond.exprState, nil, c.Op, c.Path, c.Values...)
	case CondAnd, CondOr:
		list, sep := []Cond(nil), " AND "
		if a, ok := c.(CondAnd); ok {
			list = a
		} else {
			list, sep = c.(CondOr), " OR "
		}
		parts := make([]string, 0, len(list))
		for _, sub := range list {
			s, err := w.renderCond(sub)
			if err != nil {
				return "", err
			}
			parts = append(parts, "("+s+")")
		}
		return strings.Join(parts, sep), nil
	case CondNot:
		s, err := w.renderCond(c.C)
		if err != nil {
			return "", err
		}
		return "NOT (" + s + ")", nil
	case CondRaw:
		for k, v := range c.Names {
			w.cond.addName(k, v)
		}
		for k, v := range c.Values {
			w.cond.putValue(k, v)
		}
		return c.Expression, nil
	default:
		return "", fmt.Errorf("unknown condition %T", c)
	}
}

// Request is a built DynamoDB request in engine values.
type Request struct {
	// Target is the DynamoDB operation: GetItem, PutItem, DeleteItem or
	// UpdateItem.
	Target string
	// Params holds the request fields: TableName, Key, Item, the
	// expressions, ExpressionAttributeNames (map[string]string),
	// ExpressionAttributeValues (map[string]any) and plain options.
	Params map[string]any

	w    *Write
	opts Options
}

var returnValues = map[string]string{
	"none": "NONE", "all_old": "ALL_OLD", "updated_old": "UPDATED_OLD",
	"all_new": "ALL_NEW", "updated_new": "UPDATED_NEW",
}

// Build renders the request, mirroring ElectroDB's _params, option
// application and condition merging.
func (w *Write) Build(table string, opt Options) (*Request, error) {
	if w.err != nil {
		return nil, w.err
	}
	if opt.Table != "" {
		table = opt.Table
	}
	r := &Request{w: w, opts: opt, Params: map[string]any{}}
	var err error
	switch w.method {
	case MethodGet, MethodDelete, MethodRemove, MethodCheck:
		r.Target = map[Method]string{MethodGet: "GetItem", MethodDelete: "DeleteItem", MethodRemove: "DeleteItem", MethodCheck: "ConditionCheck"}[w.method]
		key, kerr := PrimaryKey(w.m, mergeAttrs(w.pk, w.sk))
		if kerr != nil {
			return nil, kerr
		}
		r.Params["Key"] = key
		r.Params["TableName"] = table
	case MethodPut, MethodCreate:
		r.Target = "PutItem"
		err = w.buildPut(r, table)
	case MethodUpdate, MethodPatch:
		r.Target = "UpdateItem"
		err = w.buildUpdate(r, table)
	case MethodUpsert:
		r.Target = "UpdateItem"
		err = w.buildUpsert(r, table)
	}
	if err != nil {
		return nil, err
	}

	// _applyParameterOptions
	if opt.Response != "" && opt.Response != "default" {
		rv, ok := returnValues[opt.Response]
		if !ok {
			return nil, fmt.Errorf("invalid response option %q", opt.Response)
		}
		if opt.Transaction {
			r.Params["ReturnValuesOnConditionCheckFailure"] = rv
		} else {
			r.Params["ReturnValues"] = rv
		}
	}
	if opt.ReturnRejected == "all_old" {
		r.Params["ReturnValuesOnConditionCheckFailure"] = "ALL_OLD"
	}
	if opt.Consistent {
		r.Params["ConsistentRead"] = true
	}
	if w.method == MethodGet {
		w.applyProjection(r)
	}

	// _applyParameterExpressionTypes
	if cond := w.cond.expression; cond != "" {
		if existing, ok := r.Params["ConditionExpression"].(string); ok && existing != "" {
			r.Params["ConditionExpression"] = existing + " AND " + cond
		} else {
			r.Params["ConditionExpression"] = cond
		}
		if len(w.cond.names) > 0 {
			names := map[string]string{}
			for k, v := range w.cond.names {
				names[k] = v
			}
			if existing, ok := r.Params["ExpressionAttributeNames"].(map[string]string); ok {
				for k, v := range existing {
					names[k] = v
				}
			}
			r.Params["ExpressionAttributeNames"] = names
		}
		if len(w.cond.values) > 0 {
			values := map[string]any{}
			for k, v := range w.cond.values {
				values[k] = v
			}
			if existing, ok := r.Params["ExpressionAttributeValues"].(map[string]any); ok {
				for k, v := range existing {
					values[k] = v
				}
			}
			r.Params["ExpressionAttributeValues"] = values
		}
	}
	return r, nil
}

func (w *Write) buildPut(r *Request, table string) error {
	item, err := putItem(w.m, w.record, mergeAttrs(w.pk, w.sk))
	if err != nil {
		return err
	}
	r.Params["Item"] = item
	r.Params["TableName"] = table
	return nil
}

// applyProjection mirrors _applyProjectionExpressions for get.
func (w *Write) applyProjection(r *Request) {
	if len(r.opts.Attributes) == 0 {
		return
	}
	ix := w.m.TableIndex()
	seen := map[string]string{}
	var order []string
	add := func(name, field string) {
		formatted := formatExpressionName(name, seen)
		if !slices.Contains(order, formatted) {
			order = append(order, formatted)
		}
		seen[formatted] = field
	}
	add(ix.PK.Field, ix.PK.Field)
	if ix.HasSK {
		add(ix.SK.Field, ix.SK.Field)
	}
	for _, name := range r.opts.Attributes {
		field := name
		if a, ok := w.m.Attribute(name); ok {
			field = a.Field
		}
		add(field, field)
	}
	if r.opts.Exec {
		order = append(order, w.m.Identifiers.Entity, w.m.Identifiers.Version)
		seen[w.m.Identifiers.Entity] = w.m.Identifiers.Entity
		seen[w.m.Identifiers.Version] = w.m.Identifiers.Version
	}
	names := map[string]string{}
	refs := make([]string, len(order))
	for i, k := range order {
		names["#"+k] = seen[k]
		refs[i] = "#" + k
	}
	r.Params["ExpressionAttributeNames"] = names
	r.Params["ProjectionExpression"] = strings.Join(refs, ", ")
}

func (w *Write) names() map[string]string {
	out := map[string]string{}
	for k, v := range w.upd.names {
		out[k] = v
	}
	return out
}

func (w *Write) values() map[string]any {
	out := map[string]any{}
	for k, v := range w.upd.values {
		out[k] = v
	}
	return out
}

// buildUpdate mirrors _makeUpdateParams.
func (w *Write) buildUpdate(r *Request, table string) error {
	m := w.m
	ix := m.TableIndex()
	modifiedValues := map[string]any{}
	modifiedNames := map[string]string{}
	var order []string
	for _, path := range w.upd.pathOrder {
		pv := w.upd.paths[path]
		modifiedValues[path] = pv.value
		modifiedNames[path] = pv.name
		order = append(order, path)
	}
	removed := Attrs{}
	for path, op := range w.upd.impacted {
		if op == "remove" {
			removed[path] = path
		}
	}
	for k := range mergeAttrs(w.pk, w.sk) {
		delete(modifiedValues, k)
		order = slices.DeleteFunc(order, func(s string) bool { return s == k })
	}
	prepared, preparedOrder, err := fulfill(m, methodSet, modifiedValues, order)
	if err != nil {
		return err
	}

	indexKey, updatedKeys, updatedOrder, deletedKeys, err := w.updatedKeys(prepared, removed)
	if err != nil {
		return err
	}

	for _, path := range preparedOrder {
		v, ok := prepared[path]
		if !ok || isUndefined(v) {
			continue
		}
		if name, ok := modifiedNames[path]; ok {
			w.upd.putValue(name, v)
		} else if a, ok := m.AttributeAtPath(path); ok {
			w.upd.set(a.Field, v, false)
		} else {
			w.upd.set(path, v, false)
		}
	}
	notTableKey := func(field string) bool {
		_, modified := modifiedNames[field]
		return field != ix.PK.Field && (!ix.HasSK || field != ix.SK.Field) && !modified
	}
	for _, field := range updatedOrder {
		if notTableKey(field) {
			w.upd.set(field, updatedKeys[field], false)
		}
	}
	for _, field := range deletedKeys {
		if notTableKey(field) {
			w.upd.remove(field)
		}
	}
	for _, name := range append(append([]string{}, ix.PK.Composites...), ix.SK.Composites...) {
		v, ok := mergeAttrs(w.pk, w.sk)[name]
		if !ok {
			continue
		}
		a, ok := m.Attribute(name)
		if !ok {
			continue
		}
		_, modified := modifiedNames[name]
		if a.Field != ix.PK.Field && (!ix.HasSK || a.Field != ix.SK.Field) && !modified {
			w.upd.set(a.Field, v, false)
		}
	}
	w.upd.set(m.Identifiers.Entity, m.Entity, false)
	w.upd.set(m.Identifiers.Version, m.Version, false)

	r.Params["UpdateExpression"] = w.upd.build()
	r.Params["ExpressionAttributeNames"] = w.names()
	r.Params["ExpressionAttributeValues"] = w.values()
	r.Params["TableName"] = table
	r.Params["Key"] = indexKey
	return nil
}

// updatedKeys mirrors _getUpdatedKeys and _makeKeysFromAttributes.
func (w *Write) updatedKeys(set Attrs, removed Attrs) (indexKey map[string]any, updated map[string]any, updatedOrder []string, deleted []string, err error) {
	m := w.m
	table := m.TableIndex()
	keyAttrs := mergeAttrs(w.sk, w.pk)
	composites := Attrs{}
	for _, n := range w.upd.compOrder {
		composites[n] = w.upd.composites[n]
	}
	defined := Attrs{}
	for k, v := range set {
		if !isUndefined(v) {
			defined[k] = v
		}
	}

	complete, err := expectIndexFacets(m, defined, mergeAttrs(composites, keyAttrs), impactOptions{includedOnly: true})
	if err != nil {
		return nil, nil, nil, nil, err
	}
	removedImpact, err := expectIndexFacets(m, removed, keyAttrs, impactOptions{skipConditions: true})
	if err != nil {
		return nil, nil, nil, nil, err
	}

	types := complete.types
	order := complete.order
	if types[table] == nil {
		types[table] = map[model.KeyType]string{model.PK: "pk", model.SK: "sk"}
		order = append(order, table)
	}
	attrs := mergeAttrs(composites, defined, keyAttrs)

	for _, ix := range m.Indexes {
		if keep, ok := complete.conditions[ix]; ok && !keep {
			deleted = append(deleted, ix.PK.Field)
			if ix.HasSK {
				deleted = append(deleted, ix.SK.Field)
			}
		}
	}
	for _, ix := range removedImpact.order {
		for _, kt := range []model.KeyType{model.PK, model.SK} {
			if f, ok := removedImpact.types[ix][kt]; ok {
				deleted = append(deleted, f)
			}
		}
	}

	indexKey = map[string]any{}
	updated = map[string]any{}
	for _, ix := range order {
		kt := types[ix]
		if ix.Type == model.Composite {
			continue
		}
		if ix.Condition != nil && !complete.conditions[ix] && ix != table {
			continue
		}
		keys, err := indexKeys(m, ix, attrs, []Attrs{attrs}, keyOptions{})
		if err != nil {
			return nil, nil, nil, nil, err
		}
		_, hasPK := kt[model.PK]
		_, hasSK := kt[model.SK]
		pk := any(nil)
		if hasPK && truthy(keys.PK) {
			pk = keys.PK
		}
		var sks []any
		if hasSK && len(keys.SK) > 0 {
			sks = keys.SK
		}
		if ix == table {
			indexKey[ix.PK.Field] = pk
			if ix.HasSK {
				if len(sks) > 0 {
					indexKey[ix.SK.Field] = sks[0]
				}
			}
		} else if len(sks) == 0 && ix.HasSK && len(ix.SK.Composites) == 0 {
			// Sparse indexes whose sk has no composites still need the sk.
			key := ix.SK.Prefix
			if ix.SK.HasPostfix {
				key += ix.SK.Postfix
			} else {
				key += "undefined"
			}
			if key != "" {
				sks = append(sks, key)
			}
		}
		if truthy(pk) {
			updated[ix.PK.Field] = pk
			updatedOrder = append(updatedOrder, ix.PK.Field)
		}
		if ix.HasSK && len(sks) > 0 && truthy(sks[0]) {
			updated[ix.SK.Field] = sks[0]
			updatedOrder = append(updatedOrder, ix.SK.Field)
		}
	}
	return indexKey, updated, updatedOrder, deleted, nil
}

// truthy mirrors JS truthiness for key values.
func truthy(v any) bool {
	switch v := v.(type) {
	case nil:
		return false
	case string:
		return v != ""
	case Number:
		return v != "0" && v != "-0"
	default:
		return !isUndefined(v)
	}
}

// buildUpsert mirrors the upsert clause's beforeBuildParams and
// _makeUpsertParams.
func (w *Write) buildUpsert(r *Request, table string) error {
	m := w.m
	w.upd.set(m.Identifiers.Entity, m.Entity, false)
	w.upd.set(m.Identifiers.Version, m.Version, false)

	setData := Attrs{}
	nonSet := []upsertEntry{}
	allData := Attrs{}
	for _, e := range w.upsert {
		allData[e.name] = e.value
		if e.op == "set" {
			setData[e.name] = e.value
		} else {
			nonSet = append(nonSet, e)
		}
	}
	upsertData, err := CheckCreate(m, allData)
	if err != nil {
		return err
	}
	w.keyFacets(upsertData)
	if w.err != nil {
		return w.err
	}
	// ElectroDB adds the key composites to the response options after they
	// were read, so an upsert's default result is an empty object.
	w.include = Attrs{}
	applied, _, err := fulfill(m, methodSet, upsertData, rootNames(m))
	if err != nil {
		return err
	}
	onlySet := Attrs{}
	for _, a := range m.Attributes {
		v, ok := applied[a.Name]
		if !ok {
			continue
		}
		_, isSet := setData[a.Name]
		_, provided := allData[a.Name]
		if (isSet || !provided) && !isUndefined(v) {
			onlySet[a.Name] = v
		}
	}

	updatedKeys, deletedKeys, err := putKeys(m, onlySet, mergeAttrs(w.sk, w.pk))
	if err != nil {
		return err
	}
	for _, k := range deletedKeys {
		w.upd.remove(k)
	}
	key, err := PrimaryKey(m, mergeAttrs(w.pk, w.sk))
	if err != nil {
		return err
	}

	// setAttributes come back in attribute order; then index keys.
	type field struct {
		name  string
		value any
	}
	var setFields []field
	for _, a := range m.Attributes {
		if v, ok := onlySet[a.Name]; ok {
			setFields = append(setFields, field{a.Field, v})
		}
	}
	for _, ix := range m.Indexes {
		for _, f := range []string{ix.PK.Field, ix.SK.Field} {
			v, ok := updatedKeys[f]
			if !ok {
				continue
			}
			if _, isKey := key[f]; isKey {
				setFields = slices.DeleteFunc(setFields, func(x field) bool { return x.name == f })
				continue
			}
			found := false
			for i := range setFields {
				if setFields[i].name == f {
					setFields[i].value, found = v, true
				}
			}
			if !found {
				setFields = append(setFields, field{f, v})
			}
		}
	}
	for _, f := range setFields {
		if err := w.applyUpsertField(f.name, f.value, "set"); err != nil {
			return err
		}
	}
	for _, e := range nonSet {
		v, ok := applied[e.name]
		if !ok || isUndefined(v) {
			continue
		}
		a, ok := m.Attribute(e.name)
		if !ok {
			continue
		}
		if err := w.applyUpsertField(a.Field, v, e.op); err != nil {
			return err
		}
	}

	r.Params["TableName"] = table
	r.Params["UpdateExpression"] = w.upd.build()
	r.Params["ExpressionAttributeNames"] = w.names()
	r.Params["ExpressionAttributeValues"] = w.values()
	r.Params["Key"] = key
	return nil
}

// applyUpsertField mirrors _maybeApplyUpsertUpdate.
func (w *Write) applyUpsertField(field string, value any, op string) error {
	var attr *model.Attribute
	for _, a := range w.m.Attributes {
		if a.Field == field {
			attr = a
		}
	}
	if attr == nil {
		w.upd.set(field, value, op == "ifNotExists")
		return nil
	}
	if attr.ReadOnly && len(w.m.Facets(attr.Name)) == 0 {
		w.upd.set(field, value, true)
		return nil
	}
	return w.performOperation(op, Path{attr.Name}, value, true)
}

// putItem builds the put item from a validated record (PutItem without the
// checkCreate step).
func putItem(m *model.Model, record, primary Attrs) (map[string]any, error) {
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

// Response is a formatted single-item result.
type Response struct {
	// Data is the formatted item; nil (with Null set) when ElectroDB returns
	// null.
	Data map[string]any
	Null bool
	// Rejected is set when ReturnRejected was requested and the condition
	// failed.
	Rejected bool
}

// Format mirrors executeOperation's formatting of a successful response.
// resp holds the response fields in engine values ("Item", "Attributes").
func (r *Request) Format(resp map[string]any) (Response, error) {
	w := r.w
	switch rv, _ := r.Params["ReturnValues"].(string); rv {
	case "NONE":
		return Response{Null: true}, nil
	case "ALL_NEW", "ALL_OLD", "UPDATED_NEW", "UPDATED_OLD":
		return r.formatResponse(resp, false)
	}
	switch w.method {
	case MethodPut, MethodCreate:
		return r.formatResponse(map[string]any{"Item": r.Params["Item"]}, false)
	case MethodGet:
		return r.formatResponse(resp, false)
	default:
		return r.formatResponse(resp, true)
	}
}

// FormatRejected mirrors the condition-failure branch of executeOperation.
// item is the returned old item, or nil.
func (r *Request) FormatRejected(item map[string]any) (Response, error) {
	if r.opts.ReturnRejected != "all_old" || item == nil {
		return Response{Rejected: true, Null: r.opts.ReturnRejected == "all_old"}, nil
	}
	resp, err := r.formatResponse(map[string]any{"Item": item}, false)
	resp.Rejected = true
	return resp, err
}

// formatResponse mirrors formatResponse for single items.
func (r *Request) formatResponse(resp map[string]any, objectOnEmpty bool) (Response, error) {
	w := r.w
	opt := FormatOptions{IncludeKeys: r.opts.IncludeKeys, Attributes: r.opts.Attributes}
	if item, ok := resp["Item"].(map[string]any); ok {
		if OwnsItem(w.m, item) {
			data, err := FormatItem(w.m, item, opt)
			return Response{Data: data}, err
		}
		if objectOnEmpty {
			return Response{Data: map[string]any{}}, nil
		}
		return Response{Null: true}, nil
	}
	if attrs, ok := resp["Attributes"].(map[string]any); ok {
		data, err := FormatItem(w.m, attrs, opt)
		if err != nil {
			return Response{}, err
		}
		if len(data) == 0 && !objectOnEmpty {
			return Response{Null: true}, nil
		}
		return Response{Data: data}, nil
	}
	if objectOnEmpty {
		data := map[string]any{}
		for k, v := range w.include {
			data[k] = v
		}
		return Response{Data: data}, nil
	}
	return Response{Null: true}, nil
}

// ErrRejected is returned by callers when a condition check failed.
var ErrRejected = errors.New("wattdb: condition check failed")

// RejectRequested reports whether condition failures should be returned as
// a rejected result instead of an error.
func (r *Request) RejectRequested() bool { return r.opts.ReturnRejected != "" }

// RawUpdate adds a raw clause to one section of the update expression
// ("set", "remove", "add", "delete"), with its own names and values.
func (w *Write) RawUpdate(section, clause string, names map[string]string, values map[string]any) *Write {
	if w.err != nil {
		return w
	}
	switch section {
	case "set", "remove", "add", "delete":
	default:
		return w.fail(fmt.Errorf("raw update: unknown section %q", section))
	}
	for k, v := range names {
		w.upd.addName(k, v)
	}
	for k, v := range values {
		w.upd.putValue(k, v)
	}
	w.upd.add(section, clause)
	return w
}
