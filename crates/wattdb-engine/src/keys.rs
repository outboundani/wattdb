//! Key construction (ElectroDB's `_makeKey` and friends) and index impact.

use std::collections::{BTreeMap, HashMap};

use crate::error::{Error, FieldError, IncompleteCompositesError, Result, ValidationError};
use crate::model::{Cast, Index, KeyType, Model, Padding, format_casing};
use crate::value::{Attrs, Number, Value, key_string};

#[derive(Clone, Copy, Default)]
pub(crate) struct KeyOptions {
    pub exclude_label_tail: bool,
    pub exclude_postfix: bool,
}

/// A built key: a string, a number (for numeric cast keys) or absent when a
/// numeric key has no value.
pub(crate) type KeyValue = Option<Value>;

/// Mirrors ElectroDB's `_makeKey`: the key and whether every label was
/// filled.
pub(crate) fn make_key(
    m: &Model,
    k: &crate::model::Key,
    supplied: &Attrs,
    opt: KeyOptions,
) -> Result<(KeyValue, bool)> {
    if k.cast == Cast::Number {
        let name = k.composites.first().map(String::as_str).unwrap_or("");
        return numeric_cast_key(name, supplied);
    }
    let mut b = String::new();
    b.push_str(&k.prefix);
    let mut found = 0;
    for l in &k.labels {
        let value = supplied.get(&l.name);
        if value.is_none() && opt.exclude_label_tail {
            break;
        }
        if k.custom {
            b.push_str(&l.label);
        } else {
            b.push('#');
            b.push_str(&l.label);
            b.push('_');
        }
        let Some(value) = value else { break };
        found += 1;
        let mut s = key_string(value);
        if let Some(a) = m.attribute(&l.name) {
            s = pad(&s, a.padding.as_ref());
        }
        b.push_str(&s);
    }
    let fulfilled = found == k.labels.len();
    if fulfilled && k.has_postfix && !opt.exclude_postfix {
        b.push_str(&k.postfix);
    }
    Ok((Some(Value::String(format_casing(&b, k.casing))), fulfilled))
}

/// Mirrors ElectroDB's `_formatNumericCastKey`.
pub(crate) fn numeric_cast_key(name: &str, supplied: &Attrs) -> Result<(KeyValue, bool)> {
    let Some(value) = supplied.get(name) else {
        return Ok((None, false));
    };
    match value {
        Value::Number(n) => return Ok((Some(Value::Number(n.clone())), true)),
        Value::String(s) => {
            if let Some(n) = js_parse_int(s) {
                return Ok((Some(Value::Number(n)), true));
            }
        }
        Value::Bool(b) => {
            return Ok((
                Some(Value::Number(Number::new(if *b { "1" } else { "0" }))),
                true,
            ));
        }
        _ => {}
    }
    Err(ValidationError {
        fields: vec![FieldError {
            path: name.to_string(),
            reason: "could not cast composite attribute to number for index".into(),
            index: None,
            cause: None,
        }],
    }
    .into())
}

/// Mirrors `String.prototype.padStart`, measuring in UTF-16 code units.
pub(crate) fn pad(s: &str, p: Option<&Padding>) -> String {
    let Some(p) = p else { return s.to_string() };
    if p.length <= 0 || p.char.is_empty() {
        return s.to_string();
    }
    let n = p.length - s.encode_utf16().count() as i64;
    if n <= 0 {
        return s.to_string();
    }
    let fill: Vec<u16> = p.char.encode_utf16().collect();
    let mut out: Vec<u16> = Vec::with_capacity(n as usize);
    while (out.len() as i64) < n {
        out.extend_from_slice(&fill);
    }
    out.truncate(n as usize);
    String::from_utf16_lossy(&out) + s
}

/// Mirrors ElectroDB's `_makeIndexKeys`: the pk, one sk per entry in `sks`,
/// and whether any sk was fulfilled.
#[derive(Clone, Debug, Default)]
pub struct IndexKeys {
    pub pk: Option<Value>,
    pub sk: Vec<Value>,
    pub fulfilled: bool,
}

