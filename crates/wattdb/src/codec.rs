//! How Rust values are stored (decision D18).
//!
//! Every attribute type implements [`FieldType`], its default storage. A
//! field can pick another storage with `#[wattdb(codec = SomeCodec)]`,
//! where the codec implements [`Codec`] for the field's type:
//!
//! | Rust type | Default (`FieldType`) | Alternatives |
//! |---|---|---|
//! | `String` | string (S) | |
//! | integers, `f32`, `f64` | number (N), exact text | |
//! | `bool` | boolean | |
//! | `Vec<T>` | list (L) | [`VecSet`] (also `#[wattdb(as_set)]`): a set, keeping order |
//! | `BTreeSet<T>`, `HashSet<T>` | set (SS/NS), empty means missing | |
//! | `chrono::DateTime<Utc>` | UTC RFC 3339, fixed 9-digit fraction (S) | [`JsIso`]: `Date.toISOString()` |
//! | `std::time::Duration` | nanoseconds (N) | |
//! | `uuid::Uuid` | 16 bytes (B) | [`UuidString`]: canonical text (S) |
//! | `Vec<u8>` with [`Binary`] | | bytes (B) |
//! | `serde_json::Value`, [`Value`] | any | |
//! | any `Serialize + Deserialize` | | [`Serde`]: any, through serde |
//!
//! Derive [`Enum`](crate::Enum) for string enums and [`Map`](crate::Map)
//! for maps; implement `FieldType` (or a `Codec`) for anything else.

use std::collections::{BTreeSet, HashSet};
use std::hash::Hash;

use wattdb_engine::model::{AttrType, AttributeDef};

use crate::attr::{Attr, Descriptor, ListAttr, NumberAttr, SetAttr};
use crate::error::{Error, Result};
use crate::value::{Number, Value};

/// A Rust type's default storage as an attribute.
pub trait FieldType: Sized + Send + Sync + 'static {
    /// The descriptor generated for attributes of this type, for conditions
    /// and updates.
    type Attr<E: 'static>: Descriptor<E, Value = Self>;

    /// The attribute's ElectroDB type: `ty`, plus `enum_values`, `items` or
    /// `properties` where they apply.
    fn type_def() -> AttributeDef;

    /// Encodes the value; `Value::Undefined` means "missing" (an empty set).
    fn encode(&self) -> Result<Value>;

    /// Decodes a stored (defined, non-null) value.
    fn decode(v: &Value) -> Result<Self>;
}

/// An alternative storage for `T`, picked with `#[wattdb(codec = ...)]`.
pub trait Codec<T: Send + Sync + 'static>: 'static {
    /// The descriptor generated for attributes using this codec.
    type Attr<E: 'static>: Descriptor<E, Value = T>;

    fn type_def() -> AttributeDef;
    fn encode(v: &T) -> Result<Value>;
    fn decode(v: &Value) -> Result<T>;
}

/// The default codec: the type's own [`FieldType`].
pub struct DefaultCodec;

impl<T: FieldType> Codec<T> for DefaultCodec {
    type Attr<E: 'static> = T::Attr<E>;

    fn type_def() -> AttributeDef {
        T::type_def()
    }

    fn encode(v: &T) -> Result<Value> {
        v.encode()
    }

    fn decode(v: &Value) -> Result<T> {
        T::decode(v)
    }
}

/// An attribute definition of the given type.
pub fn type_def(ty: AttrType) -> AttributeDef {
    AttributeDef {
        ty,
        ..Default::default()
    }
}

fn mismatch(want: &str, v: &Value) -> Error {
    Error::decode(format!("expected a {want}, got {}", short(v)))
}

fn short(v: &Value) -> String {
    let s = format!("{v:?}");
    if s.len() > 60 {
        format!("{}...", &s[..60])
    } else {
        s
    }
}

impl FieldType for String {
    type Attr<E: 'static> = Attr<E, String>;

    fn type_def() -> AttributeDef {
        type_def(AttrType::String)
    }

    fn encode(&self) -> Result<Value> {
        Ok(Value::String(self.clone()))
    }

