// Package vectors loads the ElectroDB golden vectors recorded by
// tools/recordvectors (decisions D8, D52–D55) for wattdb's tests.
package vectors

import (
	"bytes"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"runtime"
	"sort"
)

// Vector is one recorded scenario.
type Vector struct {
	// File is the vector's path.
	File        string
	ElectroDB   string
	Description string
	Table       string
	// Entities are the scenario's entity schemas, in order.
	Entities []Entity
	Cases    []Case
}

// Entity is a named ElectroDB schema from a scenario.
type Entity struct {
	Name   string
	Schema json.RawMessage
}

// Case is one recorded case.
type Case struct {
	Name    string `json:"name"`
	Entity  string `json:"entity"`
	Service string `json:"service"`
	Chain   []Step `json:"-"`
	// Params is the wire form of .params(); nil when skipped or on error.
	Params      []Request `json:"-"`
	ParamsError *Error    `json:"-"`
	// Go is nil when .go() wasn't run.
	Go        *Go               `json:"-"`
	Value     json.RawMessage   `json:"-"`
	Error     *Error            `json:"error"`
	Responses []json.RawMessage `json:"-"`
}

// Step is one chain call.
type Step struct {
	Method string
	Args   []json.RawMessage
}

// Request is one request body sent to DynamoDB.
type Request struct {
	Target  string          `json:"target"`
	Request json.RawMessage `json:"request"`
}

// Go is the recorded outcome of .go().
type Go struct {
	Result   json.RawMessage `json:"result"`
	Error    *Error          `json:"error"`
	Requests []Request       `json:"requests"`
}

// Error is a recorded JS error.
type Error struct {
	Name    string `json:"name"`
	Message string `json:"message"`
	Code    *int   `json:"code"`
	Cause   *Error `json:"cause"`
}

// Dir returns testdata/vectors in the module root.
func Dir() string {
	return filepath.Join(moduleRoot(), "testdata", "vectors")
}

// PrivateDir returns the gitignored private/vectors directory in the module
// root. It may not exist.
func PrivateDir() string {
	return filepath.Join(moduleRoot(), "private", "vectors")
}

func moduleRoot() string {
	_, file, _, _ := runtime.Caller(0)
	return filepath.Join(filepath.Dir(file), "..", "..")
}

// LoadAll loads the committed vectors plus any private ones.
func LoadAll() ([]*Vector, error) {
	out, err := LoadDir(Dir())
	if err != nil {
		return nil, err
	}
	if _, err := os.Stat(PrivateDir()); err == nil {
		private, err := LoadDir(PrivateDir())
		if err != nil {
			return nil, err
		}
		out = append(out, private...)
	}
	return out, nil
}

// LoadDir loads every *.json vector in dir, sorted by name.
func LoadDir(dir string) ([]*Vector, error) {
	files, err := filepath.Glob(filepath.Join(dir, "*.json"))
	if err != nil {
		return nil, err
	}
	sort.Strings(files)
	var out []*Vector
	for _, f := range files {
		v, err := Load(f)
		if err != nil {
			return nil, err
		}
		out = append(out, v)
	}
	return out, nil
}

// Load reads one vector file.
func Load(path string) (*Vector, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}
	v, err := parse(data)
	if err != nil {
		return nil, fmt.Errorf("%s: %w", path, err)
	}
	v.File = path
	return v, nil
}

func parse(data []byte) (*Vector, error) {
	var raw struct {
		ElectroDB   string            `json:"electrodb"`
		Description string            `json:"description"`
		Table       string            `json:"table"`
		Entities    json.RawMessage   `json:"entities"`
		Cases       []json.RawMessage `json:"cases"`
	}
	if err := json.Unmarshal(data, &raw); err != nil {
		return nil, err
	}
	v := &Vector{ElectroDB: raw.ElectroDB, Description: raw.Description, Table: raw.Table}
	if v.Table == "" {
		v.Table = "table"
	}
	if len(raw.Entities) > 0 {
		members, err := objectMembers(raw.Entities)
		if err != nil {
			return nil, fmt.Errorf("entities: %w", err)
		}
		for _, m := range members {
			v.Entities = append(v.Entities, Entity{Name: m.Key, Schema: m.Value})
		}
	}
	for _, rc := range raw.Cases {
		c, err := parseCase(rc)
		if err != nil {
			return nil, err
		}
		v.Cases = append(v.Cases, c)
	}
	return v, nil
}

func parseCase(data json.RawMessage) (Case, error) {
	var c Case
	if err := json.Unmarshal(data, &c); err != nil {
		return c, err
	}
	var extra struct {
		Chain     [][]json.RawMessage `json:"chain"`
		Params    json.RawMessage     `json:"params"`
		Go        json.RawMessage     `json:"go"`
		Value     json.RawMessage     `json:"value"`
		Responses []json.RawMessage   `json:"responses"`
	}
	if err := json.Unmarshal(data, &extra); err != nil {
		return c, err
	}
	c.Value = extra.Value
	if g := bytes.TrimSpace(extra.Go); len(g) > 0 && g[0] == '{' {
		c.Go = &Go{}
		if err := json.Unmarshal(g, c.Go); err != nil {
			return c, fmt.Errorf("case %q: go: %w", c.Name, err)
		}
	}
	c.Responses = extra.Responses
	for _, step := range extra.Chain {
		if len(step) == 0 {
			return c, fmt.Errorf("case %q: empty chain step", c.Name)
		}
		var s Step
		if err := json.Unmarshal(step[0], &s.Method); err != nil {
			return c, fmt.Errorf("case %q: %w", c.Name, err)
		}
		s.Args = step[1:]
		c.Chain = append(c.Chain, s)
	}
	switch p := bytes.TrimSpace(extra.Params); {
	case len(p) == 0:
	case p[0] == '[':
		if err := json.Unmarshal(p, &c.Params); err != nil {
			return c, fmt.Errorf("case %q: params: %w", c.Name, err)
		}
	case p[0] == '{':
		var e struct {
			Error *Error `json:"error"`
		}
		if err := json.Unmarshal(p, &e); err != nil {
			return c, fmt.Errorf("case %q: params: %w", c.Name, err)
		}
		c.ParamsError = e.Error
	}
	return c, nil
}

// Requests returns the case's recorded requests: the params when recorded,
// otherwise the requests .go() sent.
func (c *Case) Requests() []Request {
	if c.Params != nil {
		return c.Params
	}
	if c.Go != nil {
		return c.Go.Requests
	}
	return nil
}
