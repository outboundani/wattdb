package engine

import (
	"errors"
	"fmt"

	"github.com/aws/aws-sdk-go-v2/service/dynamodb/types"
)

// ToAttributeValue converts an engine value to its wire form, as the v3
// DocumentClient marshals JS values with default options. Undefined map
// entries are skipped; an empty set is an error, as in JS.
func ToAttributeValue(v any) (types.AttributeValue, error) {
	switch v := v.(type) {
	case nil:
		return &types.AttributeValueMemberNULL{Value: true}, nil
	case string:
		return &types.AttributeValueMemberS{Value: v}, nil
	case Number:
		return &types.AttributeValueMemberN{Value: string(v)}, nil
	case bool:
		return &types.AttributeValueMemberBOOL{Value: v}, nil
	case []byte:
		return &types.AttributeValueMemberB{Value: v}, nil
	case []any:
		out := make([]types.AttributeValue, 0, len(v))
		for _, e := range v {
			if isUndefined(e) {
				continue
			}
			av, err := ToAttributeValue(e)
			if err != nil {
				return nil, err
			}
			out = append(out, av)
		}
		return &types.AttributeValueMemberL{Value: out}, nil
	case map[string]any:
		m, err := ToAttributeMap(v)
		if err != nil {
			return nil, err
		}
		return &types.AttributeValueMemberM{Value: m}, nil
	case Set:
		return setToAttributeValue(v)
	default:
		return nil, fmt.Errorf("wattdb: can't convert %T to an attribute value", v)
	}
}

// ToAttributeMap converts a map of engine values, skipping Undefined ones.
func ToAttributeMap(m map[string]any) (map[string]types.AttributeValue, error) {
	out := make(map[string]types.AttributeValue, len(m))
	for k, v := range m {
		if isUndefined(v) {
			continue
		}
		av, err := ToAttributeValue(v)
		if err != nil {
			return nil, fmt.Errorf("%s: %w", k, err)
		}
		out[k] = av
	}
	return out, nil
}

var errEmptySet = errors.New("wattdb: sets can't be empty")

func setToAttributeValue(s Set) (types.AttributeValue, error) {
	if len(s) == 0 {
		return nil, errEmptySet
	}
	switch s[0].(type) {
	case string:
		out := make([]string, len(s))
		for i, e := range s {
			v, ok := e.(string)
			if !ok {
				return nil, fmt.Errorf("wattdb: set mixes %T and %T", s[0], e)
			}
			out[i] = v
		}
		return &types.AttributeValueMemberSS{Value: out}, nil
	case Number:
		out := make([]string, len(s))
		for i, e := range s {
			v, ok := e.(Number)
			if !ok {
				return nil, fmt.Errorf("wattdb: set mixes %T and %T", s[0], e)
			}
			out[i] = string(v)
		}
		return &types.AttributeValueMemberNS{Value: out}, nil
	case []byte:
		out := make([][]byte, len(s))
		for i, e := range s {
			v, ok := e.([]byte)
			if !ok {
				return nil, fmt.Errorf("wattdb: set mixes %T and %T", s[0], e)
			}
			out[i] = v
		}
		return &types.AttributeValueMemberBS{Value: out}, nil
	default:
		return nil, fmt.Errorf("wattdb: sets can't hold %T", s[0])
	}
}

// FromAttributeValue converts a wire value to an engine value, as the v3
// DocumentClient unmarshals it (sets stay sets).
func FromAttributeValue(av types.AttributeValue) any {
	switch av := av.(type) {
	case *types.AttributeValueMemberS:
		return av.Value
	case *types.AttributeValueMemberN:
		return Number(av.Value)
	case *types.AttributeValueMemberBOOL:
		return av.Value
	case *types.AttributeValueMemberNULL:
		return nil
	case *types.AttributeValueMemberB:
		return av.Value
	case *types.AttributeValueMemberL:
		out := make([]any, len(av.Value))
		for i, e := range av.Value {
			out[i] = FromAttributeValue(e)
		}
		return out
	case *types.AttributeValueMemberM:
		return FromAttributeMap(av.Value)
	case *types.AttributeValueMemberSS:
		out := make(Set, len(av.Value))
		for i, e := range av.Value {
			out[i] = e
		}
		return out
	case *types.AttributeValueMemberNS:
		out := make(Set, len(av.Value))
		for i, e := range av.Value {
			out[i] = Number(e)
		}
		return out
	case *types.AttributeValueMemberBS:
		out := make(Set, len(av.Value))
		for i, e := range av.Value {
			out[i] = e
		}
		return out
	default:
		return Undefined
	}
}

// FromAttributeMap converts a wire item to engine values.
func FromAttributeMap(m map[string]types.AttributeValue) map[string]any {
	out := make(map[string]any, len(m))
	for k, v := range m {
		out[k] = FromAttributeValue(v)
	}
	return out
}
