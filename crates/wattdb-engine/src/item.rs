//! The attribute pipeline: casting, defaults, validation, setters and
//! getters (ElectroDB's Attribute classes and Schema).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::error::{
    CallbackError, Error, FieldError, IncompleteCompositesError, Result, ValidationError,
};
use crate::keys::put_keys;
use crate::model::{AttrType, Attribute, Casing, Model, format_casing};
use crate::value::{
    Attrs, Value, js_number, js_string, js_typeof, json_stringify, lookup, value_type,
};

fn field_error(a: &Attribute, reason: impl Into<String>) -> FieldError {
    FieldError {
        path: a.path.clone(),
        reason: reason.into(),
        index: None,
        cause: None,
    }
}

pub(crate) fn invalid(a: &Attribute, reason: impl Into<String>) -> Error {
    ValidationError {
        fields: vec![field_error(a, reason)],
    }
    .into()
}

/// Mirrors `Attribute._makeDefault`.
fn default_value(a: &Attribute) -> Result<Value> {
    match &a.default {
        None => Ok(Value::Undefined),
        Some(f) => f().map_err(Error::callback),
    }
}

/// Mirrors `Attribute._makeCast`.
pub(crate) fn cast(a: &Attribute, v: Value) -> Result<Value> {
    match a.cast.as_str() {
        "string" => {
            if v.is_undefined() {
                return Err(invalid(
                    a,
                    format!(
                        "attribute {} is undefined and cannot be cast to type string",
                        a.name
                    ),
                ));
            }
            if let Value::String(s) = v {
                return Ok(Value::String(s));
            }
            Ok(Value::String(json_stringify(&v)))
        }
        "number" => {
            if v.is_undefined() {
                return Err(invalid(
                    a,
                    format!(
                        "attribute {} is undefined and cannot be cast to type number",
                        a.name
                    ),
                ));
            }
            if let Value::Number(n) = v {
                return Ok(Value::Number(n));
            }
            match js_number(&v) {
                Some(n) => Ok(Value::Number(n)),
                None => Err(invalid(
                    a,
                    format!(
                        "attribute {} cannot be cast to type number; doing so results in NaN",
                        a.name
                    ),
                )),
            }
        }
        _ => Ok(v),
    }
}

/// Mirrors the attribute classes' `val()`: casting and defaults,
/// recursively for maps, lists and sets.
pub(crate) fn val(a: &Attribute, v: Value) -> Result<Value> {
    match a.ty {
        AttrType::Map => {
            let data = if v.is_undefined() {
                default_value(a)?
            } else {
                v
            };
            if data.is_undefined() {
                return Ok(Value::Undefined);
            }
            let Value::Map(obj) = data else {
                return Err(invalid(
                    a,
                    format!(
                        "Invalid value type at entity path: {:?}. Expected value to be an object to fulfill attribute type \"map\"",
                        a.path
                    ),
                ));
            };
            let mut out = Attrs::new();
            for p in &a.properties {
                let r = val(p, lookup(&obj, &p.name))?;
                if !r.is_undefined() {
                    out.insert(p.name.clone(), r);
                }
            }
            Ok(Value::Map(out))
        }
        AttrType::List => {
            let vtype = value_type(&v);
            let mut data = v;
            if data.is_undefined() {
                data = cast(a, data)?;
                if data.is_undefined() {
                    data = default_value(a)?;
                }
            }
            if data.is_undefined() {
                return Ok(Value::Undefined);
            }
            let Value::List(list) = data else {
                return Err(invalid(
                    a,
                    format!(
                        "Invalid value type at entity path {:?}. Received value of type {:?}, expected value of type \"array\"",
                        a.path, vtype
                    ),
                ));
            };
            let items = a.items.as_ref().expect("list items");
            let mut out = Vec::new();
            for d in list {
                let r = val(items, d)?;
                if !r.is_undefined() {
                    out.push(r);
                }
            }
            Ok(Value::List(out))
        }
        AttrType::Set => {
            let v = if v.is_undefined() {
                default_value(a)?
            } else {
                v
            };
            if v.is_undefined() {
                return Ok(Value::Undefined);
            }
            to_set(a, v)
        }
        _ => {
            let v = cast(a, v)?;
            if v.is_undefined() {
                return default_value(a);
            }
            Ok(v)
        }
    }
}