pub(crate) fn index_keys(
    m: &Model,
    ix: &Index,
    pk: &Attrs,
    sks: &[&Attrs],
    opt: KeyOptions,
) -> Result<IndexKeys> {
    let empty = Attrs::new();
    let sks: Vec<&Attrs> = if sks.is_empty() {
        vec![&empty]
    } else {
        sks.to_vec()
    };
    let mut out = IndexKeys::default();
    let (key, _) = make_key(m, &ix.pk, pk, KeyOptions::default())?;
    out.pk = key;
    if ix.has_sk {
        for sk in sks {
            let (key, fulfilled) = make_key(m, &ix.sk, sk, opt)?;
            if let Some(key) = key {
                out.sk.push(key);
            }
            if fulfilled {
                out.fulfilled = true;
            }
        }
    }
    Ok(out)
}

/// Builds the table key used by get, delete and update, as ElectroDB's
/// `_makeSimpleIndexParams` does. Every primary composite must be supplied.
pub fn primary_key(m: &Model, attrs: &Attrs) -> Result<Attrs> {
    let ix = m.table_index();
    let missing: Vec<String> = ix
        .composites()
        .into_iter()
        .filter(|n| !attrs.contains_key(n))
        .collect();
    if !missing.is_empty() {
        return Err(IncompleteCompositesError {
            missing,
            ..Default::default()
        }
        .into());
    }
    let keys = index_keys(m, ix, attrs, &[attrs], KeyOptions::default())?;
    let mut out = Attrs::new();
    out.insert(ix.pk.field.clone(), keys.pk.unwrap_or(Value::Null));
    if ix.has_sk {
        if let Some(sk) = keys.sk.into_iter().next() {
            out.insert(ix.sk.field.clone(), sk);
        }
    }
    Ok(out)
}

/// The sort-key operation of a query.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueryType {
    /// A plain access-pattern query, with no sort-key operation.
    Is,
    Begins,
    Between,
    Gt,
    Gte,
    Lt,
    Lte,
    /// The end half of a between, as in ElectroDB.
    And,
    Collection,
    ClusteredCollection,
}

/// Builds a query's key values, as ElectroDB's `_makeQueryKeys` does. `sks`
/// holds the sort-key composites for each key condition: one map, or two for
/// between (start and end). Each must already include the composites passed
/// to the access pattern.
pub fn query_keys(
    m: &Model,
    ix: &Index,
    typ: QueryType,
    pk: &Attrs,
    sks: &[&Attrs],
) -> Result<IndexKeys> {
    let missing: Vec<String> = ix
        .pk
        .composites
        .iter()
        .filter(|n| !pk.contains_key(*n))
        .cloned()
        .collect();
    if !missing.is_empty() {
        return Err(IncompleteCompositesError {
            access_patterns: vec![ix.access_pattern.clone()],
            missing,
            ..Default::default()
        }
        .into());
    }
    let opt = KeyOptions {
        exclude_label_tail: typ != QueryType::Is,
        ..Default::default()
    };
    index_keys(m, ix, pk, sks, opt)
}

/// The key fields a put or create writes, mirroring ElectroDB's
/// `_getPutKeys`. `set` holds the attribute values after defaults and
/// setters; `primary` holds the primary-key composites as passed to put.
pub fn put_keys(m: &Model, set: &Attrs, primary: &Attrs) -> Result<Attrs> {
    Ok(put_keys_deleted(m, set, primary)?.0)
}

