package vectors

import (
	"bytes"
	"encoding/json"
	"fmt"

	"github.com/aws/aws-sdk-go-v2/service/dynamodb/types"
	"github.com/outboundani/wattdb/internal/engine"
)

// member is one key of a JSON object, in document order.
type member struct {
	Key   string
	Value json.RawMessage
}

// objectMembers returns an object's members in document order. ElectroDB
// schemas depend on key order, which encoding/json maps don't keep.
func objectMembers(data json.RawMessage) ([]member, error) {
	dec := json.NewDecoder(bytes.NewReader(data))
	tok, err := dec.Token()
	if err != nil {
		return nil, err
	}
	if d, ok := tok.(json.Delim); !ok || d != '{' {
		return nil, fmt.Errorf("expected an object")
	}
	var out []member
	for dec.More() {
		tok, err := dec.Token()
		if err != nil {
			return nil, err
		}
		var value json.RawMessage
		if err := dec.Decode(&value); err != nil {
			return nil, err
		}
		out = append(out, member{Key: tok.(string), Value: value})
	}
	return out, nil
}

// Value decodes JSON into engine values: numbers become engine.Number with
// their JSON text (which is JS's String(n) form), objects become
// map[string]any and arrays []any.
func Value(data json.RawMessage) (any, error) {
	dec := json.NewDecoder(bytes.NewReader(data))
	dec.UseNumber()
	var v any
	if err := dec.Decode(&v); err != nil {
		return nil, err
	}
	return toEngine(v), nil
}

// Attrs decodes a JSON object into engine attribute values.
func Attrs(data json.RawMessage) (engine.Attrs, error) {
	v, err := Value(data)
	if err != nil {
		return nil, err
	}
	m, ok := v.(map[string]any)
	if !ok {
		return nil, fmt.Errorf("expected an object")
	}
	return m, nil
}

func toEngine(v any) any {
	switch v := v.(type) {
	case json.Number:
		return engine.Number(v.String())
	case map[string]any:
		for k, e := range v {
			v[k] = toEngine(e)
		}
		return v
	case []any:
		for i, e := range v {
			v[i] = toEngine(e)
		}
		return v
	default:
		return v
	}
}

// AttributeValue decodes wire AttributeValue JSON ({"S": "x"}, {"N": "1"}, ...).
func AttributeValue(data json.RawMessage) (types.AttributeValue, error) {
	var raw map[string]json.RawMessage
	if err := json.Unmarshal(data, &raw); err != nil {
		return nil, err
	}
	if len(raw) != 1 {
		return nil, fmt.Errorf("attribute value needs exactly one type key: %s", data)
	}
	for typ, value := range raw {
		switch typ {
		case "S":
			var s string
			err := json.Unmarshal(value, &s)
			return &types.AttributeValueMemberS{Value: s}, err
		case "N":
			var s string
			err := json.Unmarshal(value, &s)
			return &types.AttributeValueMemberN{Value: s}, err
		case "B":
			var b []byte
			err := json.Unmarshal(value, &b)
			return &types.AttributeValueMemberB{Value: b}, err
		case "BOOL":
			var b bool
			err := json.Unmarshal(value, &b)
			return &types.AttributeValueMemberBOOL{Value: b}, err
		case "NULL":
			return &types.AttributeValueMemberNULL{Value: true}, nil
		case "SS":
			var ss []string
			err := json.Unmarshal(value, &ss)
			return &types.AttributeValueMemberSS{Value: ss}, err
		case "NS":
			var ns []string
			err := json.Unmarshal(value, &ns)
			return &types.AttributeValueMemberNS{Value: ns}, err
		case "BS":
			var bs [][]byte
			err := json.Unmarshal(value, &bs)
			return &types.AttributeValueMemberBS{Value: bs}, err
		case "L":
			var items []json.RawMessage
			if err := json.Unmarshal(value, &items); err != nil {
				return nil, err
			}
			out := make([]types.AttributeValue, len(items))
			for i, item := range items {
				av, err := AttributeValue(item)
				if err != nil {
					return nil, err
				}
				out[i] = av
			}
			return &types.AttributeValueMemberL{Value: out}, nil
		case "M":
			m, err := AttributeMap(value)
			return &types.AttributeValueMemberM{Value: m}, err
		default:
			return nil, fmt.Errorf("unknown attribute value type %q", typ)
		}
	}
	panic("unreachable")
}

// AttributeMap decodes a JSON object of wire AttributeValues.
func AttributeMap(data json.RawMessage) (map[string]types.AttributeValue, error) {
	var raw map[string]json.RawMessage
	if err := json.Unmarshal(data, &raw); err != nil {
		return nil, err
	}
	out := make(map[string]types.AttributeValue, len(raw))
	for k, v := range raw {
		av, err := AttributeValue(v)
		if err != nil {
			return nil, fmt.Errorf("%s: %w", k, err)
		}
		out[k] = av
	}
	return out, nil
}
