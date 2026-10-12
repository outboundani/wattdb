package wattdb

import (
	"errors"
	"fmt"
	"reflect"
	"regexp"

	"github.com/aws/aws-sdk-go-v2/service/dynamodb/types"
	"github.com/outboundani/wattdb/attr"
	"github.com/outboundani/wattdb/codec"
	"github.com/outboundani/wattdb/index"
	"github.com/outboundani/wattdb/internal/engine"
	"github.com/outboundani/wattdb/internal/model"
)

// Hooks carry a schema's callbacks, adapted by generated code from their
// typed form (decision D11) to DynamoDB values. A nil value means missing;
// returning nil leaves the attribute out. Attribute hooks are keyed by Go
// name path ("Profile.DisplayName", "Steps[*].Name"); conditions by access
// pattern.
//
// Generated code builds Hooks; applications don't.
type Hooks struct {
	Defaults   map[string]func() (types.AttributeValue, error)
	Gets       map[string]func(v types.AttributeValue, siblings map[string]types.AttributeValue) (types.AttributeValue, error)
	Sets       map[string]func(v types.AttributeValue, siblings map[string]types.AttributeValue) (types.AttributeValue, error)
	Validates  map[string]func(v types.AttributeValue) error
	Conditions map[string]func(item map[string]types.AttributeValue) (bool, error)
}

// EntityDef is a compiled entity schema. Generated packages create one when
// db/runtime registers the schema.
type EntityDef struct {
	model *model.Model
}

// Compile checks a schema and compiles it with its callbacks. Generated code
// calls it; applications don't.
func Compile(s Schema, h Hooks) (*EntityDef, error) {
	def, err := definition(s, &h, true)
	if err != nil {
		return nil, err
	}
	m, err := model.New(def)
	if err != nil {
		return nil, fmt.Errorf("wattdb: entity %q: %w", def.Entity, err)
	}
	return &EntityDef{model: m}, nil
}

// Check validates a schema the way ElectroDB validates a model, without
// callbacks. The generator and `wattdb check` use it.
func Check(s Schema) error {
	def, err := definition(s, nil, false)
	if err != nil {
		return err
	}
	if _, err := model.New(def); err != nil {
		return fmt.Errorf("wattdb: entity %q: %w", def.Entity, err)
	}
	return nil
}

// compiler translates descriptors (Go names) into a model definition
// (fields), attaching hooks.
type compiler struct {
	hooks *Hooks
	// needHooks makes a callback without a hook an error; Check skips hooks.
	needHooks bool
	used      map[string]bool
}

func definition(s Schema, h *Hooks, needHooks bool) (model.Definition, error) {
	md := s.Model()
	def := model.Definition{Entity: md.Entity, Version: md.Version, Service: md.Service.name}
	if md.Service == (ServiceDef{}) {
		return def, fmt.Errorf("wattdb: entity %q: Model.Service must be declared with wattdb.Service", md.Entity)
	}
	c := &compiler{hooks: h, needHooks: needHooks, used: map[string]bool{}}

	var descs []*attr.Descriptor
	for _, a := range s.Attributes() {
		descs = append(descs, a.Descriptor())
	}
	fields := map[string]string{}
	for _, d := range descs {
		fields[d.Name] = fieldOf(d)
	}
	for _, d := range descs {
		a, err := c.attribute(d, d.Name, fields)
		if err != nil {
			return def, fmt.Errorf("wattdb: entity %q: %w", md.Entity, err)
		}
		def.Attributes = append(def.Attributes, a)
	}
	for _, ix := range s.Indexes() {
		d, err := c.index(ix.Descriptor(), fields)
		if err != nil {
			return def, fmt.Errorf("wattdb: entity %q: %w", md.Entity, err)
		}
		def.Indexes = append(def.Indexes, d)
	}
	if h != nil {
		if err := c.checkUnused(); err != nil {
			return def, fmt.Errorf("wattdb: entity %q: %w", md.Entity, err)
		}
	}
	return def, nil
}

