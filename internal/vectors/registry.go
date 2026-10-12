package vectors

import (
	"encoding/json"
	"errors"
	"fmt"
	"regexp"
	"strings"

	"github.com/outboundani/wattdb/internal/engine"
	"github.com/outboundani/wattdb/internal/model"
)

// Fn is a named callback, {"$fn": "<name>", "args": [...]}. Its semantics
// mirror tools/recordvectors/registry.mjs exactly.
type Fn struct {
	Name string            `json:"$fn"`
	Args []json.RawMessage `json:"args"`
}

func (f *Fn) stringArg(i int) (string, error) {
	if i >= len(f.Args) {
		return "", fmt.Errorf("$fn %q: missing argument %d", f.Name, i)
	}
	var s string
	if err := json.Unmarshal(f.Args[i], &s); err != nil {
		return "", fmt.Errorf("$fn %q: argument %d: %w", f.Name, i, err)
	}
	return s, nil
}

// Condition resolves an index condition callback.
func (f *Fn) Condition() (func(engine.Attrs) (bool, error), error) {
	switch f.Name {
	case "has":
		attr, err := f.stringArg(0)
		if err != nil {
			return nil, err
		}
		return func(a engine.Attrs) (bool, error) {
			_, ok := a[attr]
			return ok, nil
		}, nil
	case "equals":
		attr, err := f.stringArg(0)
		if err != nil {
			return nil, err
		}
		if len(f.Args) < 2 {
			return nil, fmt.Errorf("$fn %q: missing value", f.Name)
		}
		want, err := Value(f.Args[1])
		if err != nil {
			return nil, err
		}
		return func(a engine.Attrs) (bool, error) {
			v, ok := a[attr]
			return ok && v == want, nil
		}, nil
	default:
		return nil, fmt.Errorf("$fn %q is not a condition", f.Name)
	}
}

func (f *Fn) intArg(i int) (int, error) {
	if i >= len(f.Args) {
		return 0, fmt.Errorf("$fn %q: missing argument %d", f.Name, i)
	}
	var n int
	if err := json.Unmarshal(f.Args[i], &n); err != nil {
		return 0, fmt.Errorf("$fn %q: argument %d: %w", f.Name, i, err)
	}
	return n, nil
}

// Default resolves a default callback.
func (f *Fn) Default() (func() (any, error), error) {
	switch f.Name {
	case "const":
		if len(f.Args) < 1 {
			return nil, fmt.Errorf("$fn %q: missing value", f.Name)
		}
		return defaultFn(f.Args[0])
	default:
		return nil, fmt.Errorf("$fn %q is not a default", f.Name)
	}
}

// Transform resolves a get or set callback.
func (f *Fn) Transform() (func(any, map[string]any) (any, error), error) {
	switch f.Name {
	case "upper", "lower":
		casing := strings.ToUpper
		if f.Name == "lower" {
			casing = strings.ToLower
		}
		return func(v any, _ map[string]any) (any, error) {
			if s, ok := v.(string); ok {
				return casing(s), nil
			}
			return v, nil
		}, nil
	case "prefix":
		p, err := f.stringArg(0)
		if err != nil {
			return nil, err
		}
		return func(v any, _ map[string]any) (any, error) {
			if v == model.Undefined {
				return v, nil
			}
			return p + engine.JSString(v), nil
		}, nil
	case "join":
		sep, err := f.stringArg(0)
		if err != nil {
			return nil, err
		}
		var attrs []string
		for i := 1; i < len(f.Args); i++ {
			a, err := f.stringArg(i)
			if err != nil {
				return nil, err
			}
			attrs = append(attrs, a)
		}
		return func(_ any, item map[string]any) (any, error) {
			parts := make([]string, len(attrs))
			for i, a := range attrs {
				v, ok := item[a]
				if ok && v != nil && v != model.Undefined {
					parts[i] = engine.JSString(v)
				}
			}
			return strings.Join(parts, sep), nil
		}, nil
	default:
		return nil, fmt.Errorf("$fn %q is not a get/set transform", f.Name)
	}
}

// Validator resolves a validate callback.
func (f *Fn) Validator() (func(any) (bool, error), error) {
	switch f.Name {
	case "minLength":
		n, err := f.intArg(0)
		if err != nil {
			return nil, err
		}
		return func(v any) (bool, error) {
			return v == model.Undefined || engine.JSLength(engine.JSString(v)) >= n, nil
		}, nil
	case "throws":
		msg, err := f.stringArg(0)
		if err != nil {
			return nil, err
		}
		return func(any) (bool, error) { return false, errors.New(msg) }, nil
	default:
		return nil, fmt.Errorf("$fn %q is not a validator", f.Name)
	}
}

// Regexp resolves {"$fn": "regexp", "args": [source, flags]}.
func (f *Fn) Regexp() (*regexp.Regexp, error) {
	src, err := f.stringArg(0)
	if err != nil {
		return nil, err
	}
	if len(f.Args) > 1 {
		flags, err := f.stringArg(1)
		if err != nil {
			return nil, err
		}
		if strings.Contains(flags, "i") {
			src = "(?i)" + src
		}
	}
	return regexp.Compile(src)
}