/// Mirrors `SetAttribute.toDDBSet` with the v3 DocumentClient: arrays and
/// sets are deduplicated, and a single string or number becomes a one-item
/// set.
fn to_set(a: &Attribute, v: Value) -> Result<Value> {
    match v {
        Value::Set(items) | Value::List(items) => Ok(Value::Set(dedupe(items))),
        v @ (Value::String(_) | Value::Number(_)) => {
            get_validate(a.items.as_ref().expect("set items"), v.clone())?;
            Ok(Value::Set(vec![v]))
        }
        v => Err(invalid(
            a,
            format!(
                "Invalid attribute value supplied to \"set\" attribute {:?}. Received value of type {:?}. Set values must be supplied as either Arrays, native JavaScript Set objects, DocumentClient Set objects, strings, or numbers.",
                a.path,
                value_type(&v)
            ),
        )),
    }
}

/// Keeps the first occurrence of each value, like `new Set(array)`. Maps
/// and lists are never equal to each other, as JS compares objects by
/// identity.
fn dedupe(items: Vec<Value>) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for it in items {
        if !matches!(it, Value::Map(_) | Value::List(_)) && out.contains(&it) {
            continue;
        }
        out.push(it);
    }
    out
}

/// Mirrors `Attribute.isValid`: the type check, then validate when the type
/// is right.
fn is_valid(a: &Attribute, v: &Value) -> (bool, Vec<FieldError>) {
    let (typed, mut reasons) = is_type(a, v);
    if !typed {
        return (false, reasons);
    }
    let (ok, verrs) = validate(a, v);
    reasons.extend(verrs);
    (ok, reasons)
}

fn is_type(a: &Attribute, v: &Value) -> (bool, Vec<FieldError>) {
    if v.is_undefined() {
        if a.required {
            return (
                false,
                vec![field_error(
                    a,
                    format!(
                        "Invalid value type at entity path: {:?}. Value is required.",
                        a.path
                    ),
                )],
            );
        }
        return (true, vec![]);
    }
    match a.ty {
        AttrType::Enum => {
            if let Value::String(s) = v {
                if a.enum_values.iter().any(|e| e == s) {
                    return (true, vec![]);
                }
            }
            (
                false,
                vec![field_error(
                    a,
                    format!(
                        "Invalid value type at entity path: {:?}. Value not found in set of acceptable values: {}",
                        a.path,
                        quote_list(&a.enum_values)
                    ),
                )],
            )
        }
        AttrType::Any => (true, vec![]),
        AttrType::Map => {
            let Value::Map(obj) = v else {
                return (
                    false,
                    vec![field_error(
                        a,
                        format!(
                            "Invalid value type at entity path {:?}. Received value of type {:?}, expected value of type \"object\"",
                            a.path,
                            value_type(v)
                        ),
                    )],
                );
            };
            let mut errs = Vec::new();
            for p in &a.properties {
                let (ok, e) = is_valid(p, &lookup(obj, &p.name));
                if !ok {
                    errs.extend(e);
                }
            }
            (errs.is_empty(), errs)
        }
        AttrType::List => {
            let Value::List(list) = v else {
                return (
                    false,
                    vec![field_error(
                        a,
                        format!(
                            "Invalid value type at entity path {:?}. Received value of type {:?}, expected value of type \"array\"",
                            a.path,
                            value_type(v)
                        ),
                    )],
                );
            };
            let items = a.items.as_ref().expect("list items");
            let mut errs = Vec::new();
            for (i, item) in list.iter().enumerate() {
                let (ok, e) = is_valid(items, item);
                if !ok {
                    errs.extend(e.into_iter().map(|mut fe| {
                        fe.index = Some(i);
                        fe
                    }));
                }
            }
            (errs.is_empty(), errs)
        }
        AttrType::Set => {
            let items_list = match v {
                Value::Set(s) | Value::List(s) => s,
                _ => {
                    return (
                        false,
                        vec![field_error(
                            a,
                            format!(
                                "Invalid value type at attribute path: {:?}. Expected value to be an Array, native JavaScript Set objects, or DocumentClient Set objects to fulfill attribute type \"set\"",
                                a.path
                            ),
                        )],
                    );
                }
            };
            let items = a.items.as_ref().expect("set items");
            let mut errs = Vec::new();
            for item in items_list {
                let (ok, e) = is_valid(items, item);
                if !ok {
                    errs.extend(e);
                }
            }
            (errs.is_empty(), errs)
        }
        _ => {
            let got = js_typeof(v);
            if got != a.ty.as_str() {
                return (
                    false,
                    vec![field_error(
                        a,
                        format!(
                            "Invalid value type at entity path: {:?}. Received value of type {:?}, expected value of type {:?}",
                            a.path,
                            got,
                            a.ty.as_str()
                        ),
                    )],
                );
            }
            (true, vec![])
        }
    }
}

