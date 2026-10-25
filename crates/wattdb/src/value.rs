//! Conversions between engine values and the SDK's `AttributeValue`.

use std::collections::HashMap;

use aws_sdk_dynamodb::primitives::Blob;
use aws_sdk_dynamodb::types::AttributeValue;
pub use wattdb_engine::{Attrs, Number, Value};

use crate::error::{Error, Result};

/// A DynamoDB item (or key) in SDK form.
pub type Item = HashMap<String, AttributeValue>;

/// Converts an engine value to its SDK form, as the v3 DocumentClient
/// marshals JS values with default options. `Undefined` is `None`; an empty
/// set is an error, as in JS.
pub fn to_attribute_value(v: &Value) -> Result<Option<AttributeValue>> {
    Ok(Some(match v {
        Value::Undefined => return Ok(None),
        Value::Null => AttributeValue::Null(true),
        Value::String(s) => AttributeValue::S(s.clone()),
        Value::Number(n) => AttributeValue::N(n.as_str().to_string()),
        Value::Bool(b) => AttributeValue::Bool(*b),
        Value::Binary(b) => AttributeValue::B(Blob::new(b.clone())),
        Value::List(items) => {
            let mut out = Vec::with_capacity(items.len());
            for e in items {
                if let Some(av) = to_attribute_value(e)? {
                    out.push(av);
                }
            }
            AttributeValue::L(out)
        }
        Value::Map(m) => AttributeValue::M(to_item(m)?),
        Value::Set(items) => {
            wattdb_engine::wire::check(v)
                .map_err(|e| Error::encode(e.to_string().trim_start_matches("wattdb: ")))?;
            match &items[0] {
                Value::String(_) => AttributeValue::Ss(
                    items
                        .iter()
                        .map(|e| e.as_str().unwrap_or_default().to_string())
                        .collect(),
                ),
                Value::Number(_) => AttributeValue::Ns(
                    items
                        .iter()
                        .map(|e| match e {
                            Value::Number(n) => n.as_str().to_string(),
                            _ => String::new(),
                        })
                        .collect(),
                ),
                _ => AttributeValue::Bs(
                    items
                        .iter()
                        .map(|e| match e {
                            Value::Binary(b) => Blob::new(b.clone()),
                            _ => Blob::new(Vec::new()),
                        })
                        .collect(),
                ),
            }
        }
    }))
}

/// Converts a map of engine values, skipping `Undefined` ones.
pub fn to_item(m: &Attrs) -> Result<Item> {
    let mut out = HashMap::with_capacity(m.len());
    for (k, v) in m {
        match to_attribute_value(v) {
            Ok(Some(av)) => {
                out.insert(k.clone(), av);
            }
            Ok(None) => {}
            Err(e) => {
                return Err(Error::encode(format!(
                    "{k}: {}",
                    e.to_string().trim_start_matches("wattdb: ")
                )));
            }
        }
    }
    Ok(out)
}

/// Converts an SDK value to an engine value, as the v3 DocumentClient
/// unmarshals it (sets stay sets).
pub fn from_attribute_value(av: &AttributeValue) -> Value {
    match av {
        AttributeValue::S(s) => Value::String(s.clone()),
        AttributeValue::N(n) => Value::Number(Number::new(n.clone())),
        AttributeValue::Bool(b) => Value::Bool(*b),
        AttributeValue::Null(_) => Value::Null,
        AttributeValue::B(b) => Value::Binary(b.as_ref().to_vec()),
        AttributeValue::L(items) => Value::List(items.iter().map(from_attribute_value).collect()),
        AttributeValue::M(m) => Value::Map(from_item(m)),
        AttributeValue::Ss(items) => {
            Value::Set(items.iter().map(|s| Value::String(s.clone())).collect())
        }
        AttributeValue::Ns(items) => Value::Set(
            items
                .iter()
                .map(|s| Value::Number(Number::new(s.clone())))
                .collect(),
        ),
        AttributeValue::Bs(items) => Value::Set(
            items
                .iter()
                .map(|b| Value::Binary(b.as_ref().to_vec()))
                .collect(),
        ),
        _ => Value::Undefined,
    }
}

/// Converts an SDK item to engine values.
pub fn from_item(m: &Item) -> Attrs {
    m.iter()
        .map(|(k, v)| (k.clone(), from_attribute_value(v)))
        .collect()
}
