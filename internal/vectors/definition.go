package vectors

import (
	"bytes"
	"encoding/json"
	"fmt"

	"github.com/outboundani/wattdb/internal/model"
)

// Definition converts the entity's embedded ElectroDB schema into a model
// Definition, resolving named callbacks through the registry.
func (e Entity) Definition() (model.Definition, error) {
	var raw struct {
		Model struct {
			Entity  string `json:"entity"`
			Version string `json:"version"`
			Service string `json:"service"`
		} `json:"model"`
		Attributes json.RawMessage `json:"attributes"`
		Indexes    json.RawMessage `json:"indexes"`
	}
	if err := json.Unmarshal(e.Schema, &raw); err != nil {
		return model.Definition{}, err
	}
	def := model.Definition{
		Entity:  raw.Model.Entity,
		Version: raw.Model.Version,
		Service: raw.Model.Service,
	}
	attrs, err := objectMembers(raw.Attributes)
	if err != nil {
		return def, fmt.Errorf("attributes: %w", err)
	}
	for _, m := range attrs {
		a, err := attributeDef(m.Key, m.Value)
		if err != nil {
			return def, fmt.Errorf("attribute %q: %w", m.Key, err)
		}
		def.Attributes = append(def.Attributes, a)
	}
	indexes, err := objectMembers(raw.Indexes)
	if err != nil {
		return def, fmt.Errorf("indexes: %w", err)
	}
	for _, m := range indexes {
		ix, err := indexDef(m.Key, m.Value)
		if err != nil {
			return def, fmt.Errorf("index %q: %w", m.Key, err)
		}
		def.Indexes = append(def.Indexes, ix)
	}
	return def, nil
}

// Model builds the entity's normalized model.
func (e Entity) Model() (*model.Model, error) {
	def, err := e.Definition()
	if err != nil {
		return nil, err
	}
	return model.New(def)
}

func attributeDef(name string, data json.RawMessage) (model.AttributeDef, error) {
	a := model.AttributeDef{Name: name}
	// Shorthand: "attr": "string" or "attr": ["a", "b"].
	if t := bytes.TrimSpace(data); len(t) > 0 && t[0] != '{' {
		data = json.RawMessage(`{"type":` + string(t) + `}`)
	}
	var raw struct {
		Type       json.RawMessage `json:"type"`
		Field      string          `json:"field"`
		Label      *string         `json:"label"`
		Required   bool            `json:"required"`
		Hidden     bool            `json:"hidden"`
		ReadOnly   bool            `json:"readOnly"`
		Padding    *model.Padding  `json:"padding"`
		Cast       string          `json:"cast"`
		Default    json.RawMessage `json:"default"`
		Get        *Fn             `json:"get"`
		Set        *Fn             `json:"set"`
		Validate   *Fn             `json:"validate"`
		Watch      json.RawMessage `json:"watch"`
		Items      json.RawMessage `json:"items"`
		Properties json.RawMessage `json:"properties"`
	}
	if err := json.Unmarshal(data, &raw); err != nil {
		return a, err
	}
	a.Field = raw.Field
	a.Label = raw.Label
	a.Required = raw.Required
	a.Hidden = raw.Hidden
	a.ReadOnly = raw.ReadOnly
	a.Padding = raw.Padding
	a.Cast = raw.Cast

	if t := bytes.TrimSpace(raw.Type); len(t) > 0 && t[0] == '[' {
		a.Type = model.TypeEnum
		if err := json.Unmarshal(t, &a.Enum); err != nil {
			return a, err
		}
	} else if len(t) > 0 {
		if err := json.Unmarshal(t, &a.Type); err != nil {
			return a, err
		}
	}

	if d := bytes.TrimSpace(raw.Default); len(d) > 0 {
		def, err := defaultFn(d)
		if err != nil {
			return a, fmt.Errorf("default: %w", err)
		}
		a.Default = def
	}
	var err error
	if raw.Get != nil {
		if a.Get, err = raw.Get.Transform(); err != nil {
			return a, fmt.Errorf("get: %w", err)
		}
	}
	if raw.Set != nil {
		if a.Set, err = raw.Set.Transform(); err != nil {
			return a, fmt.Errorf("set: %w", err)
		}
	}
	if raw.Validate != nil {
		if raw.Validate.Name == "regexp" {
			if a.ValidateRegexp, err = raw.Validate.Regexp(); err != nil {
				return a, fmt.Errorf("validate: %w", err)
			}
		} else if a.Validate, err = raw.Validate.Validator(); err != nil {
			return a, fmt.Errorf("validate: %w", err)
		}
	}
	if w := bytes.TrimSpace(raw.Watch); len(w) > 0 {
		if w[0] == '"' {
			var s string
			if err := json.Unmarshal(w, &s); err != nil {
				return a, err
			}
			if s != "*" {
				return a, fmt.Errorf("watch must be an array or \"*\"")
			}
			a.WatchAll = true
		} else if err := json.Unmarshal(w, &a.Watch); err != nil {
			return a, err
		}
	}

	if items := bytes.TrimSpace(raw.Items); len(items) > 0 {
		it, err := attributeDef("", items)
		if err != nil {
			return a, fmt.Errorf("items: %w", err)
		}
		a.Items = &it
	}
	if len(raw.Properties) > 0 {
		props, err := objectMembers(raw.Properties)
		if err != nil {
			return a, err
		}
		a.Properties = []model.AttributeDef{}
		for _, p := range props {
			pa, err := attributeDef(p.Key, p.Value)
			if err != nil {
				return a, fmt.Errorf("%s: %w", p.Key, err)
			}
			a.Properties = append(a.Properties, pa)
		}
	}
	return a, nil
}