/// Mirrors `Attribute._makeValidate` (and SetAttribute's, which passes the
/// items as an array).
fn validate(a: &Attribute, v: &Value) -> (bool, Vec<FieldError>) {
    let as_list;
    let v = match (a.ty, v) {
        (AttrType::Set, Value::Set(items)) => {
            as_list = Value::List(items.clone());
            &as_list
        }
        _ => v,
    };
    if let Some(f) = &a.validate {
        return match f(v) {
            Err(err) => {
                let cause = CallbackError(Arc::from(err));
                let mut fe = field_error(a, cause.to_string());
                fe.cause = Some(cause);
                (false, vec![fe])
            }
            Ok(false) => (false, vec![field_error(a, "Invalid value provided")]),
            Ok(true) => (true, vec![]),
        };
    }
    if let Some(re) = &a.validate_regex {
        if v.is_undefined() {
            return (true, vec![]);
        }
        if !re.is_match(&js_string(v)) {
            return (
                false,
                vec![field_error(
                    a,
                    format!(
                        "Invalid value for attribute {:?}: Failed model defined regex",
                        a.path
                    ),
                )],
            );
        }
    }
    (true, vec![])
}

/// Mirrors `Attribute.getValidate`: val, then isValid.
pub(crate) fn get_validate(a: &Attribute, v: Value) -> Result<Value> {
    let v = val(a, v)?;
    let (ok, errs) = is_valid(a, &v);
    if !ok {
        return Err(ValidationError { fields: errs }.into());
    }
    Ok(v)
}