/// Mirrors `_getPutKeys`, also returning the key fields a sparse index
/// condition removed (used by upsert).
pub(crate) fn put_keys_deleted(
    m: &Model,
    set: &Attrs,
    primary: &Attrs,
) -> Result<(Attrs, Vec<String>)> {
    let imp = expect_index_facets(m, set, primary, ImpactOptions::default())?;
    let mut deleted = Vec::new();
    for ix in &m.indexes {
        if imp.conditions.get(&ix.id) == Some(&false) {
            deleted.push(ix.pk.field.clone());
            if ix.has_sk {
                deleted.push(ix.sk.field.clone());
            }
        }
    }
    let mut completed = imp.completed.clone();
    let table = m.table_index();
    if !completed.contains(&table.id) {
        completed.push(table.id);
    }

    let merged = merge_attrs(&[primary, set]);
    let mut out = Attrs::new();
    for id in completed {
        let ix = &m.indexes[id];
        if let Some(cond) = &ix.condition {
            if !cond(&merged).map_err(Error::callback)? {
                continue;
            }
        }
        let keys = index_keys(m, ix, &merged, &[&merged], KeyOptions::default())?;
        if let Some(pk) = keys.pk {
            if pk != Value::String(String::new()) {
                out.insert(ix.pk.field.clone(), pk);
            }
        }
        if ix.has_sk {
            if let Some(sk) = keys.sk.into_iter().next() {
                if sk != Value::String(String::new()) {
                    out.insert(ix.sk.field.clone(), sk);
                }
            }
        }
    }
    Ok((out, deleted))
}

/// The options of ElectroDB's `_getIndexImpact`.
#[derive(Clone, Copy, Default)]
pub(crate) struct ImpactOptions {
    /// Also counts sparse indexes whose composites are all in `included`
    /// (update "set").
    pub included_only: bool,
    /// Treats every index as unconditional (update "remove").
    pub skip_conditions: bool,
}

/// The result of `index_impact`.
#[derive(Default)]
pub(crate) struct Impact {
    /// Indexes whose impacted keys are complete.
    pub completed: Vec<usize>,
    /// Each impacted index's impacted key fields; `order` lists those
    /// indexes in the order ElectroDB first saw them.
    pub types: HashMap<usize, BTreeMap<KeyType, String>>,
    pub order: Vec<usize>,
    /// The evaluated sparse-index conditions.
    pub conditions: HashMap<usize, bool>,
    /// The indexes left half-built, with their missing composites.
    pub incomplete: Vec<(usize, Vec<String>)>,
}

#[derive(Default)]
struct KeyImpact {
    names: Vec<String>,
    provided: bool,
}