// defaultFn resolves a default: a $fn, or a constant (decoded fresh on each
// call, so callers can't share mutable values).
func defaultFn(data json.RawMessage) (func() any, error) {
	var fn Fn
	if data[0] == '{' && json.Unmarshal(data, &fn) == nil && fn.Name != "" {
		return fn.Default()
	}
	if _, err := Value(data); err != nil {
		return nil, err
	}
	return func() any {
		v, _ := Value(data)
		return v
	}, nil
}

func indexDef(pattern string, data json.RawMessage) (model.IndexDef, error) {
	ix := model.IndexDef{AccessPattern: pattern}
	var raw struct {
		Index      string           `json:"index"`
		Type       model.IndexType  `json:"type"`
		Collection json.RawMessage  `json:"collection"`
		Scope      string           `json:"scope"`
		Condition  *Fn              `json:"condition"`
		PK         model.KeyDef     `json:"pk"`
		SK         *model.KeyDef    `json:"sk"`
		Projection *json.RawMessage `json:"projection"`
	}
	if err := json.Unmarshal(data, &raw); err != nil {
		return ix, err
	}
	ix.Index = raw.Index
	ix.Type = raw.Type
	ix.Scope = raw.Scope
	ix.PK = raw.PK
	ix.SK = raw.SK

	if c := bytes.TrimSpace(raw.Collection); len(c) > 0 {
		if c[0] == '[' {
			ix.CollectionArray = true
			if err := json.Unmarshal(c, &ix.Collection); err != nil {
				return ix, err
			}
		} else {
			var s string
			if err := json.Unmarshal(c, &s); err != nil {
				return ix, err
			}
			ix.Collection = []string{s}
		}
	}

	if raw.Condition != nil {
		cond, err := raw.Condition.Condition()
		if err != nil {
			return ix, err
		}
		ix.Condition = cond
	}

	if raw.Projection != nil {
		p := bytes.TrimSpace(*raw.Projection)
		proj := &model.Projection{}
		if len(p) > 0 && p[0] == '[' {
			if err := json.Unmarshal(p, &proj.Attributes); err != nil {
				return ix, err
			}
		} else if err := json.Unmarshal(p, &proj.Kind); err != nil {
			return ix, err
		}
		ix.Projection = proj
	}
	return ix, nil
}
