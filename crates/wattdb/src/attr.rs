//! Attribute descriptors and conditions (decisions D34, D36).
//!
//! The derive macros generate a descriptor per attribute, as an associated
//! function named after the field (`Task::status()`). Descriptors build
//! conditions (`Task::status().eq(Status::Open)`) and name the attributes
//! updates change (`.set(Task::title(), "x")`).

use std::marker::PhantomData;
use std::ops::Deref;

use indexmap::IndexMap;
use wattdb_engine::Path;
use wattdb_engine::write::Cond as EngineCond;

use crate::codec::FieldType;
use crate::error::{Error, Result};
use crate::value::{Attrs, Value, from_attribute_value};

/// Encodes a descriptor's values.
pub type Encoder<T> = fn(&T) -> Result<Value>;

/// A generated attribute descriptor of entity `E`. Every descriptor can be
/// set and removed; the marker traits below say what else it allows.
pub trait Descriptor<E>: Clone + Send + Sync + 'static {
    /// The attribute's Rust type.
    type Value;

    /// Builds the descriptor; generated code calls it.
    fn new(path: Vec<String>, encode: Encoder<Self::Value>) -> Self;

    /// The attribute's path: the root field, then map properties or list
    /// indexes.
    fn path(&self) -> &[String];

    /// Encodes a value of the attribute.
    fn encode(&self, v: &Self::Value) -> Result<Value>;
}

/// A number or set descriptor (`ADD`).
pub trait Addable<E>: Descriptor<E> {}
/// A number descriptor.
pub trait Subtractable<E>: Descriptor<E> {}
/// A list descriptor.
pub trait Appendable<E>: Descriptor<E> {}
/// A set descriptor (`DELETE`).
pub trait Deletable<E>: Descriptor<E> {}

/// The ElectroDB path of a descriptor, e.g. `profile.displayName`.
pub fn path_string<E, D: Descriptor<E>>(d: &D) -> String {
    Path(d.path().to_vec()).to_string()
}

/// A condition for `where_` (decision D36). Build them from descriptors
/// (`User::email().eq("a@b.co")`) and combine them with [`and`], [`or`] and
/// [`not`].
#[derive(Clone, Debug)]
pub struct Cond(std::result::Result<EngineCond, String>);

impl Cond {
    pub(crate) fn engine(&self) -> Result<EngineCond> {
        self.0.clone().map_err(Error::Encode)
    }

    fn op(op: &str, path: &[String], values: Result<Vec<Value>>) -> Cond {
        Cond(
            values
                .map_err(|e| e.to_string())
                .map(|values| EngineCond::Op {
                    op: op.into(),
                    path: Path(path.to_vec()),
                    values,
                }),
        )
    }

    /// ANDs another condition with this one.
    pub fn and(self, other: Cond) -> Cond {
        and([self, other])
    }

    /// ORs another condition with this one.
    pub fn or(self, other: Cond) -> Cond {
        or([self, other])
    }
}

fn combine(conds: impl IntoIterator<Item = Cond>) -> std::result::Result<Vec<EngineCond>, String> {
    conds.into_iter().map(|c| c.0).collect()
}

/// True when every condition is, rendered `(a) AND (b)`.
pub fn and(conds: impl IntoIterator<Item = Cond>) -> Cond {
    Cond(combine(conds).map(EngineCond::And))
}

/// True when any condition is, rendered `(a) OR (b)`.
pub fn or(conds: impl IntoIterator<Item = Cond>) -> Cond {
    Cond(combine(conds).map(EngineCond::Or))
}

/// Negates a condition, rendered `NOT (a)`.
pub fn not(c: Cond) -> Cond {
    Cond(c.0.map(|c| EngineCond::Not(Box::new(c))))
}

impl std::ops::Not for Cond {
    type Output = Cond;

    fn not(self) -> Cond {
        not(self)
    }
}

/// A name or value referenced by a [`raw`] expression.
#[derive(Clone, Debug)]
pub enum RawArg {
    Name(String),
    Value(Value),
}

/// A field name for [`raw`], referenced as `#n0`, `#n1`, ... in order.
pub fn name(field: impl Into<String>) -> RawArg {
    RawArg::Name(field.into())
}