/// Mirrors ElectroDB's `_getIndexImpact`.
pub(crate) fn index_impact(
    m: &Model,
    attrs: &Attrs,
    included: &Attrs,
    opt: ImpactOptions,
) -> Result<Impact> {
    let mut impacted: HashMap<usize, BTreeMap<KeyType, KeyImpact>> = HashMap::new();
    let mut out = Impact::default();

    fn note_type(out: &mut Impact, ix: &Index, kt: KeyType) {
        if !out.types.contains_key(&ix.id) {
            out.types.insert(ix.id, BTreeMap::new());
            out.order.push(ix.id);
        }
        out.types
            .get_mut(&ix.id)
            .unwrap()
            .insert(kt, ix.key(kt).field.clone());
    }

    for name in m.facet_attributes() {
        if !attrs.contains_key(name) {
            continue;
        }
        for f in m.facets(name) {
            let ix = &m.indexes[f.index];
            if ix.ty == crate::model::IndexType::Composite {
                continue;
            }
            let ki = impacted.entry(ix.id).or_default().entry(f.kt).or_default();
            ki.names.push(name.clone());
            ki.provided = true;
            note_type(&mut out, ix, f.kt);
        }
    }

    if opt.included_only {
        for ix in &m.indexes {
            if ix.ty == crate::model::IndexType::Composite
                || ix.name.is_empty()
                || ix.condition.is_none()
            {
                continue;
            }
            for kt in [KeyType::Pk, KeyType::Sk] {
                let k = ix.key(kt);
                if k.composites.is_empty() || !all_in(&k.composites, included) {
                    continue;
                }
                if kt == KeyType::Sk && !ix.has_sk {
                    continue;
                }
                // Replace the impacted list with the full composite list, as
                // ElectroDB does, keeping any "provided" source.
                let entry = impacted.entry(ix.id).or_default();
                let provided = entry.get(&kt).is_some_and(|p| p.provided);
                entry.insert(
                    kt,
                    KeyImpact {
                        names: k.composites.clone(),
                        provided,
                    },
                );
                note_type(&mut out, ix, kt);
            }
        }
    }

    let mut all: Vec<(usize, Vec<String>)> = Vec::new();
    for ix in &m.indexes {
        if ix.ty == crate::model::IndexType::Composite {
            continue;
        }
        let mut missing = Vec::new();
        if let Some(imp) = impacted.get(&ix.id) {
            let missing_pk = imp
                .get(&KeyType::Pk)
                .is_some_and(|k| k.names.len() != ix.pk.composites.len());
            let missing_sk = imp
                .get(&KeyType::Sk)
                .is_some_and(|k| k.names.len() != ix.sk.composites.len());
            if missing_pk {
                missing.extend(not_in(
                    &ix.pk.composites,
                    &imp[&KeyType::Pk].names,
                    included,
                ));
            }
            if missing_sk {
                missing.extend(not_in(
                    &ix.sk.composites,
                    &imp[&KeyType::Sk].names,
                    included,
                ));
            }
            if !missing_pk && !missing_sk {
                out.completed.push(ix.id);
            }
        }
        all.push((ix.id, missing));
    }

    for (id, missing) in all {
        let ix = &m.indexes[id];
        if opt.skip_conditions || ix.name.is_empty() || ix.condition.is_none() {
            if !missing.is_empty() {
                out.incomplete.push((id, missing));
            }
            out.conditions.insert(id, true);
            continue;
        }
        let imp = impacted.get(&id);
        let member_provided = imp.is_some_and(|imp| imp.values().any(|k| k.provided));
        let composites = ix.composites();
        let all_included = all_in(&composites, included);
        if member_provided || all_included {
            let absent: Vec<String> = composites
                .iter()
                .filter(|n| {
                    (!attrs.contains_key(*n) && !included.contains_key(*n)) || missing.contains(n)
                })
                .cloned()
                .collect();
            if !absent.is_empty() {
                return Err(IncompleteCompositesError {
                    access_patterns: vec![ix.access_pattern.clone()],
                    missing: absent,
                    conditional: true,
                }
                .into());
            }
            let merged = merge_attrs(&[attrs, included]);
            let keep = (ix.condition.as_ref().unwrap())(&merged).map_err(Error::callback)?;
            out.conditions.insert(id, keep);
        } else if !missing.is_empty() {
            out.incomplete.push((id, missing));
        }
    }
    Ok(out)
}

/// Mirrors ElectroDB's `_expectIndexFacets`.
pub(crate) fn expect_index_facets(
    m: &Model,
    attrs: &Attrs,
    included: &Attrs,
    opt: ImpactOptions,
) -> Result<Impact> {
    let imp = index_impact(m, attrs, included, opt)?;
    if !imp.incomplete.is_empty() {
        let mut e = IncompleteCompositesError::default();
        for (id, missing) in &imp.incomplete {
            e.access_patterns
                .push(m.indexes[*id].access_pattern.clone());
            e.missing.extend(missing.iter().cloned());
        }
        return Err(e.into());
    }
    Ok(imp)
}

fn all_in(names: &[String], attrs: &Attrs) -> bool {
    names.iter().all(|n| attrs.contains_key(n))
}

pub(crate) fn merge_attrs(maps: &[&Attrs]) -> Attrs {
    let mut out = Attrs::new();
    for m in maps {
        for (k, v) in m.iter() {
            out.insert(k.clone(), v.clone());
        }
    }
    out
}

