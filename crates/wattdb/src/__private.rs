//! Support for the code the derive macros generate. Not a public API.

use std::fmt::Display;
use std::sync::Arc;

pub use std::sync::OnceLock;

pub use futures;
pub use serde;
pub use wattdb_engine::model::{
    AttrType, AttributeDef, ConditionFn, DefaultFn, Definition, IndexDef, IndexType, KeyDef,
    Padding, Projection, TransformFn, ValidateFn,
};

use crate::codec::Codec;
use crate::error::{Error, Result};
use crate::value::{Attrs, Value};

/// Types the generated callbacks receive as their siblings: an entity's
/// `Partial`, or a map struct.
pub trait FromAttrs: Sized {
    fn from_attrs(attrs: &Attrs) -> Result<Self>;
}

/// A service name: a string, or a [`Service`](crate::Service).
pub trait ServiceName {
    fn service_name(&self) -> &str;
}

impl ServiceName for &str {
    fn service_name(&self) -> &str {
        self
    }
}

impl ServiceName for crate::Service {
    fn service_name(&self) -> &str {
        self.name()
    }
}

pub fn service_name(s: impl ServiceName) -> String {
    s.service_name().to_string()
}

fn defined(v: &Value) -> Option<&Value> {
    match v {
        Value::Undefined | Value::Null => None,
        v => Some(v),
    }
}

/// Decodes an optional attribute.
pub fn decode<T: Send + Sync + 'static, C: Codec<T>>(
    attrs: &Attrs,
    field: &str,
) -> Result<Option<T>> {
    match attrs.get(field).and_then(defined) {
        None => Ok(None),
        Some(v) => C::decode(v)
            .map(Some)
            .map_err(|e| Error::Decode(format!("{field}: {}", strip(&e)))),
    }
}

/// Decodes an attribute held as a plain value: missing is the type's
/// default, as a missing required attribute reads in ElectroDB.
pub fn decode_or_default<T: Default + Send + Sync + 'static, C: Codec<T>>(
    attrs: &Attrs,
    field: &str,
) -> Result<T> {
    Ok(decode::<T, C>(attrs, field)?.unwrap_or_default())
}

fn strip(e: &Error) -> String {
    e.to_string().trim_start_matches("wattdb: ").to_string()
}

/// Encodes an attribute into `out`; an encoded `Undefined` (an empty set)
/// is left out.
pub fn encode<T: Send + Sync + 'static, C: Codec<T>>(
    out: &mut Attrs,
    field: &str,
    v: &T,
) -> Result<()> {
    let e = C::encode(v).map_err(|e| Error::Encode(format!("{field}: {}", strip(&e))))?;
    if !e.is_undefined() {
        out.insert(field.to_string(), e);
    }
    Ok(())
}

/// Encodes an optional attribute.
pub fn encode_opt<T: Send + Sync + 'static, C: Codec<T>>(
    out: &mut Attrs,
    field: &str,
    v: &Option<T>,
) -> Result<()> {
    match v {
        Some(v) => encode::<T, C>(out, field, v),
        None => Ok(()),
    }
}

/// Encodes one value for a key condition or step.
pub fn encode_value<T: Send + Sync + 'static, C: Codec<T>>(v: &T) -> Result<Value> {
    C::encode(v)
}

fn callback_value<T: Send + Sync + 'static, C: Codec<T>>(
    v: &Value,
) -> std::result::Result<Option<T>, crate::BoxError> {
    match defined(v) {
        None => Ok(None),
        Some(v) => Ok(Some(
            C::decode(v).map_err(|e| Box::new(e) as crate::BoxError)?,
        )),
    }
}

fn callback_out<T: Send + Sync + 'static, C: Codec<T>>(
    v: Option<T>,
) -> std::result::Result<Value, crate::BoxError> {
    match v {
        None => Ok(Value::Undefined),
        Some(v) => C::encode(&v).map_err(|e| Box::new(e) as crate::BoxError),
    }
}

/// Wraps a default expression.
pub fn default_fn<T, C>(f: impl Fn() -> T + Send + Sync + 'static) -> DefaultFn
where
    T: Send + Sync + 'static,
    C: Codec<T>,
{
    Arc::new(move || callback_out::<T, C>(Some(f())))
}

/// Wraps a get or set callback: `fn(Option<T>, &Siblings) -> Option<T>`.
pub fn transform_fn<T, C, S>(
    f: impl Fn(Option<T>, &S) -> Option<T> + Send + Sync + 'static,
) -> TransformFn
where
    T: Send + Sync + 'static,
    C: Codec<T>,
    S: FromAttrs,
{
    Arc::new(move |v: Value, siblings: &Attrs| {
        let v = callback_value::<T, C>(&v)?;
        let s = S::from_attrs(siblings).map_err(|e| Box::new(e) as crate::BoxError)?;
        callback_out::<T, C>(f(v, &s))
    })
}

/// Wraps a validate callback: `fn(Option<&T>) -> Result<(), impl Display>`.
pub fn validate_fn<T, C, E>(
    f: impl Fn(Option<&T>) -> std::result::Result<(), E> + Send + Sync + 'static,
) -> ValidateFn
where
    T: Send + Sync + 'static,
    C: Codec<T>,
    E: Display,
{
    Arc::new(move |v: &Value| {
        let v = callback_value::<T, C>(v)?;
        match f(v.as_ref()) {
            Ok(()) => Ok(true),
            Err(e) => Err(e.to_string().into()),
        }
    })
}

/// Wraps a sparse-index condition: `fn(&Partial) -> bool`.
pub fn condition_fn<P: FromAttrs>(f: impl Fn(&P) -> bool + Send + Sync + 'static) -> ConditionFn {
    Arc::new(move |attrs: &Attrs| {
        let p = P::from_attrs(attrs).map_err(|e| Box::new(e) as crate::BoxError)?;
        Ok(f(&p))
    })
}

/// Compiles a `validate_regex` pattern.
pub fn regex(pattern: &str) -> std::result::Result<regex::Regex, String> {
    regex::Regex::new(pattern).map_err(|e| format!("invalid validate_regex {pattern:?}: {e}"))
}

/// Compiles a generated definition, caching it in `cell`.
pub fn entity_def(
    cell: &'static OnceLock<std::result::Result<crate::EntityDef, String>>,
    def: impl FnOnce() -> std::result::Result<Definition, String>,
) -> Result<&'static crate::EntityDef> {
    cell.get_or_init(|| def().and_then(crate::EntityDef::compile))
        .as_ref()
        .map_err(|e| Error::Schema(e.clone()))
}

/// The definition of a list attribute's items with extra callbacks.
pub fn with_items(mut def: AttributeDef, f: impl FnOnce(&mut AttributeDef)) -> AttributeDef {
    if let Some(items) = def.items.as_mut() {
        f(items);
    }
    def
}

pub use aws_sdk_dynamodb::operation::query::{QueryInput, QueryOutput};

pub use crate::collection::member;
pub use crate::query::facet;