    fn decode(v: &Value) -> Result<Self> {
        match v {
            Value::String(s) => Ok(s.clone()),
            v => Err(mismatch("string", v)),
        }
    }
}

impl FieldType for bool {
    type Attr<E: 'static> = Attr<E, bool>;

    fn type_def() -> AttributeDef {
        type_def(AttrType::Boolean)
    }

    fn encode(&self) -> Result<Value> {
        Ok(Value::Bool(*self))
    }

    fn decode(v: &Value) -> Result<Self> {
        match v {
            Value::Bool(b) => Ok(*b),
            v => Err(mismatch("boolean", v)),
        }
    }
}

fn number_text(v: &Value) -> Result<&str> {
    match v {
        Value::Number(n) => Ok(n.as_str()),
        v => Err(mismatch("number", v)),
    }
}

fn decode_int<T: TryFrom<i128>>(v: &Value) -> Result<T> {
    let text = number_text(v)?;
    let i = match text.parse::<i128>() {
        Ok(i) => i,
        Err(_) => {
            let f: f64 = text
                .parse()
                .map_err(|_| Error::decode(format!("{text:?} isn't a number")))?;
            if f.fract() != 0.0 || !f.is_finite() {
                return Err(Error::decode(format!("{text:?} isn't an integer")));
            }
            f as i128
        }
    };
    T::try_from(i).map_err(|_| Error::decode(format!("{text} is out of range")))
}

macro_rules! int_field {
    ($($t:ty),*) => {$(
        impl FieldType for $t {
            type Attr<E: 'static> = NumberAttr<E, $t>;

            fn type_def() -> AttributeDef {
                type_def(AttrType::Number)
            }

            fn encode(&self) -> Result<Value> {
                Ok(Value::Number(Number::new(self.to_string())))
            }

            fn decode(v: &Value) -> Result<Self> {
                decode_int(v)
            }
        }
    )*};
}

int_field!(
    i8, i16, i32, i64, i128, isize, u8, u16, u32, u64, u128, usize
);

/// Formats a float the way wattdb writes numbers (decision D56): the
/// shortest round-tripping decimal, never an exponent, and `-0` as `0`.
pub fn format_float(f: f64) -> Result<Number> {
    if !f.is_finite() {
        return Err(Error::encode(format!("{f} can't be stored in DynamoDB")));
    }
    Ok(Number::from_f64(f))
}

impl FieldType for f64 {
    type Attr<E: 'static> = NumberAttr<E, f64>;

    fn type_def() -> AttributeDef {
        type_def(AttrType::Number)
    }

    fn encode(&self) -> Result<Value> {
        Ok(Value::Number(format_float(*self)?))
    }

    fn decode(v: &Value) -> Result<Self> {
        let text = number_text(v)?;
        text.parse()
            .map_err(|_| Error::decode(format!("{text:?} isn't a number")))
    }
}

impl FieldType for f32 {
    type Attr<E: 'static> = NumberAttr<E, f32>;

    fn type_def() -> AttributeDef {
        type_def(AttrType::Number)
    }

    fn encode(&self) -> Result<Value> {
        if !self.is_finite() {
            return Err(Error::encode(format!("{self} can't be stored in DynamoDB")));
        }
        Ok(Value::Number(Number::new(if *self == 0.0 {
            "0".to_string()
        } else {
            self.to_string()
        })))
    }

    fn decode(v: &Value) -> Result<Self> {
        let text = number_text(v)?;
        text.parse()
            .map_err(|_| Error::decode(format!("{text:?} isn't a number")))
    }
}

impl<T: FieldType> FieldType for Vec<T> {
    type Attr<E: 'static> = ListAttr<E, T>;

    fn type_def() -> AttributeDef {
        AttributeDef {
            ty: AttrType::List,
            items: Some(Box::new(T::type_def())),
            ..Default::default()
        }
    }

