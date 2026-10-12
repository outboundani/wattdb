// Package vectest loads wattdb's recorded ElectroDB vectors for the example
// module's tests.
package vectest

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"runtime"

	"github.com/aws/aws-sdk-go-v2/feature/dynamodb/attributevalue"
	"github.com/aws/aws-sdk-go-v2/service/dynamodb/types"
)

// Case is the part of a recorded case these tests use.
type Case struct {
	Name   string `json:"name"`
	Entity string `json:"entity"`
	// Method and Arg are the chain's first call and its first argument.
	Method string
	Arg    json.RawMessage
	// Item is the recorded put item; Rejected is set when ElectroDB failed.
	Item     map[string]types.AttributeValue
	Rejected string
	// Stored is the first scripted response's item; Data is what .go()
	// returned for it, as plain JSON-able values.
	Stored map[string]types.AttributeValue
	Data   any
}

// Load reads testdata/vectors/<name>.json from the wattdb repository, or
// from $WATTDB_VECTORS when set.
func Load(name string) ([]Case, error) {
	dir := os.Getenv("WATTDB_VECTORS")
	if dir == "" {
		_, file, _, _ := runtime.Caller(0)
		dir = filepath.Join(filepath.Dir(file), "../../../../../testdata/vectors")
	}
	path := filepath.Join(dir, name+".json")
	b, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}
	var raw struct {
		Cases []struct {
			Name      string              `json:"name"`
			Entity    string              `json:"entity"`
			Chain     [][]json.RawMessage `json:"chain"`
			Responses []struct {
				Item json.RawMessage `json:"Item"`
			} `json:"responses"`
			Recorded struct {
				Params json.RawMessage `json:"params"`
				Go     struct {
					Result json.RawMessage `json:"result"`
				} `json:"go"`
			} `json:"recorded"`
		} `json:"cases"`
	}
	if err := json.Unmarshal(b, &raw); err != nil {
		return nil, err
	}
	var out []Case
	for _, rc := range raw.Cases {
		c := Case{Name: rc.Name, Entity: rc.Entity}
		if len(rc.Chain) > 0 {
			json.Unmarshal(rc.Chain[0][0], &c.Method)
			if len(rc.Chain[0]) > 1 {
				c.Arg = rc.Chain[0][1]
			}
		}
		var reqs []struct {
			Request struct {
				Item json.RawMessage `json:"Item"`
			} `json:"request"`
		}
		var perr struct {
			Error *struct {
				Message string `json:"message"`
			} `json:"error"`
		}
		if json.Unmarshal(rc.Recorded.Params, &reqs) == nil && len(reqs) > 0 && reqs[0].Request.Item != nil {
			if c.Item, err = AttributeMap(reqs[0].Request.Item); err != nil {
				return nil, err
			}
		} else if json.Unmarshal(rc.Recorded.Params, &perr) == nil && perr.Error != nil {
			c.Rejected = perr.Error.Message
		}
		if len(rc.Responses) > 0 && rc.Responses[0].Item != nil {
			if c.Stored, err = AttributeMap(rc.Responses[0].Item); err != nil {
				return nil, err
			}
			result, err := AttributeValue(rc.Recorded.Go.Result)
			if err != nil {
				return nil, err
			}
			var r struct {
				Data any `dynamodbav:"data"`
			}
			if err := attributevalue.Unmarshal(result, &r); err != nil {
				return nil, err
			}
			c.Data = r.Data
		}
		out = append(out, c)
	}
	return out, nil
}

// AttributeValue decodes wire AttributeValue JSON.
func AttributeValue(data json.RawMessage) (types.AttributeValue, error) {
	var raw map[string]json.RawMessage
	if err := json.Unmarshal(data, &raw); err != nil {
		return nil, err
	}
	for typ, v := range raw {
		switch typ {
		case "S":
			var s string
			err := json.Unmarshal(v, &s)
			return &types.AttributeValueMemberS{Value: s}, err
		case "N":
			var s string
			err := json.Unmarshal(v, &s)
			return &types.AttributeValueMemberN{Value: s}, err
		case "BOOL":
			var b bool
			err := json.Unmarshal(v, &b)
			return &types.AttributeValueMemberBOOL{Value: b}, err
		case "NULL":
			return &types.AttributeValueMemberNULL{Value: true}, nil
		case "SS":
			var ss []string
			err := json.Unmarshal(v, &ss)
			return &types.AttributeValueMemberSS{Value: ss}, err
		case "NS":
			var ns []string
			err := json.Unmarshal(v, &ns)
			return &types.AttributeValueMemberNS{Value: ns}, err
		case "L":
			var items []json.RawMessage
			if err := json.Unmarshal(v, &items); err != nil {
				return nil, err
			}
			l := make([]types.AttributeValue, len(items))
			for i, it := range items {
				av, err := AttributeValue(it)
				if err != nil {
					return nil, err
				}
				l[i] = av
			}
			return &types.AttributeValueMemberL{Value: l}, nil
		case "M":
			m, err := AttributeMap(v)
			return &types.AttributeValueMemberM{Value: m}, err
		}
	}
	return nil, fmt.Errorf("vectest: can't decode %s", data)
}

// AttributeMap decodes an object of wire AttributeValues.
func AttributeMap(data json.RawMessage) (map[string]types.AttributeValue, error) {
	var raw map[string]json.RawMessage
	if err := json.Unmarshal(data, &raw); err != nil {
		return nil, err
	}
	out := map[string]types.AttributeValue{}
	for k, v := range raw {
		av, err := AttributeValue(v)
		if err != nil {
			return nil, err
		}
		out[k] = av
	}
	return out, nil
}