/// A value for [`raw`], referenced as `:v0`, `:v1`, ... in order.
pub fn value<T: FieldType>(v: T) -> RawArg {
    match v.encode() {
        Ok(v) => RawArg::Value(v),
        Err(e) => RawArg::Value(Value::String(format!("<{e}>"))),
    }
}

/// A value for [`raw`] given as an SDK attribute value.
pub fn attribute_value(av: aws_sdk_dynamodb::types::AttributeValue) -> RawArg {
    RawArg::Value(from_attribute_value(&av))
}

/// Numbers raw arguments: names `#<p>n0...` and values `:<p>v0...`.
pub(crate) fn raw_args(args: &[RawArg], prefix: &str) -> (IndexMap<String, String>, Attrs) {
    let (mut names, mut values) = (IndexMap::new(), Attrs::new());
    let (mut n, mut v) = (0, 0);
    for a in args {
        match a {
            RawArg::Name(f) => {
                names.insert(format!("#{prefix}n{n}"), f.clone());
                n += 1;
            }
            RawArg::Value(x) => {
                values.insert(format!(":{prefix}v{v}"), x.clone());
                v += 1;
            }
        }
    }
    (names, values)
}

/// A condition written as a DynamoDB expression. Names and values are
/// numbered in order: the first name is `#n0`, the first value `:v0`.
///
/// ```
/// let c = wattdb::raw("size(#n0) > :v0", [wattdb::name("tags"), wattdb::value(3)]);
/// ```
pub fn raw(expr: impl Into<String>, args: impl IntoIterator<Item = RawArg>) -> Cond {
    let args: Vec<RawArg> = args.into_iter().collect();
    let (names, values) = raw_args(&args, "");
    Cond(Ok(EngineCond::Raw {
        expression: expr.into(),
        names,
        values,
    }))
}

/// Describes an attribute of entity `E` holding a `T`.
pub struct Attr<E, T> {
    path: Vec<String>,
    enc: Encoder<T>,
    _e: PhantomData<fn() -> E>,
}

impl<E, T> Clone for Attr<E, T> {
    fn clone(&self) -> Self {
        Attr {
            path: self.path.clone(),
            enc: self.enc,
            _e: PhantomData,
        }
    }
}

impl<E, T> std::fmt::Debug for Attr<E, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Attr({})", Path(self.path.clone()))
    }
}

impl<E: 'static, T: 'static> Descriptor<E> for Attr<E, T> {
    type Value = T;

    fn new(path: Vec<String>, encode: Encoder<T>) -> Self {
        Attr {
            path,
            enc: encode,
            _e: PhantomData,
        }
    }

    fn path(&self) -> &[String] {
        &self.path
    }

    fn encode(&self, v: &T) -> Result<Value> {
        (self.enc)(v).map_err(|e| {
            Error::encode(format!(
                "{}: {}",
                Path(self.path.clone()),
                e.to_string().trim_start_matches("wattdb: ")
            ))
        })
    }
}

impl<E: 'static, T: 'static> Attr<E, T> {
    /// The path of a property or list item under this attribute.
    pub fn child(&self, name: impl Into<String>) -> Vec<String> {
        let mut p = self.path.clone();
        p.push(name.into());
        p
    }

    fn cond(&self, op: &str, vals: Vec<T>) -> Cond {
        Cond::op(
            op,
            &self.path,
            vals.iter().map(|v| Descriptor::encode(self, v)).collect(),
        )
    }

    pub fn eq(&self, v: impl Into<T>) -> Cond {
        self.cond("eq", vec![v.into()])
    }

    pub fn ne(&self, v: impl Into<T>) -> Cond {
        self.cond("ne", vec![v.into()])
    }

    pub fn gt(&self, v: impl Into<T>) -> Cond {
        self.cond("gt", vec![v.into()])
    }

    pub fn gte(&self, v: impl Into<T>) -> Cond {
        self.cond("gte", vec![v.into()])
    }

    pub fn lt(&self, v: impl Into<T>) -> Cond {
        self.cond("lt", vec![v.into()])
    }

    pub fn lte(&self, v: impl Into<T>) -> Cond {
        self.cond("lte", vec![v.into()])
    }