/// The composites neither impacted nor included.
fn not_in(composites: &[String], impacted: &[String], included: &Attrs) -> Vec<String> {
    composites
        .iter()
        .filter(|n| !impacted.contains(n) && !included.contains_key(*n))
        .cloned()
        .collect()
}

/// Mirrors JS `parseInt(s)` with no radix, returning the integer's decimal
/// text.
pub(crate) fn js_parse_int(s: &str) -> Option<Number> {
    let mut s = s.trim_start_matches([
        ' ', '\t', '\n', '\u{0b}', '\u{0c}', '\r', '\u{a0}', '\u{feff}', '\u{2028}', '\u{2029}',
    ]);
    let mut neg = false;
    if let Some(rest) = s.strip_prefix('+') {
        s = rest;
    } else if let Some(rest) = s.strip_prefix('-') {
        neg = true;
        s = rest;
    }
    let mut base = 10;
    if s.len() >= 2 && (s.starts_with("0x") || s.starts_with("0X")) {
        base = 16;
        s = &s[2..];
    }
    let end = s
        .bytes()
        .position(|c| digit_value(c) >= base)
        .unwrap_or(s.len());
    if end == 0 {
        return None;
    }
    let digits = &s[..end];
    if base == 16 {
        // Hex values are converted through f64, as JS does.
        let mut f = 0.0f64;
        for c in digits.bytes() {
            f = f * 16.0 + digit_value(c) as f64;
        }
        if neg {
            f = -f;
        }
        return Some(Number::from_f64(f));
    }
    let digits = digits.trim_start_matches('0');
    if digits.is_empty() {
        return Some(Number::new("0"));
    }
    Some(Number::new(if neg {
        format!("-{digits}")
    } else {
        digits.to_string()
    }))
}

fn digit_value(c: u8) -> u32 {
    match c {
        b'0'..=b'9' => (c - b'0') as u32,
        b'a'..=b'z' => (c - b'a') as u32 + 10,
        b'A'..=b'Z' => (c - b'A') as u32 + 10,
        _ => 99,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_int_matches_js() {
        let cases = [
            ("42", Some("42")),
            ("42abc", Some("42")),
            ("  -7", Some("-7")),
            ("+5", Some("5")),
            ("007", Some("7")),
            ("-0", Some("0")),
            ("0x1A", Some("26")),
            ("1.9", Some("1")),
            ("abc", None),
            ("", None),
            ("-", None),
        ];
        for (input, want) in cases {
            assert_eq!(
                js_parse_int(input).as_ref().map(Number::as_str),
                want,
                "js_parse_int({input:?})"
            );
        }
    }

    #[test]
    fn pads_like_pad_start() {
        let p = |length, ch: &str| Padding {
            length,
            char: ch.into(),
        };
        assert_eq!(pad("42", Some(&p(5, "0"))), "00042");
        assert_eq!(pad("x", Some(&p(5, "ab"))), "ababx");
        assert_eq!(pad("123456", Some(&p(3, "0"))), "123456");
        assert_eq!(pad("7", None), "7");
        assert_eq!(pad("7", Some(&p(3, ""))), "7");
    }

    #[test]
    fn numeric_cast_keys() {
        let n = |s: &str| Value::Number(Number::new(s));
        let cases = [
            (n("7"), n("7")),
            ("12px".into(), n("12")),
            (Value::Bool(true), n("1")),
            (Value::Bool(false), n("0")),
        ];
        for (input, want) in cases {
            let mut attrs = Attrs::new();
            attrs.insert("n".into(), input.clone());
            let (got, ok) = numeric_cast_key("n", &attrs).unwrap();
            assert!(ok && got == Some(want), "numeric_cast_key({input:?})");
        }
        let mut attrs = Attrs::new();
        attrs.insert("n".into(), "abc".into());
        assert!(numeric_cast_key("n", &attrs).is_err());
        assert_eq!(numeric_cast_key("n", &Attrs::new()).unwrap(), (None, false));
    }
}