/// Mirrors `Schema.checkCreate`: every root attribute goes through cast,
/// default and validation, stopping at the first invalid one. The record has
/// an entry for every root attribute, `Undefined` when missing.
pub fn check_create(m: &Model, payload: &Attrs) -> Result<Attrs> {
    let mut record = Attrs::new();
    for a in &m.attributes {
        let v = get_validate(a, lookup(payload, &a.name))?;
        record.insert(a.name.clone(), v);
    }
    Ok(record)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Method {
    Get,
    Set,
}

fn has_handler(a: &Attribute, mth: Method) -> bool {
    match mth {
        Method::Get => a.get.is_some(),
        Method::Set => a.set.is_some(),
    }
}

/// Runs an attribute's get or set, including the built-in handling of maps,
/// lists and sets.
fn mutate(a: &Attribute, mth: Method, v: Value, siblings: &Attrs) -> Result<Value> {
    match mth {
        Method::Get => get(a, v, siblings),
        Method::Set => set(a, v, siblings),
    }
}

fn run_transform(
    f: &Option<crate::model::TransformFn>,
    v: Value,
    siblings: &Attrs,
) -> Result<Value> {
    match f {
        None => Ok(v),
        Some(f) => f(v, siblings).map_err(Error::callback),
    }
}

/// Mirrors the attribute classes' `_makeSet`.
fn set(a: &Attribute, v: Value, siblings: &Attrs) -> Result<Value> {
    match a.ty {
        AttrType::Map => {
            if v.is_undefined() {
                if a.set.is_none() {
                    return Ok(Value::Undefined);
                }
                return run_transform(&a.set, v, siblings);
            }
            let obj = match v {
                Value::Map(m) => m,
                _ => Attrs::new(),
            };
            let mut data = Attrs::new();
            for p in &a.properties {
                let pv = lookup(&obj, &p.name);
                if pv.is_undefined() {
                    continue;
                }
                let r = set(p, pv, &obj)?;
                if !r.is_undefined() {
                    data.insert(p.field.clone(), r);
                }
            }
            run_transform(&a.set, Value::Map(data), siblings)
        }
        AttrType::List => {
            if v.is_undefined() {
                return run_transform(&a.set, v, siblings);
            }
            let list = match v {
                Value::List(l) => l,
                _ => Vec::new(),
            };
            let items = a.items.as_ref().expect("list items");
            let sib = list_siblings(&list);
            let mut data = Vec::new();
            for item in list {
                let r = set(items, item, &sib)?;
                if !r.is_undefined() {
                    data.push(r);
                }
            }
            run_transform(&a.set, Value::List(data), siblings)
        }
        AttrType::Set => {
            let r = run_transform(&a.set, from_set(v), siblings)?;
            if r.is_undefined() {
                return Ok(Value::Undefined);
            }
            to_set(a, r)
        }
        _ => run_transform(&a.set, v, siblings),
    }
}

/// Mirrors passing `[...values]` as a list item's siblings.
fn list_siblings(list: &[Value]) -> Attrs {
    list.iter()
        .enumerate()
        .map(|(i, v)| (i.to_string(), v.clone()))
        .collect()
}

/// Mirrors `SetAttribute.fromDDBSet`.
fn from_set(v: Value) -> Value {
    match v {
        Value::Set(items) => Value::List(items),
        v => v,
    }
}

/// Mirrors the attribute classes' `_makeGet`.
fn get(a: &Attribute, v: Value, siblings: &Attrs) -> Result<Value> {
    if a.hidden {
        return Ok(Value::Undefined);
    }
    match a.ty {
        AttrType::Map => {
            if v.is_undefined() {
                if a.get.is_none() {
                    return Ok(Value::Undefined);
                }
                return run_transform(&a.get, Value::Map(Attrs::new()), siblings);
            }
            let obj = match v {
                Value::Map(m) => m,
                _ => Attrs::new(),
            };
            let mut data = Attrs::new();
            for p in &a.properties {
                let pv = lookup(&obj, &p.field);
                if pv.is_undefined() {
                    continue;
                }
                let r = get(p, pv, &obj)?;
                if !r.is_undefined() {
                    data.insert(p.name.clone(), r);
                }
            }
            run_transform(&a.get, Value::Map(data), siblings)
        }
        AttrType::List => {
            if v.is_undefined() {
                return run_transform(&a.get, Value::List(Vec::new()), siblings);
            }
            let Value::List(list) = v else {
                return run_transform(&a.get, v, siblings);
            };
            let items = a.items.as_ref().expect("list items");
            let sib = list_siblings(&list);
            let mut data = Vec::new();
            for item in list {
                let r = get(items, item, &sib)?;
                if !r.is_undefined() {
                    data.push(r);
                }
            }
            run_transform(&a.get, Value::List(data), siblings)
        }
        AttrType::Set => {
            if !v.is_undefined() {
                return run_transform(&a.get, from_set(v), siblings);
            }
            let r = run_transform(&a.get, Value::Undefined, siblings)?;
            if r.is_undefined() {
                return Ok(Value::Undefined);
            }
            Ok(from_set(r))
        }
        _ => run_transform(&a.get, unformat(a, v), siblings),
    }
}

/// Mirrors `Attribute._makeDestructureKey`: strings stored in a key field
/// lose the key template's prefix and postfix.
fn unformat(a: &Attribute, v: Value) -> Value {
    let Value::String(s) = &v else { return v };
    if a.ty != AttrType::String && a.ty != AttrType::Enum {
        return v;
    }
    if s.len() <= a.key_prefix.len() {
        return v;
    }
    Value::String(remove_fixings(&a.key_prefix, &a.key_postfix, s))
}

pub(crate) fn remove_fixings(prefix: &str, postfix: &str, value: &str) -> String {
    if prefix.is_empty() && postfix.is_empty() {
        return value.to_string();
    }
    let lower = format_casing(value, Casing::Lower);
    let mut start = 0;
    if lower.starts_with(&format_casing(prefix, Casing::Lower)) {
        start = prefix.len();
    }
    let mut end = value.len();
    if lower.ends_with(&format_casing(postfix, Casing::Lower)) {
        end -= postfix.len();
    }
    if end < start {
        return String::new();
    }
    value.get(start..end).unwrap_or_default().to_string()
}

/// Resolves a payload key the way ElectroDB's traverser does: by attribute
/// path (root names, or nested paths in updates), or by field.
fn root_attribute<'a>(m: &'a Model, key: &str) -> Option<&'a Arc<Attribute>> {
    m.attribute_at_path(key)
        .or_else(|| m.attributes.iter().find(|a| a.field == key))
}

