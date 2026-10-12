package vectors

import (
	"encoding/json"
	"fmt"

	"github.com/outboundani/wattdb/internal/engine"
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
func (f *Fn) Condition() (func(engine.Attrs) bool, error) {
	switch f.Name {
	case "has":
		attr, err := f.stringArg(0)
		if err != nil {
			return nil, err
		}
		return func(a engine.Attrs) bool {
			_, ok := a[attr]
			return ok
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
		return func(a engine.Attrs) bool {
			v, ok := a[attr]
			return ok && v == want
		}, nil
	default:
		return nil, fmt.Errorf("$fn %q is not a condition", f.Name)
	}
}