func fieldOf(d *attr.Descriptor) string {
	if d.Field != "" {
		return d.Field
	}
	return d.Name
}

// attribute compiles one descriptor. siblings maps the Go names at its level
// to fields, for Watch.
func (c *compiler) attribute(d *attr.Descriptor, path string, siblings map[string]string) (model.AttributeDef, error) {
	if d.Err != nil {
		return model.AttributeDef{}, d.Err
	}
	a := model.AttributeDef{
		Name:           fieldOf(d),
		Field:          fieldOf(d),
		Label:          d.Label,
		Required:       d.Required,
		Hidden:         d.Hidden,
		ReadOnly:       d.ReadOnly,
		ValidateRegexp: d.ValidateRegexp,
		WatchAll:       d.WatchAll,
		Enum:           d.Enum,
	}
	if d.Padding != nil {
		a.Padding = &model.Padding{Length: d.Padding.Length, Char: d.Padding.Char}
	}
	for _, w := range d.Watch {
		f, ok := siblings[w]
		if !ok {
			return a, fmt.Errorf("attribute %q watches unknown attribute %q", path, w)
		}
		a.Watch = append(a.Watch, f)
	}

	typ, err := attrType(d)
	if err != nil {
		return a, fmt.Errorf("attribute %q: %w", path, err)
	}
	a.Type = typ

	switch d.Kind {
	case attr.KindList:
		if d.Items == nil {
			return a, fmt.Errorf("attribute %q: list needs items", path)
		}
		items, err := c.attribute(d.Items, path+"[*]", nil)
		if err != nil {
			return a, err
		}
		items.Name, items.Field = "", ""
		a.Items = &items
	case attr.KindSet:
		itemType := model.TypeString
		if d.GoType != nil && d.GoType.Kind() != reflect.String {
			itemType = model.TypeNumber
		}
		a.Items = &model.AttributeDef{Type: itemType}
	case attr.KindMap:
		props := map[string]string{}
		for _, p := range d.Props {
			props[p.Name] = fieldOf(p)
		}
		a.Properties = []model.AttributeDef{}
		for _, p := range d.Props {
			pa, err := c.attribute(p, path+"."+p.Name, props)
			if err != nil {
				return a, err
			}
			a.Properties = append(a.Properties, pa)
		}
	}

	if err := c.attachHooks(&a, d, path); err != nil {
		return a, err
	}
	return a, nil
}

// attrType maps a constructor (and codec) to an ElectroDB attribute type.
func attrType(d *attr.Descriptor) (model.AttrType, error) {
	switch d.Kind {
	case attr.KindString:
		return model.TypeString, nil
	case attr.KindInt, attr.KindInt64, attr.KindFloat64:
		return model.TypeNumber, nil
	case attr.KindBool:
		return model.TypeBoolean, nil
	case attr.KindEnum:
		return model.TypeEnum, nil
	case attr.KindSet:
		return model.TypeSet, nil
	case attr.KindList:
		return model.TypeList, nil
	case attr.KindMap:
		return model.TypeMap, nil
	case attr.KindAny, attr.KindAnyOf:
		return model.TypeAny, nil
	case attr.KindTime, attr.KindDuration, attr.KindUUID, attr.KindCustom:
		c := d.Codec
		if c == nil {
			switch d.Kind {
			case attr.KindTime:
				c = codec.Time
			case attr.KindDuration:
				c = codec.Duration
			case attr.KindUUID:
				c = codec.UUIDBinary
			default:
				return "", errors.New("Custom needs a codec")
			}
		}
		typed, ok := c.(interface{ Type() codec.Type })
		if !ok {
			return "", fmt.Errorf("codec %T doesn't implement codec.Codec", c)
		}
		switch typed.Type() {
		case codec.String:
			return model.TypeString, nil
		case codec.Number:
			return model.TypeNumber, nil
		case codec.Boolean:
			return model.TypeBoolean, nil
		default:
			return model.TypeAny, nil
		}
	default:
		return "", fmt.Errorf("unknown attribute kind %q", d.Kind)
	}
}

