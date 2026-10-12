package engine

import (
	"strconv"
)

// The engine works on untyped attribute values keyed by attribute name. Scalar
// values are string, Number and bool; the typed layer converts Go types to
// these before calling the engine.

// Number is a DynamoDB number kept in its string form, so integers of any
// size and numbers recorded from JS keep their exact text.
type Number string

// FloatNumber converts a float64 using strconv's shortest 'f' formatting
// (decision D56): identical to JS for integers and ordinary decimals, plain
// decimals where JS would switch to exponents.
func FloatNumber(f float64) Number {
	if f == 0 {
		return "0" // JS writes -0 as "0"
	}
	return Number(strconv.FormatFloat(f, 'f', -1, 64))
}

// IntNumber converts an int64.
func IntNumber(i int64) Number {
	return Number(strconv.FormatInt(i, 10))
}

// keyString is JS template-literal stringification of a key composite.
func keyString(v any) string {
	switch v := v.(type) {
	case string:
		return v
	case Number:
		return string(v)
	case bool:
		if v {
			return "true"
		}
		return "false"
	case nil:
		return "null"
	default:
		return "[object Object]"
	}
}