    /// An inclusive range.
    pub fn between(&self, lo: impl Into<T>, hi: impl Into<T>) -> Cond {
        self.cond("between", vec![lo.into(), hi.into()])
    }

    pub fn begins_with(&self, v: impl Into<T>) -> Cond {
        self.cond("begins", vec![v.into()])
    }

    pub fn exists(&self) -> Cond {
        self.cond("exists", vec![])
    }

    pub fn not_exists(&self) -> Cond {
        self.cond("notExists", vec![])
    }

    /// DynamoDB's `contains()` with a value of the attribute's type, e.g. a
    /// substring of a string attribute.
    pub fn contains(&self, v: impl Into<T>) -> Cond {
        self.cond("contains", vec![v.into()])
    }

    pub fn not_contains(&self, v: impl Into<T>) -> Cond {
        self.cond("notContains", vec![v.into()])
    }

    /// DynamoDB's `attribute_type()`, with a type code such as `"S"` or
    /// `"N"`.
    pub fn is_type(&self, code: &str) -> Cond {
        Cond::op("type", &self.path, Ok(vec![Value::String(code.into())]))
    }
}

macro_rules! wrapper_descriptor {
    ($name:ident <E, $($p:ident),*> => $value:ty, base: $base:ty) => {
        impl<E, $($p),*> Clone for $name<E, $($p),*> {
            fn clone(&self) -> Self {
                $name { base: self.base.clone(), _p: PhantomData }
            }
        }

        impl<E: 'static, $($p: 'static),*> Deref for $name<E, $($p),*> {
            type Target = $base;

            fn deref(&self) -> &$base {
                &self.base
            }
        }
    };
}

/// Describes a number attribute; `add` and `subtract` accept it.
pub struct NumberAttr<E, T> {
    base: Attr<E, T>,
    _p: PhantomData<fn() -> T>,
}

wrapper_descriptor!(NumberAttr<E, T> => T, base: Attr<E, T>);

impl<E: 'static, T: Send + Sync + 'static> Descriptor<E> for NumberAttr<E, T> {
    type Value = T;

    fn new(path: Vec<String>, encode: Encoder<T>) -> Self {
        NumberAttr {
            base: Attr::new(path, encode),
            _p: PhantomData,
        }
    }

    fn path(&self) -> &[String] {
        self.base.path()
    }

    fn encode(&self, v: &T) -> Result<Value> {
        self.base.encode(v)
    }
}

impl<E: 'static, T: Send + Sync + 'static> Addable<E> for NumberAttr<E, T> {}
impl<E: 'static, T: Send + Sync + 'static> Subtractable<E> for NumberAttr<E, T> {}

/// Describes a set attribute of `I` items, held as a `C`; `add` and `delete`
/// accept it.
pub struct SetAttr<E, I, C> {
    base: Attr<E, C>,
    _p: PhantomData<fn() -> I>,
}

wrapper_descriptor!(SetAttr<E, I, C> => C, base: Attr<E, C>);

impl<E: 'static, I: FieldType, C: Send + Sync + 'static> Descriptor<E> for SetAttr<E, I, C> {
    type Value = C;

    fn new(path: Vec<String>, encode: Encoder<C>) -> Self {
        SetAttr {
            base: Attr::new(path, encode),
            _p: PhantomData,
        }
    }

    fn path(&self) -> &[String] {
        self.base.path()
    }

    fn encode(&self, v: &C) -> Result<Value> {
        self.base.encode(v)
    }
}

impl<E: 'static, I: FieldType, C: Send + Sync + 'static> Addable<E> for SetAttr<E, I, C> {}
impl<E: 'static, I: FieldType, C: Send + Sync + 'static> Deletable<E> for SetAttr<E, I, C> {}

impl<E: 'static, I: FieldType, C: 'static> SetAttr<E, I, C> {
    /// True when the set holds `v`.
    pub fn contains(&self, v: impl Into<I>) -> Cond {
        Cond::op(
            "contains",
            self.base.path(),
            v.into().encode().map(|v| vec![v]),
        )
    }

    pub fn not_contains(&self, v: impl Into<I>) -> Cond {
        Cond::op(
            "notContains",
            self.base.path(),
            v.into().encode().map(|v| vec![v]),
        )
    }
}