func (c *compiler) attachHooks(a *model.AttributeDef, d *attr.Descriptor, path string) error {
	needs := map[string]bool{
		"default":  d.HasDefault || d.DefaultFunc != nil,
		"get":      d.Get != nil,
		"set":      d.Set != nil,
		"validate": d.Validate != nil,
	}
	if c.hooks == nil {
		// Check: callbacks are present but not attached; mark them so the
		// model still sees which attributes have them.
		if needs["default"] {
			a.Default = func() (any, error) { return engine.Undefined, nil }
		}
		if needs["get"] {
			a.Get = func(v any, _ map[string]any) (any, error) { return v, nil }
		}
		if needs["set"] {
			a.Set = func(v any, _ map[string]any) (any, error) { return v, nil }
		}
		if needs["validate"] {
			a.Validate = func(any) (bool, error) { return true, nil }
		}
		return nil
	}
	h := c.hooks
	if needs["default"] {
		fn, ok := h.Defaults[path]
		if !ok {
			return c.missingHook("default", path)
		}
		c.used["default:"+path] = true
		a.Default = func() (any, error) {
			av, err := fn()
			if err != nil || av == nil {
				return engine.Undefined, err
			}
			return engine.FromAttributeValue(av), nil
		}
	}
	if needs["get"] {
		fn, ok := h.Gets[path]
		if !ok {
			return c.missingHook("get", path)
		}
		c.used["get:"+path] = true
		a.Get = transform(fn)
	}
	if needs["set"] {
		fn, ok := h.Sets[path]
		if !ok {
			return c.missingHook("set", path)
		}
		c.used["set:"+path] = true
		a.Set = transform(fn)
	}
	if needs["validate"] {
		fn, ok := h.Validates[path]
		if !ok {
			return c.missingHook("validate", path)
		}
		c.used["validate:"+path] = true
		a.Validate = func(v any) (bool, error) {
			av, err := toAV(v)
			if err != nil {
				return false, err
			}
			if err := fn(av); err != nil {
				return false, err
			}
			return true, nil
		}
	}
	return nil
}

func (c *compiler) missingHook(kind, path string) error {
	if !c.needHooks {
		return nil
	}
	return fmt.Errorf("attribute %q has a %s callback but no generated hook; run wattdb generate", path, kind)
}

func (c *compiler) checkUnused() error {
	check := func(kind string, keys []string) error {
		for _, k := range keys {
			if !c.used[kind+":"+k] {
				return fmt.Errorf("generated %s hook for %q has no callback in the schema; run wattdb generate", kind, k)
			}
		}
		return nil
	}
	h := c.hooks
	for kind, keys := range map[string][]string{
		"default":   mapKeys(h.Defaults),
		"get":       mapKeys(h.Gets),
		"set":       mapKeys(h.Sets),
		"validate":  mapKeys(h.Validates),
		"condition": mapKeys(h.Conditions),
	} {
		if err := check(kind, keys); err != nil {
			return err
		}
	}
	return nil
}

func mapKeys[V any](m map[string]V) []string {
	out := make([]string, 0, len(m))
	for k := range m {
		out = append(out, k)
	}
	return out
}

func transform(fn func(types.AttributeValue, map[string]types.AttributeValue) (types.AttributeValue, error)) func(any, map[string]any) (any, error) {
	return func(v any, siblings map[string]any) (any, error) {
		av, err := toAV(v)
		if err != nil {
			return nil, err
		}
		sib, err := toAVMap(siblings)
		if err != nil {
			return nil, err
		}
		out, err := fn(av, sib)
		if err != nil || out == nil {
			return engine.Undefined, err
		}
		return engine.FromAttributeValue(out), nil
	}
}

// toAV converts an engine value for a hook; missing is nil.
func toAV(v any) (types.AttributeValue, error) {
	if v == engine.Undefined {
		return nil, nil
	}
	return engine.ToAttributeValue(v)
}

