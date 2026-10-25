use std::fmt;
use std::sync::Arc;

use regex::Regex;

use crate::error::BoxError;
use crate::value::{Attrs, Value};

/// A default callback.
pub type DefaultFn = Arc<dyn Fn() -> Result<Value, BoxError> + Send + Sync>;
/// A get or set callback: the attribute's value (`Undefined` when missing)
/// and its siblings.
pub type TransformFn = Arc<dyn Fn(Value, &Attrs) -> Result<Value, BoxError> + Send + Sync>;
/// A validate callback; `Ok(false)` and `Err` both reject the value.
pub type ValidateFn = Arc<dyn Fn(&Value) -> Result<bool, BoxError> + Send + Sync>;
/// A sparse-index condition.
pub type ConditionFn = Arc<dyn Fn(&Attrs) -> Result<bool, BoxError> + Send + Sync>;

/// An entity schema as written: the equivalent of an ElectroDB v1 schema
/// object. Attributes and indexes are ordered, as ElectroDB follows object
/// key order.
#[derive(Clone, Default)]
pub struct Definition {
    pub entity: String,
    pub version: String,
    pub service: String,
    pub attributes: Vec<AttributeDef>,
    pub indexes: Vec<IndexDef>,
    /// Overrides the identifier field names; empty fields keep the
    /// defaults, `__edb_e__` and `__edb_v__`.
    pub identifiers: Identifiers,
}

/// An attribute's type.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum AttrType {
    #[default]
    String,
    Number,
    Boolean,
    Enum,
    Map,
    List,
    Set,
    Any,
}

impl AttrType {
    pub fn as_str(self) -> &'static str {
        match self {
            AttrType::String => "string",
            AttrType::Number => "number",
            AttrType::Boolean => "boolean",
            AttrType::Enum => "enum",
            AttrType::Map => "map",
            AttrType::List => "list",
            AttrType::Set => "set",
            AttrType::Any => "any",
        }
    }

    /// Parses an ElectroDB type name.
    pub fn parse(s: &str) -> Option<AttrType> {
        Some(match s {
            "" | "string" => AttrType::String,
            "number" => AttrType::Number,
            "boolean" => AttrType::Boolean,
            "enum" => AttrType::Enum,
            "map" => AttrType::Map,
            "list" => AttrType::List,
            "set" => AttrType::Set,
            "any" => AttrType::Any,
            _ => return None,
        })
    }
}

impl fmt::Display for AttrType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One attribute as written.
#[derive(Clone, Default)]
pub struct AttributeDef {
    pub name: String,
    /// The type; for enums, `Enum` with the allowed values in `enum_values`.
    pub ty: AttrType,
    pub enum_values: Vec<String>,
    pub field: String,
    pub label: Option<String>,
    pub required: bool,
    pub hidden: bool,
    pub read_only: bool,
    pub padding: Option<Padding>,
    /// `"string"` or `"number"` to coerce written values; empty means none.
    pub cast: String,

    pub default: Option<DefaultFn>,
    pub get: Option<TransformFn>,
    pub set: Option<TransformFn>,
    pub validate: Option<ValidateFn>,
    pub validate_regex: Option<Regex>,

    /// The sibling attributes this attribute's get/set watch.
    pub watch: Vec<String>,
    /// Watches every attribute (`"*"`).
    pub watch_all: bool,

    /// List and set items.
    pub items: Option<Box<AttributeDef>>,
    /// A map's properties, in order. `None` for a map is an error.
    pub properties: Option<Vec<AttributeDef>>,
}

/// An index's key layout.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum IndexType {
    #[default]
    Isolated,
    Clustered,
    Composite,
}

impl IndexType {
    pub fn parse(s: &str) -> Option<IndexType> {
        Some(match s {
            "" | "isolated" => IndexType::Isolated,
            "clustered" => IndexType::Clustered,
            "composite" => IndexType::Composite,
            _ => return None,
        })
    }
}

/// One access pattern as written.
#[derive(Clone, Default)]
pub struct IndexDef {
    pub access_pattern: String,
    /// The DynamoDB index name; empty for the table's own index.
    pub index: String,
    pub ty: IndexType,
    /// The collections the index belongs to. `collection_array` records
    /// whether they were written as an array (sub-collections).
    pub collection: Vec<String>,
    pub collection_array: bool,
    pub scope: String,
    /// Decides whether a write keeps this index's keys (a sparse index).
    pub condition: Option<ConditionFn>,
    pub pk: KeyDef,
    /// `None` when the index has no sort key.
    pub sk: Option<KeyDef>,
    pub projection: Option<Projection>,
}

/// A pk or sk definition as written.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KeyDef {
    pub field: String,
    /// The composite attributes; `None` when none were written.
    pub composite: Option<Vec<String>>,
    /// The key template, e.g. `user#${userId}`.
    pub template: Option<String>,
    /// `"lower"`, `"upper"`, `"none"` or `"default"`; empty means default.
    pub casing: String,
    /// `"string"` or `"number"`; empty means unset.
    pub cast: String,
}

/// An index projection.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Projection {
    /// `"all"` or `"keys_only"` when `attributes` is empty.
    pub kind: String,
    pub attributes: Vec<String>,
}

/// Pads string forms of key composites, like `String.padStart`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Padding {
    pub length: i64,
    pub char: String,
}

/// The fields holding the entity name and version.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Identifiers {
    pub entity: String,
    pub version: String,
}