/// Describes a list attribute of `I` items; `append` accepts it, and `at`
/// addresses one item.
pub struct ListAttr<E, I> {
    base: Attr<E, Vec<I>>,
    _p: PhantomData<fn() -> I>,
}

wrapper_descriptor!(ListAttr<E, I> => Vec<I>, base: Attr<E, Vec<I>>);

impl<E: 'static, I: FieldType> Descriptor<E> for ListAttr<E, I> {
    type Value = Vec<I>;

    fn new(path: Vec<String>, encode: Encoder<Vec<I>>) -> Self {
        ListAttr {
            base: Attr::new(path, encode),
            _p: PhantomData,
        }
    }

    fn path(&self) -> &[String] {
        self.base.path()
    }

    fn encode(&self, v: &Vec<I>) -> Result<Value> {
        self.base.encode(v)
    }
}

impl<E: 'static, I: FieldType> Appendable<E> for ListAttr<E, I> {}

impl<E: 'static, I: FieldType> ListAttr<E, I> {
    /// Addresses item `i` (ElectroDB's `path[i]`).
    pub fn at(&self, i: usize) -> I::Attr<E> {
        Descriptor::new(self.base.child(i.to_string()), I::encode)
    }

    /// True when the list holds `v`.
    pub fn contains(&self, v: impl Into<I>) -> Cond {
        Cond::op(
            "contains",
            self.base.path(),
            v.into().encode().map(|v| vec![v]),
        )
    }

    pub fn not_contains(&self, v: impl Into<I>) -> Cond {
        Cond::op(
            "notContains",
            self.base.path(),
            v.into().encode().map(|v| vec![v]),
        )
    }
}

/// Names the attributes a read projects: one descriptor, a tuple of them,
/// or a `Vec`/array of one kind.
pub trait AttrList<E> {
    fn root_fields(&self) -> Vec<String>;
}

impl<E: 'static, T: 'static> AttrList<E> for Attr<E, T> {
    fn root_fields(&self) -> Vec<String> {
        vec![self.path[0].clone()]
    }
}

impl<E: 'static, T: Send + Sync + 'static> AttrList<E> for NumberAttr<E, T> {
    fn root_fields(&self) -> Vec<String> {
        vec![self.path()[0].clone()]
    }
}

impl<E: 'static, I: FieldType, C: Send + Sync + 'static> AttrList<E> for SetAttr<E, I, C> {
    fn root_fields(&self) -> Vec<String> {
        vec![self.path()[0].clone()]
    }
}

impl<E: 'static, I: FieldType> AttrList<E> for ListAttr<E, I> {
    fn root_fields(&self) -> Vec<String> {
        vec![self.path()[0].clone()]
    }
}

impl<E: 'static, D: Descriptor<E>> AttrList<E> for Vec<D> {
    fn root_fields(&self) -> Vec<String> {
        self.iter().map(|d| d.path()[0].clone()).collect()
    }
}

impl<E: 'static, D: Descriptor<E>, const N: usize> AttrList<E> for [D; N] {
    fn root_fields(&self) -> Vec<String> {
        self.iter().map(|d| d.path()[0].clone()).collect()
    }
}

macro_rules! tuple_attr_list {
    ($($d:ident),+) => {
        impl<E: 'static, $($d: Descriptor<E>),+> AttrList<E> for ($($d,)+) {
            #[allow(non_snake_case)]
            fn root_fields(&self) -> Vec<String> {
                let ($($d,)+) = self;
                vec![$($d.path()[0].clone()),+]
            }
        }
    };
}

tuple_attr_list!(A);
tuple_attr_list!(A, B);
tuple_attr_list!(A, B, C);
tuple_attr_list!(A, B, C, D);
tuple_attr_list!(A, B, C, D, F);
tuple_attr_list!(A, B, C, D, F, G);
tuple_attr_list!(A, B, C, D, F, G, H);
tuple_attr_list!(A, B, C, D, F, G, H, I);
tuple_attr_list!(A, B, C, D, F, G, H, I, J);
tuple_attr_list!(A, B, C, D, F, G, H, I, J, K);
tuple_attr_list!(A, B, C, D, F, G, H, I, J, K, L);
tuple_attr_list!(A, B, C, D, F, G, H, I, J, K, L, M);