/// Mirrors `Schema._fulfillAttributeMutationMethod`: non-watchers first,
/// then the watchers of whatever is present, with the first pass's results
/// as their siblings.
pub(crate) fn fulfill(
    m: &Model,
    mth: Method,
    payload: &Attrs,
    order: &[String],
) -> Result<(Attrs, Vec<String>)> {
    let mut watchers: HashSet<String> = HashSet::new();
    let mut watch_all: Vec<String> = Vec::new();
    let mut to_watchers: HashMap<String, Vec<String>> = HashMap::new();
    let mut has_watchers = false;
    for a in &m.attributes {
        if a.is_watcher() && has_handler(a, mth) {
            has_watchers = true;
            watchers.insert(a.name.clone());
        } else if a.watch_all && has_handler(a, mth) {
            has_watchers = true;
            watchers.insert(a.name.clone());
            watch_all.push(a.name.clone());
        } else {
            let ws = a
                .watched_by
                .iter()
                .filter(|w| m.attribute(w).is_some_and(|wa| has_handler(wa, mth)))
                .cloned()
                .collect();
            to_watchers.insert(a.name.clone(), ws);
        }
    }

    let apply = |include: &[String],
                 avoid: &dyn Fn(&str) -> bool,
                 payload: &Attrs|
     -> Result<(Attrs, Vec<String>)> {
        let mut data = payload.clone();
        for key in include {
            let Some(a) = root_attribute(m, key) else {
                continue;
            };
            if avoid(key) {
                continue;
            }
            let r = mutate(a, mth, lookup(payload, key), payload)?;
            data.insert(key.clone(), r);
        }
        let mut keys: Vec<String> = order.to_vec();
        for k in include {
            if !keys.contains(k) {
                keys.push(k.clone());
            }
        }
        Ok((data, keys))
    };

    let (data, keys) = apply(order, &|k| watchers.contains(k), payload)?;
    if !has_watchers {
        return Ok((data, keys));
    }
    let mut include: Vec<String> = keys.clone();
    for k in &keys {
        if let Some(ws) = to_watchers.get(k) {
            for w in ws {
                if !include.contains(w) {
                    include.push(w.clone());
                }
            }
        }
    }
    for w in &watch_all {
        if !include.contains(w) {
            include.push(w.clone());
        }
    }
    apply(&include, &|k| to_watchers.contains_key(k), &data)
}

/// The root attribute names in order.
pub(crate) fn root_names(m: &Model) -> Vec<String> {
    m.attributes.iter().map(|a| a.name.clone()).collect()
}

/// Builds the item a put or create writes, mirroring the put clause and
/// `_makePutParams`: checkCreate, the primary key from the validated record,
/// setters (watchers last), keys, field translation and identifiers.
pub fn put_item(m: &Model, payload: &Attrs) -> Result<Attrs> {
    let record = check_create(m, payload)?;
    let mut primary = Attrs::new();
    let mut missing = Vec::new();
    for name in m.table_index().composites() {
        let v = record.get(&name).cloned().unwrap_or_default();
        if v.is_undefined() {
            missing.push(name);
            continue;
        }
        primary.insert(name, v);
    }
    if !missing.is_empty() {
        return Err(IncompleteCompositesError {
            missing,
            ..Default::default()
        }
        .into());
    }
    put_item_from_record(m, &record, &primary)
}