func toAVMap(m map[string]any) (map[string]types.AttributeValue, error) {
	return engine.ToAttributeMap(m)
}

var templateVar = regexp.MustCompile(`\$\{([^}]*)\}`)

func (c *compiler) key(k index.Key, fields map[string]string, pattern string) (model.KeyDef, error) {
	kd := model.KeyDef{Field: k.Field, Casing: string(k.Casing), Cast: string(k.Cast)}
	if k.Composites != nil {
		kd.Composite = []string{}
		for _, name := range k.Composites {
			f, ok := fields[name]
			if !ok {
				return kd, fmt.Errorf("access pattern %q: unknown composite attribute %q", pattern, name)
			}
			kd.Composite = append(kd.Composite, f)
		}
	}
	if k.Template != nil {
		var err error
		t := templateVar.ReplaceAllStringFunc(*k.Template, func(m string) string {
			name := m[2 : len(m)-1]
			f, ok := fields[name]
			if !ok {
				err = fmt.Errorf("access pattern %q: template names unknown attribute %q", pattern, name)
				return m
			}
			return "${" + f + "}"
		})
		if err != nil {
			return kd, err
		}
		kd.Template = &t
	}
	return kd, nil
}

func (c *compiler) index(d *index.Descriptor, fields map[string]string) (model.IndexDef, error) {
	ix := model.IndexDef{
		AccessPattern:   d.AccessPattern,
		Index:           d.Index,
		Type:            model.IndexType(d.Type),
		Scope:           d.Scope,
		Collection:      d.Collection,
		CollectionArray: d.SubCollections,
	}
	pk, err := c.key(d.PK, fields, d.AccessPattern)
	if err != nil {
		return ix, err
	}
	ix.PK = pk
	if d.SK != nil {
		sk, err := c.key(*d.SK, fields, d.AccessPattern)
		if err != nil {
			return ix, err
		}
		ix.SK = &sk
	}
	if d.Projection != nil {
		p := &model.Projection{Kind: d.Projection.Kind}
		for _, name := range d.Projection.Attributes {
			f, ok := fields[name]
			if !ok {
				return ix, fmt.Errorf("access pattern %q: unknown projection attribute %q", d.AccessPattern, name)
			}
			p.Attributes = append(p.Attributes, f)
		}
		ix.Projection = p
	}
	if d.Condition != nil {
		if c.hooks == nil {
			ix.Condition = func(map[string]any) (bool, error) { return true, nil }
		} else {
			fn, ok := c.hooks.Conditions[d.AccessPattern]
			if !ok {
				if c.needHooks {
					return ix, fmt.Errorf("access pattern %q has a condition but no generated hook; run wattdb generate", d.AccessPattern)
				}
			} else {
				c.used["condition:"+d.AccessPattern] = true
				ix.Condition = func(item map[string]any) (bool, error) {
					av, err := toAVMap(item)
					if err != nil {
						return false, err
					}
					return fn(av)
				}
			}
		}
	}
	return ix, nil
}

// PutItem builds the item a put writes from an encoded Input, running
// defaults, validation, setters and keys. Generated code uses it.
func (d *EntityDef) PutItem(in map[string]types.AttributeValue) (map[string]types.AttributeValue, error) {
	item, err := engine.PutItem(d.model, engine.FromAttributeMap(in))
	if err != nil {
		return nil, err
	}
	return engine.ToAttributeMap(item)
}

// FormatItem formats a stored item for reading (getters, hidden attributes)
// and reports whether this entity owns it. Generated code uses it.
func (d *EntityDef) FormatItem(item map[string]types.AttributeValue) (map[string]types.AttributeValue, bool, error) {
	values := engine.FromAttributeMap(item)
	if !engine.OwnsItem(d.model, values) {
		return nil, false, nil
	}
	out, err := engine.FormatItem(d.model, values, engine.FormatOptions{})
	if err != nil {
		return nil, true, err
	}
	av, err := engine.ToAttributeMap(out)
	return av, true, err
}
