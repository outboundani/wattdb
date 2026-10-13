package engine

import (
	"encoding/json"
	"math"
	"sort"
	"strconv"
	"strings"
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

// jsonStringify mirrors JSON.stringify for engine values.
func jsonStringify(v any) string {
	switch v := v.(type) {
	case nil:
		return "null"
	case string:
		b, _ := json.Marshal(v)
		return string(b)
	case Number:
		return string(v)
	case bool:
		return strconv.FormatBool(v)
	case []any:
		parts := make([]string, len(v))
		for i, e := range v {
			if isUndefined(e) {
				parts[i] = "null"
			} else {
				parts[i] = jsonStringify(e)
			}
		}
		return "[" + strings.Join(parts, ",") + "]"
	case map[string]any:
		keys := make([]string, 0, len(v))
		for k, e := range v {
			if !isUndefined(e) {
				keys = append(keys, k)
			}
		}
		sort.Strings(keys)
		parts := make([]string, len(keys))
		for i, k := range keys {
			kb, _ := json.Marshal(k)
			parts[i] = string(kb) + ":" + jsonStringify(v[k])
		}
		return "{" + strings.Join(parts, ",") + "}"
	default:
		return "{}"
	}
}

// jsNumber mirrors JS Number(v). It reports false for NaN.
func jsNumber(v any) (Number, bool) {
	switch v := v.(type) {
	case Number:
		return v, true
	case bool:
		if v {
			return "1", true
		}
		return "0", true
	case nil:
		return "0", true
	case string:
		s := strings.TrimSpace(v)
		if s == "" {
			return "0", true
		}
		if len(s) > 2 && s[0] == '0' {
			base := 0
			switch s[1] {
			case 'x', 'X':
				base = 16
			case 'o', 'O':
				base = 8
			case 'b', 'B':
				base = 2
			}
			if base != 0 {
				n, err := strconv.ParseUint(s[2:], base, 64)
				if err != nil {
					return "", false
				}
				return FloatNumber(float64(n)), true
			}
		}
		if strings.ContainsAny(s, "xXpP_") || strings.EqualFold(s, "nan") || strings.Contains(strings.ToLower(s), "inf") {
			return "", false
		}
		f, err := strconv.ParseFloat(s, 64)
		if err != nil || math.IsInf(f, 0) {
			return "", false
		}
		return FloatNumber(f), true
	case []any:
		switch len(v) {
		case 0:
			return "0", true
		case 1:
			return jsNumber(JSString(v[0]))
		}
	}
	return "", false
}

// KeyString renders a key value as text, for matching keys.
func KeyString(v any) string { return keyString(v) }
