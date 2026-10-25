//! Engine values: the untyped attribute values the engine works on, and the
//! JS semantics (`String(v)`, `Number(v)`, `typeof`, ...) ElectroDB applies to
//! them.

use std::collections::BTreeMap;
use std::fmt;

/// Attribute values keyed by attribute name. A missing name is JS's
/// `undefined`.
pub type Attrs = BTreeMap<String, Value>;

/// An engine value, as the v3 DocumentClient sees JS values.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Value {
    /// JS `undefined`: a value that is missing. Callbacks receive and return
    /// it.
    #[default]
    Undefined,
    /// JS `null`.
    Null,
    String(String),
    Number(Number),
    Bool(bool),
    Binary(Vec<u8>),
    List(Vec<Value>),
    Map(Attrs),
    /// A DynamoDB set: string, number or binary items, deduplicated.
    Set(Vec<Value>),
}

/// A DynamoDB number kept in its string form, so integers of any size and
/// numbers recorded from JS keep their exact text (decision D57).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Number(String);

impl Number {
    /// Wraps a number's text as is.
    pub fn new(text: impl Into<String>) -> Self {
        Number(text.into())
    }

    /// Formats a float the way wattdb writes numbers (decision D56): the
    /// shortest round-tripping decimal, never an exponent, and `-0` as `0`
    /// (D58). Identical to JS for integers and ordinary decimals.
    pub fn from_f64(f: f64) -> Self {
        if f == 0.0 {
            return Number("0".into());
        }
        Number(format!("{f}"))
    }

    pub fn from_i64(i: i64) -> Self {
        Number(i.to_string())
    }

    pub fn from_u64(i: u64) -> Self {
        Number(i.to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }

    /// The number as a float, if its text parses.
    pub fn as_f64(&self) -> Option<f64> {
        self.0.parse().ok()
    }
}

impl fmt::Display for Number {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Value::String(s.to_string())
    }
}

impl From<String> for Value {
    fn from(s: String) -> Self {
        Value::String(s)
    }
}

impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Value::Bool(b)
    }
}

impl From<Number> for Value {
    fn from(n: Number) -> Self {
        Value::Number(n)
    }
}

impl From<i64> for Value {
    fn from(i: i64) -> Self {
        Value::Number(Number::from_i64(i))
    }
}

impl From<Attrs> for Value {
    fn from(m: Attrs) -> Self {
        Value::Map(m)
    }
}

impl Value {
    pub fn is_undefined(&self) -> bool {
        matches!(self, Value::Undefined)
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_map(&self) -> Option<&Attrs> {
        match self {
            Value::Map(m) => Some(m),
            _ => None,
        }
    }
}

/// Returns `attrs[name]`, or `Undefined` when it's missing.
pub(crate) fn lookup(attrs: &Attrs, name: &str) -> Value {
    attrs.get(name).cloned().unwrap_or(Value::Undefined)
}

/// JS template-literal stringification of a key composite.
pub fn key_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.0.clone(),
        Value::Bool(true) => "true".into(),
        Value::Bool(false) => "false".into(),
        Value::Null => "null".into(),
        _ => "[object Object]".into(),
    }
}

/// Mirrors `JSON.stringify` for engine values, with object keys sorted.
pub(crate) fn json_stringify(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::String(s) => serde_json::to_string(s).unwrap_or_default(),
        Value::Number(n) => n.0.clone(),
        Value::Bool(b) => b.to_string(),
        Value::List(items) => {
            let parts: Vec<String> = items
                .iter()
                .map(|e| {
                    if e.is_undefined() {
                        "null".into()
                    } else {
                        json_stringify(e)
                    }
                })
                .collect();
            format!("[{}]", parts.join(","))
        }
        Value::Map(m) => {
            let parts: Vec<String> = m
                .iter()
                .filter(|(_, e)| !e.is_undefined())
                .map(|(k, e)| {
                    format!(
                        "{}:{}",
                        serde_json::to_string(k).unwrap_or_default(),
                        json_stringify(e)
                    )
                })
                .collect();
            format!("{{{}}}", parts.join(","))
        }
        _ => "{}".into(),
    }
}

/// Mirrors JS `Number(v)`; `None` is NaN.
pub(crate) fn js_number(v: &Value) -> Option<Number> {
    match v {
        Value::Number(n) => Some(n.clone()),
        Value::Bool(true) => Some(Number::new("1")),
        Value::Bool(false) | Value::Null => Some(Number::new("0")),
        Value::String(s) => js_number_str(s),
        Value::List(items) => match items.len() {
            0 => Some(Number::new("0")),
            1 => js_number_str(&js_string(&items[0])),
            _ => None,
        },
        _ => None,
    }
}