    fn encode(&self) -> Result<Value> {
        let mut out = Vec::with_capacity(self.len());
        for (i, v) in self.iter().enumerate() {
            let e = v
                .encode()
                .map_err(|e| Error::encode(format!("[{i}]: {e}")))?;
            if !e.is_undefined() {
                out.push(e);
            }
        }
        Ok(Value::List(out))
    }

    fn decode(v: &Value) -> Result<Self> {
        match v {
            Value::List(items) => items
                .iter()
                .enumerate()
                .map(|(i, e)| T::decode(e).map_err(|err| Error::decode(format!("[{i}]: {err}"))))
                .collect(),
            v => Err(mismatch("list", v)),
        }
    }
}

fn set_items(v: &Value) -> Result<&[Value]> {
    match v {
        // Sets come back from getters as lists, as in ElectroDB.
        Value::Set(items) | Value::List(items) => Ok(items),
        v => Err(mismatch("set", v)),
    }
}

fn set_def<T: FieldType>() -> AttributeDef {
    AttributeDef {
        ty: AttrType::Set,
        items: Some(Box::new(T::type_def())),
        ..Default::default()
    }
}

fn encode_set<'a, T: FieldType + 'a>(items: impl Iterator<Item = &'a T>) -> Result<Value> {
    let out: Vec<Value> = items.map(FieldType::encode).collect::<Result<_>>()?;
    // An empty set is "absent" (decision D18).
    Ok(if out.is_empty() {
        Value::Undefined
    } else {
        Value::Set(out)
    })
}

impl<T: FieldType + Ord> FieldType for BTreeSet<T> {
    type Attr<E: 'static> = SetAttr<E, T, BTreeSet<T>>;

    fn type_def() -> AttributeDef {
        set_def::<T>()
    }

    fn encode(&self) -> Result<Value> {
        encode_set(self.iter())
    }

    fn decode(v: &Value) -> Result<Self> {
        set_items(v)?.iter().map(T::decode).collect()
    }
}

impl<T: FieldType + Eq + Hash> FieldType for HashSet<T> {
    type Attr<E: 'static> = SetAttr<E, T, HashSet<T>>;

    fn type_def() -> AttributeDef {
        set_def::<T>()
    }

    fn encode(&self) -> Result<Value> {
        encode_set(self.iter())
    }

    fn decode(v: &Value) -> Result<Self> {
        set_items(v)?.iter().map(T::decode).collect()
    }
}

/// Stores a `Vec<T>` as a DynamoDB set, keeping the order given and
/// dropping duplicates on write; an empty vec is "missing". It's what
/// `#[wattdb(as_set)]` picks.
pub struct VecSet;

impl<T: FieldType> Codec<Vec<T>> for VecSet {
    type Attr<E: 'static> = SetAttr<E, T, Vec<T>>;

    fn type_def() -> AttributeDef {
        set_def::<T>()
    }

    fn encode(v: &Vec<T>) -> Result<Value> {
        encode_set(v.iter())
    }

    fn decode(v: &Value) -> Result<Vec<T>> {
        set_items(v)?.iter().map(T::decode).collect()
    }
}

/// Stores bytes as binary (B). ElectroDB can't read binary through its
/// types, so the attribute is `any`.
pub struct Binary;

impl Codec<Vec<u8>> for Binary {
    type Attr<E: 'static> = Attr<E, Vec<u8>>;

    fn type_def() -> AttributeDef {
        type_def(AttrType::Any)
    }

    fn encode(v: &Vec<u8>) -> Result<Value> {
        Ok(Value::Binary(v.clone()))
    }

    fn decode(v: &Value) -> Result<Vec<u8>> {
        match v {
            Value::Binary(b) => Ok(b.clone()),
            v => Err(mismatch("binary value", v)),
        }
    }
}

impl FieldType for std::time::Duration {
    type Attr<E: 'static> = Attr<E, std::time::Duration>;

    fn type_def() -> AttributeDef {
        type_def(AttrType::Number)
    }

    /// Nanoseconds, as a number.
    fn encode(&self) -> Result<Value> {
        Ok(Value::Number(Number::new(self.as_nanos().to_string())))
    }

