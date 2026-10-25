//! Engine values in DynamoDB's JSON wire form (`{"S": "x"}`, `{"N": "1"}`,
//! ...), as the v3 DocumentClient marshals JS values with default options.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Map, Value as Json, json};

use crate::error::{Error, Result};
use crate::value::{Attrs, Number, Value};

/// Renders a value in wire form. Undefined map entries and list items are
/// skipped.
pub fn to_json(v: &Value) -> Json {
    match v {
        Value::Undefined => Json::Null,
        Value::Null => json!({ "NULL": true }),
        Value::String(s) => json!({ "S": s }),
        Value::Number(n) => json!({ "N": n.as_str() }),
        Value::Bool(b) => json!({ "BOOL": b }),
        Value::Binary(b) => json!({ "B": STANDARD.encode(b) }),
        Value::List(items) => {
            json!({ "L": items.iter().filter(|e| !e.is_undefined()).map(to_json).collect::<Vec<_>>() })
        }
        Value::Map(m) => json!({ "M": attrs_to_json(m) }),
        Value::Set(items) => match items.first() {
            Some(Value::Number(_)) => {
                json!({ "NS": items.iter().map(|e| match e { Value::Number(n) => n.as_str().to_string(), e => crate::value::key_string(e) }).collect::<Vec<_>>() })
            }
            Some(Value::Binary(_)) => {
                json!({ "BS": items.iter().map(|e| match e { Value::Binary(b) => STANDARD.encode(b), _ => String::new() }).collect::<Vec<_>>() })
            }
            _ => json!({ "SS": items.iter().map(crate::value::key_string).collect::<Vec<_>>() }),
        },
    }
}

/// Renders a map of values in wire form, skipping undefined ones.
pub fn attrs_to_json(m: &Attrs) -> Json {
    Json::Object(
        m.iter()
            .filter(|(_, v)| !v.is_undefined())
            .map(|(k, v)| (k.clone(), to_json(v)))
            .collect(),
    )
}

/// Decodes a wire value, as the v3 DocumentClient unmarshals it (sets stay
/// sets).
pub fn from_json(j: &Json) -> Result<Value> {
    let Json::Object(o) = j else {
        return Err(Error::invalid(format!(
            "attribute value must be an object: {j}"
        )));
    };
    if o.len() != 1 {
        return Err(Error::invalid(format!(
            "attribute value needs exactly one type key: {j}"
        )));
    }
    let (typ, v) = o.iter().next().unwrap();
    let str_of = |v: &Json| {
        v.as_str()
            .map(String::from)
            .ok_or_else(|| Error::invalid(format!("{typ} needs a string: {v}")))
    };
    let list_of = |v: &Json| {
        v.as_array()
            .cloned()
            .ok_or_else(|| Error::invalid(format!("{typ} needs an array: {v}")))
    };
    let bytes = |v: &Json| -> Result<Vec<u8>> {
        let s = str_of(v)?;
        STANDARD
            .decode(s)
            .map_err(|e| Error::invalid(format!("invalid base64: {e}")))
    };
    Ok(match typ.as_str() {
        "S" => Value::String(str_of(v)?),
        "N" => Value::Number(Number::new(str_of(v)?)),
        "B" => Value::Binary(bytes(v)?),
        "BOOL" => Value::Bool(
            v.as_bool()
                .ok_or_else(|| Error::invalid("BOOL needs a boolean"))?,
        ),
        "NULL" => Value::Null,
        "SS" => Value::Set(
            list_of(v)?
                .iter()
                .map(|e| str_of(e).map(Value::String))
                .collect::<Result<_>>()?,
        ),
        "NS" => Value::Set(
            list_of(v)?
                .iter()
                .map(|e| str_of(e).map(|s| Value::Number(Number::new(s))))
                .collect::<Result<_>>()?,
        ),
        "BS" => Value::Set(
            list_of(v)?
                .iter()
                .map(|e| bytes(e).map(Value::Binary))
                .collect::<Result<_>>()?,
        ),
        "L" => Value::List(list_of(v)?.iter().map(from_json).collect::<Result<_>>()?),
        "M" => Value::Map(attrs_from_json(v)?),
        t => {
            return Err(Error::invalid(format!(
                "unknown attribute value type {t:?}"
            )));
        }
    })
}

/// Decodes a JSON object of wire values.
pub fn attrs_from_json(j: &Json) -> Result<Attrs> {
    let Json::Object(o) = j else {
        return Err(Error::invalid(format!(
            "expected an object of attribute values: {j}"
        )));
    };
    let mut out = Attrs::new();
    for (k, v) in o {
        out.insert(
            k.clone(),
            from_json(v).map_err(|e| Error::invalid(format!("{k}: {e}")))?,
        );
    }
    Ok(out)
}

/// Decodes plain JSON into engine values, as the JS values ElectroDB was
/// given: numbers keep their JSON text (JS's `String(n)` form), objects
/// become maps and arrays lists.
pub fn from_plain_json(j: &Json) -> Value {
    match j {
        Json::Null => Value::Null,
        Json::Bool(b) => Value::Bool(*b),
        Json::Number(n) => Value::Number(Number::new(n.to_string())),
        Json::String(s) => Value::String(s.clone()),
        Json::Array(a) => Value::List(a.iter().map(from_plain_json).collect()),
        Json::Object(o) => Value::Map(
            o.iter()
                .map(|(k, v)| (k.clone(), from_plain_json(v)))
                .collect(),
        ),
    }
}

/// Renders an engine value as plain JSON: sets become arrays and binary
/// base64 text. Undefined values are left out.
pub fn to_plain_json(v: &Value) -> Json {
    match v {
        Value::Undefined | Value::Null => Json::Null,
        Value::String(s) => Json::String(s.clone()),
        Value::Number(n) => serde_json::from_str::<serde_json::Number>(n.as_str())
            .map(Json::Number)
            .unwrap_or_else(|_| Json::String(n.as_str().to_string())),
        Value::Bool(b) => Json::Bool(*b),
        Value::Binary(b) => Json::String(STANDARD.encode(b)),
        Value::List(items) | Value::Set(items) => Json::Array(
            items
                .iter()
                .filter(|e| !e.is_undefined())
                .map(to_plain_json)
                .collect(),
        ),
        Value::Map(m) => Json::Object(
            m.iter()
                .filter(|(_, v)| !v.is_undefined())
                .map(|(k, v)| (k.clone(), to_plain_json(v)))
                .collect::<Map<_, _>>(),
        ),
    }
}

/// Reports why a value can't be marshalled, as the DocumentClient refuses
/// to: an empty set, or a set mixing item types.
pub fn check(v: &Value) -> Result<()> {
    match v {
        Value::Set(items) => {
            let Some(first) = items.first() else {
                return Err(Error::invalid("wattdb: sets can't be empty"));
            };
            if items
                .iter()
                .any(|e| std::mem::discriminant(e) != std::mem::discriminant(first))
            {
                return Err(Error::invalid("wattdb: a set can't mix item types"));
            }
            if !matches!(
                first,
                Value::String(_) | Value::Number(_) | Value::Binary(_)
            ) {
                return Err(Error::invalid(format!("wattdb: sets can't hold {first:?}")));
            }
            Ok(())
        }
        Value::List(items) => items.iter().try_for_each(check),
        Value::Map(m) => check_attrs(m),
        _ => Ok(()),
    }
}

/// `check` for every value of a map.
pub fn check_attrs(m: &Attrs) -> Result<()> {
    m.iter()
        .try_for_each(|(k, v)| check(v).map_err(|e| Error::invalid(format!("{k}: {e}"))))
}