fn js_number_str(s: &str) -> Option<Number> {
    let s = s.trim();
    if s.is_empty() {
        return Some(Number::new("0"));
    }
    let b = s.as_bytes();
    if b.len() > 2 && b[0] == b'0' {
        let base = match b[1] {
            b'x' | b'X' => 16,
            b'o' | b'O' => 8,
            b'b' | b'B' => 2,
            _ => 0,
        };
        if base != 0 {
            let digits = &s[2..];
            if digits.starts_with(['+', '-']) {
                return None;
            }
            return u64::from_str_radix(digits, base)
                .ok()
                .map(|n| Number::from_f64(n as f64));
        }
    }
    let lower = s.to_ascii_lowercase();
    if s.contains(['x', 'X', 'p', 'P', '_']) || lower == "nan" || lower.contains("inf") {
        return None;
    }
    match s.parse::<f64>() {
        Ok(f) if f.is_finite() => Some(Number::from_f64(f)),
        _ => None,
    }
}

/// Mirrors JS `isNaN(string)`.
pub(crate) fn is_nan(s: &str) -> bool {
    js_number_str(s).is_none()
}

/// Mirrors JS `typeof` for engine values.
pub(crate) fn js_typeof(v: &Value) -> &'static str {
    match v {
        Value::String(_) => "string",
        Value::Number(_) => "number",
        Value::Bool(_) => "boolean",
        Value::Undefined => "undefined",
        _ => "object",
    }
}

/// Mirrors ElectroDB's `getValueType`.
pub(crate) fn value_type(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::String(_) => "string",
        Value::Number(_) => "number",
        Value::Bool(_) => "boolean",
        Value::List(_) => "array",
        Value::Set(_) => "set",
        Value::Map(_) => "object",
        Value::Undefined => "undefined",
        Value::Binary(_) => "unknown",
    }
}

/// Mirrors JS `String(v)` for engine values.
pub fn js_string(v: &Value) -> String {
    match v {
        Value::List(items) => items
            .iter()
            .map(|e| match e {
                Value::Null | Value::Undefined => String::new(),
                e => js_string(e),
            })
            .collect::<Vec<_>>()
            .join(","),
        Value::Set(_) => "[object Set]".into(),
        Value::Undefined => "undefined".into(),
        v => key_string(v),
    }
}

/// A string's length in UTF-16 code units, as in JS.
pub fn js_length(s: &str) -> usize {
    s.encode_utf16().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(s: &str) -> Value {
        Value::Number(Number::new(s))
    }

    #[test]
    fn js_number_matches_js() {
        let cases: Vec<(Value, Option<&str>)> = vec![
            ("12".into(), Some("12")),
            (" 1.5 ".into(), Some("1.5")),
            ("".into(), Some("0")),
            ("0x10".into(), Some("16")),
            ("0b11".into(), Some("3")),
            ("1e3".into(), Some("1000")),
            ("12px".into(), None),
            ("Infinity".into(), None),
            (Value::Bool(true), Some("1")),
            (Value::Null, Some("0")),
            (Value::List(vec![]), Some("0")),
            (Value::List(vec!["7".into()]), Some("7")),
            (Value::List(vec![n("1"), n("2")]), None),
            (Value::Map(Attrs::new()), None),
        ];
        for (input, want) in cases {
            let got = js_number(&input);
            assert_eq!(
                got.as_ref().map(Number::as_str),
                want,
                "js_number({input:?})"
            );
        }
    }

    #[test]
    fn json_stringify_sorts_and_skips_undefined() {
        let mut m = Attrs::new();
        m.insert(
            "b".into(),
            Value::List(vec![n("1"), Value::Null, Value::Bool(true)]),
        );
        m.insert("a".into(), "x\"y".into());
        m.insert("u".into(), Value::Undefined);
        assert_eq!(
            json_stringify(&Value::Map(m)),
            r#"{"a":"x\"y","b":[1,null,true]}"#
        );
    }

    #[test]
    fn from_f64_never_uses_exponents() {
        let cases = [
            (42.0, "42"),
            (-17.25, "-17.25"),
            (0.1, "0.1"),
            (0.30000000000000004, "0.30000000000000004"),
            (-0.0, "0"),
            (9007199254740991.0, "9007199254740991"),
            (1e20, "100000000000000000000"),
            (1e-7, "0.0000001"),
            (1e21, "1000000000000000000000"),
        ];
        for (f, want) in cases {
            assert_eq!(Number::from_f64(f).as_str(), want, "from_f64({f})");
        }
    }
}