    fn decode(v: &Value) -> Result<Self> {
        let n: u64 = decode_int(v)?;
        Ok(std::time::Duration::from_nanos(n))
    }
}

/// Any value, as engine values: lossless, including sets and binary.
impl FieldType for Value {
    type Attr<E: 'static> = Attr<E, Value>;

    fn type_def() -> AttributeDef {
        type_def(AttrType::Any)
    }

    fn encode(&self) -> Result<Value> {
        Ok(self.clone())
    }

    fn decode(v: &Value) -> Result<Self> {
        Ok(v.clone())
    }
}

/// Any value, as JSON. Numbers keep their stored text where JSON can hold
/// it; sets read back as arrays and binary as base64 text.
#[cfg(feature = "json")]
impl FieldType for serde_json::Value {
    type Attr<E: 'static> = Attr<E, serde_json::Value>;

    fn type_def() -> AttributeDef {
        type_def(AttrType::Any)
    }

    fn encode(&self) -> Result<Value> {
        Ok(wattdb_engine::wire::from_plain_json(self))
    }

    fn decode(v: &Value) -> Result<Self> {
        Ok(wattdb_engine::wire::to_plain_json(v))
    }
}

/// Stores any `Serialize + Deserialize` type as an `any` attribute, through
/// its JSON form (Go wattdb's `AnyOf`).
#[cfg(feature = "json")]
pub struct Serde;

#[cfg(feature = "json")]
impl<T> Codec<T> for Serde
where
    T: serde::Serialize + serde::de::DeserializeOwned + Send + Sync + 'static,
{
    type Attr<E: 'static> = Attr<E, T>;

    fn type_def() -> AttributeDef {
        type_def(AttrType::Any)
    }

    fn encode(v: &T) -> Result<Value> {
        let j = serde_json::to_value(v).map_err(Error::encode)?;
        Ok(wattdb_engine::wire::from_plain_json(&j))
    }

    fn decode(v: &Value) -> Result<T> {
        serde_json::from_value(wattdb_engine::wire::to_plain_json(v)).map_err(Error::decode)
    }
}

#[cfg(feature = "chrono")]
mod time {
    use chrono::{DateTime, SecondsFormat, Utc};

    use super::*;

    /// UTC RFC 3339 with a fixed 9-digit fraction, so stored times sort
    /// lexically. This is the default for `DateTime<Utc>`.
    impl FieldType for DateTime<Utc> {
        type Attr<E: 'static> = Attr<E, DateTime<Utc>>;

        fn type_def() -> AttributeDef {
            type_def(AttrType::String)
        }

        fn encode(&self) -> Result<Value> {
            Ok(Value::String(
                self.to_rfc3339_opts(SecondsFormat::Nanos, true),
            ))
        }

        fn decode(v: &Value) -> Result<Self> {
            parse(v)
        }
    }

    fn parse(v: &Value) -> Result<DateTime<Utc>> {
        match v {
            Value::String(s) => DateTime::parse_from_rfc3339(s)
                .map(|t| t.with_timezone(&Utc))
                .map_err(|e| Error::decode(format!("invalid time {s:?}: {e}"))),
            v => Err(mismatch("time string", v)),
        }
    }

    /// Stores times the way JS `Date.toISOString` does (UTC, milliseconds),
    /// for fields shared with ElectroDB services.
    pub struct JsIso;

    impl Codec<DateTime<Utc>> for JsIso {
        type Attr<E: 'static> = Attr<E, DateTime<Utc>>;

        fn type_def() -> AttributeDef {
            type_def(AttrType::String)
        }

        fn encode(v: &DateTime<Utc>) -> Result<Value> {
            Ok(Value::String(
                v.to_rfc3339_opts(SecondsFormat::Millis, true),
            ))
        }

        fn decode(v: &Value) -> Result<DateTime<Utc>> {
            parse(v)
        }
    }
}

#[cfg(feature = "chrono")]
pub use time::JsIso;

#[cfg(feature = "uuid")]
mod uuids {
    use uuid::Uuid;