/// Builds the put item from a validated record (put_item without the
/// checkCreate step).
pub(crate) fn put_item_from_record(m: &Model, record: &Attrs, primary: &Attrs) -> Result<Attrs> {
    let (applied, _) = fulfill(m, Method::Set, record, &root_names(m))?;
    let defined: Attrs = applied
        .into_iter()
        .filter(|(_, v)| !v.is_undefined())
        .collect();
    let keys = put_keys(m, &defined, primary)?;
    let mut item = Attrs::new();
    for a in &m.attributes {
        if let Some(v) = defined.get(&a.name) {
            item.insert(a.field.clone(), v.clone());
        }
    }
    item.extend(keys);
    item.insert(
        m.identifiers.entity.clone(),
        Value::String(m.entity.clone()),
    );
    item.insert(
        m.identifiers.version.clone(),
        Value::String(m.version.clone()),
    );
    Ok(item)
}

/// Controls read formatting.
#[derive(Clone, Debug, Default)]
pub struct FormatOptions {
    /// Keeps fields that aren't attributes (keys, identifiers).
    pub include_keys: bool,
    /// When set, limits the result to these attributes.
    pub attributes: Vec<String>,
}

/// Mirrors `Schema.formatItemForRetrieval`: fields to attribute names,
/// getters (watchers last), then hidden and unrequested attributes removed.
pub fn format_item(m: &Model, item: &Attrs, opt: &FormatOptions) -> Result<Attrs> {
    let by_field: HashMap<&str, &str> = m
        .attributes
        .iter()
        .map(|a| (a.field.as_str(), a.name.as_str()))
        .collect();
    let mut remapped = Attrs::new();
    let mut order = Vec::new();
    for (field, v) in item {
        if let Some(name) = by_field.get(field.as_str()) {
            remapped.insert(name.to_string(), v.clone());
            order.push(name.to_string());
        } else if opt.include_keys {
            remapped.insert(field.clone(), v.clone());
            order.push(field.clone());
        }
    }
    let (data, _) = fulfill(m, Method::Get, &remapped, &order)?;
    let mut out = Attrs::new();
    for (k, v) in data {
        if v.is_undefined() {
            continue;
        }
        if m.attribute(&k).is_some_and(|a| a.hidden) {
            continue;
        }
        if !opt.attributes.is_empty() && !opt.attributes.contains(&k) {
            continue;
        }
        out.insert(k, v);
    }
    Ok(out)
}

/// Reports whether an item's identifiers name this entity and version.
pub fn owns_item(m: &Model, item: &Attrs) -> bool {
    item.get(&m.identifiers.entity) == Some(&Value::String(m.entity.clone()))
        && item.get(&m.identifiers.version) == Some(&Value::String(m.version.clone()))
}

fn quote_list(values: &[String]) -> String {
    values
        .iter()
        .map(|v| format!("{v:?}"))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::Number;

    #[test]
    fn casts() {
        let s = Attribute {
            name: "s".into(),
            path: "s".into(),
            cast: "string".into(),
            ..Default::default()
        };
        assert_eq!(
            cast(&s, Value::Number(Number::new("5"))).unwrap(),
            Value::String("5".into())
        );
        assert!(
            cast(&s, Value::Undefined).is_err(),
            "cast string(undefined) should fail, as in ElectroDB"
        );
        let n = Attribute {
            name: "n".into(),
            path: "n".into(),
            cast: "number".into(),
            ..Default::default()
        };
        assert_eq!(
            cast(&n, "42".into()).unwrap(),
            Value::Number(Number::new("42"))
        );
        assert!(cast(&n, "abc".into()).is_err());
    }

    #[test]
    fn removes_fixings() {
        let cases = [
            ("user#", "", "USER#abc", "abc"),
            ("", "#v1", "abc#V1", "abc"),
            ("a#", "#z", "a#mid#z", "mid"),
            ("x#", "", "nope", "nope"),
        ];
        for (prefix, postfix, input, want) in cases {
            assert_eq!(remove_fixings(prefix, postfix, input), want);
        }
    }
}