    use super::*;

    /// 16 bytes (B). This is the default for `Uuid`; JS can't read binary
    /// through ElectroDB's types, so shared fields should use
    /// [`UuidString`].
    impl FieldType for Uuid {
        type Attr<E: 'static> = Attr<E, Uuid>;

        fn type_def() -> AttributeDef {
            type_def(AttrType::Any)
        }

        fn encode(&self) -> Result<Value> {
            Ok(Value::Binary(self.as_bytes().to_vec()))
        }

        fn decode(v: &Value) -> Result<Self> {
            match v {
                Value::Binary(b) => {
                    Uuid::from_slice(b).map_err(|e| Error::decode(format!("invalid uuid: {e}")))
                }
                v => Err(mismatch("16-byte binary value", v)),
            }
        }
    }

    /// Stores a UUID in its canonical lowercase text form.
    pub struct UuidString;

    impl Codec<Uuid> for UuidString {
        type Attr<E: 'static> = Attr<E, Uuid>;

        fn type_def() -> AttributeDef {
            type_def(AttrType::String)
        }

        fn encode(v: &Uuid) -> Result<Value> {
            Ok(Value::String(v.hyphenated().to_string()))
        }

        fn decode(v: &Value) -> Result<Uuid> {
            match v {
                Value::String(s) => Uuid::parse_str(s)
                    .map_err(|e| Error::decode(format!("invalid uuid {s:?}: {e}"))),
                v => Err(mismatch("uuid string", v)),
            }
        }
    }
}

#[cfg(feature = "uuid")]
pub use uuids::UuidString;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_round_trip() {
        assert_eq!(42i64.encode().unwrap(), Value::Number(Number::new("42")));
        assert_eq!(i64::decode(&Value::Number(Number::new("3.0"))).unwrap(), 3);
        assert!(i64::decode(&Value::Number(Number::new("3.5"))).is_err());
        assert!(u8::decode(&Value::Number(Number::new("300"))).is_err());
        assert_eq!(
            1e21f64.encode().unwrap(),
            Value::Number(Number::new("1000000000000000000000"))
        );
        assert_eq!((-0.0f64).encode().unwrap(), Value::Number(Number::new("0")));
        assert!(f64::NAN.encode().is_err());
    }

    #[test]
    fn empty_sets_are_missing() {
        assert_eq!(
            BTreeSet::<String>::new().encode().unwrap(),
            Value::Undefined
        );
        assert_eq!(
            <VecSet as Codec<Vec<i64>>>::encode(&vec![]).unwrap(),
            Value::Undefined
        );
        let s: BTreeSet<String> = ["b".to_string(), "a".to_string()].into();
        assert_eq!(
            s.encode().unwrap(),
            Value::Set(vec!["a".into(), "b".into()])
        );
        let back = BTreeSet::<String>::decode(&Value::List(vec!["x".into()])).unwrap();
        assert!(back.contains("x"));
    }

    #[cfg(feature = "chrono")]
    #[test]
    fn times() {
        use chrono::TimeZone;
        let t = chrono::Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap();
        assert_eq!(
            t.encode().unwrap(),
            Value::String("2026-01-02T03:04:05.000000000Z".into())
        );
        assert_eq!(
            JsIso::encode(&t).unwrap(),
            Value::String("2026-01-02T03:04:05.000Z".into())
        );
        assert_eq!(
            chrono::DateTime::<chrono::Utc>::decode(&Value::String(
                "2026-01-02T03:04:05.000Z".into()
            ))
            .unwrap(),
            t
        );
    }

    #[cfg(feature = "uuid")]
    #[test]
    fn uuids() {
        let u = uuid::Uuid::from_u128(0x0123_4567_89ab_cdef_0123_4567_89ab_cdef);
        assert_eq!(
            UuidString::encode(&u).unwrap(),
            Value::String("01234567-89ab-cdef-0123-456789abcdef".into())
        );
        assert_eq!(uuid::Uuid::decode(&u.encode().unwrap()).unwrap(), u);
    }
}
