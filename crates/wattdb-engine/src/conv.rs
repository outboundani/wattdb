//! Conversions (conversions.js and the `_from*To*` helpers in entity.js),
//! find/match index selection, and parse.

use crate::error::{Error, Result};
use crate::item::{FormatOptions, format_item, owns_item};
use crate::keys::{KeyOptions, index_keys, merge_attrs};
use crate::model::display_index;
use crate::model::{Index, IndexType, KeyType, Model};
use crate::pager::{
    decode_cursor, deconstruct_index, encode_cursor, item_includes_keys, owns_keys,
};
use crate::value::{Attrs, Value};
use crate::write::expect_facets;

fn conv_err(msg: impl Into<String>) -> Error {
    Error::Conversion(msg.into())
}

/// How strictly a conversion needs composites (ElectroDB's `strict`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Strict {
    /// Builds whatever keys the composites allow.
    #[default]
    None,
    /// Needs every partition-key composite.
    Pk,
    /// Needs every composite.
    All,
}

/// Mirrors `_constructPagerIndex`.
fn pager_index(
    m: &Model,
    ix: &Index,
    item: &Attrs,
    relaxed_pk: bool,
    relaxed_sk: bool,
) -> Result<Attrs> {
    let pick = |names: &[String], relaxed: bool| -> Result<Attrs> {
        if relaxed {
            return Ok(names
                .iter()
                .filter_map(|n| item.get(n).map(|v| (n.clone(), v.clone())))
                .collect());
        }
        expect_facets(item, names)
    };
    let pk = pick(&ix.pk.composites, relaxed_pk)?;
    let sk = pick(&ix.sk.composites, relaxed_sk)?;
    if ix.ty == IndexType::Composite {
        let mut out = Attrs::new();
        for (k, v) in merge_attrs(&[&pk, &sk]) {
            if let Some(a) = m.attribute(&k) {
                out.insert(a.field.clone(), v);
            }
        }
        return Ok(out);
    }
    let keys = index_keys(m, ix, &pk, &[&sk], KeyOptions::default())?;
    let mut out = Attrs::new();
    out.insert(ix.pk.field.clone(), keys.pk.unwrap_or(Value::Null));
    if ix.has_sk
        && let Some(sk) = keys.sk.into_iter().next()
    {
        out.insert(ix.sk.field.clone(), sk);
    }
    Ok(out)
}

/// Mirrors `_formatSuppliedPager`.
fn supplied_pager(
    m: &Model,
    ix: &Index,
    item: &Attrs,
    relaxed_pk: bool,
    relaxed_sk: bool,
) -> Result<Option<Attrs>> {
    if item.is_empty() {
        return Ok(None);
    }
    let mut out = pager_index(m, ix, item, relaxed_pk, relaxed_sk)?;
    let table = m.table_index();
    if ix.id != table.id {
        out.extend(pager_index(m, table, item, relaxed_pk, relaxed_sk)?);
    }
    Ok(Some(out))
}

/// Mirrors `byAccessPattern.<ap>.fromComposite.toKeys`.
pub fn composite_to_keys_by_index(
    m: &Model,
    ix: &Index,
    provided: &Attrs,
    strict: Strict,
) -> Result<Option<Attrs>> {
    supplied_pager(
        m,
        ix,
        provided,
        strict != Strict::Pk && strict != Strict::All,
        strict != Strict::All,
    )
}

/// Mirrors `fromComposite.toKeys`.
pub fn composite_to_keys(m: &Model, provided: &Attrs, strict: Strict) -> Result<Attrs> {
    if provided.is_empty() {
        return Err(conv_err("composite is empty"));
    }
    let secondary = if strict == Strict::All || strict == Strict::Pk {
        Strict::Pk
    } else {
        Strict::None
    };
    let mut out = Attrs::new();
    for ix in &m.indexes {
        let s = if ix.name.is_empty() {
            strict
        } else {
            secondary
        };
        if let Some(keys) = composite_to_keys_by_index(m, ix, provided, s)? {
            out.extend(keys);
        }
    }
    if out.is_empty() {
        return Err(conv_err("composite didn't form any keys"));
    }
    Ok(out)
}

/// Mirrors `fromComposite.toCursor` (strict "all").
pub fn composite_to_cursor(m: &Model, provided: &Attrs) -> Result<Option<String>> {
    let keys = composite_to_keys(m, provided, Strict::All)?;
    encode_cursor(&keys)
}

fn translate_from_fields(m: &Model, item: &Attrs) -> Attrs {
    m.attributes
        .iter()
        .filter_map(|a| item.get(&a.field).map(|v| (a.name.clone(), v.clone())))
        .collect()
}

/// Mirrors `_verifyKeys`.
fn verify_keys(m: &Model, ix: &Index, provided: &Attrs) -> bool {
    if ix.ty == IndexType::Composite {
        let item = translate_from_fields(m, provided);
        return ix.pk.composites.iter().all(|n| item.contains_key(n));
    }
    if !provided.contains_key(&ix.pk.field) {
        return false;
    }
    !ix.has_sk || provided.contains_key(&ix.sk.field)
}

/// Mirrors `byAccessPattern.<ap>.fromKeys.toCursor`.
pub fn keys_to_cursor_by_index(m: &Model, ix: &Index, provided: &Attrs) -> Result<Option<String>> {
    if !verify_keys(m, m.table_index(), provided) {
        return Err(conv_err(
            "keys don't include the primary index's key fields",
        ));
    }
    if !verify_keys(m, ix, provided) {
        return Err(conv_err(format!(
            "keys don't include index {}'s key fields",
            display_index(&ix.name)
        )));
    }
    encode_cursor(provided)
}

/// Mirrors `fromKeys.toCursor`.
pub fn keys_to_cursor(provided: &Attrs) -> Result<Option<String>> {
    if provided.is_empty() {
        return Err(conv_err("keys are empty"));
    }
    encode_cursor(provided)
}

/// Mirrors `byAccessPattern.<ap>.fromKeys.toComposite`.
pub fn keys_to_composite_by_index(m: &Model, ix: &Index, provided: &Attrs) -> Result<Attrs> {
    let mut all = if ix.ty == IndexType::Composite {
        let item = translate_from_fields(m, provided);
        ix.composites()
            .into_iter()
            .filter_map(|n| item.get(&n).map(|v| (n.clone(), v.clone())))
            .collect()
    } else {
        deconstruct_index(m, ix, provided).ok_or_else(|| {
            conv_err(format!(
                "keys don't include valid values for index {}",
                display_index(&ix.name)
            ))
        })?
    };
    let table = m.table_index();
    if ix.id != table.id {
        match deconstruct_index(m, table, provided) {
            Some(t) => all.extend(t),
            None => return Ok(all),
        }
    }
    if all.is_empty() {
        return Err(conv_err("keys didn't form any composite attributes"));
    }
    Ok(all)
}

/// Mirrors `fromKeys.toComposite`.
pub fn keys_to_composite(m: &Model, provided: &Attrs) -> Result<Attrs> {
    if provided.is_empty() {
        return Err(conv_err("keys are empty"));
    }
    let mut out = Attrs::new();
    for ix in &m.indexes {
        for (k, v) in keys_to_composite_by_index(m, ix, provided)? {
            out.entry(k).or_insert(v);
        }
    }
    if out.is_empty() {
        return Err(conv_err("keys didn't form any composite attributes"));
    }
    Ok(out)
}

/// Mirrors `fromCursor.toKeys`.
pub fn cursor_to_keys(cursor: &str) -> Result<Attrs> {
    if cursor.is_empty() {
        return Err(conv_err("cursor is empty"));
    }
    Ok(decode_cursor(cursor)?.unwrap_or_default())
}

/// Mirrors `fromCursor.toComposite`.
pub fn cursor_to_composite(m: &Model, cursor: &str) -> Result<Attrs> {
    keys_to_composite(m, &cursor_to_keys(cursor)?)
}

/// Mirrors `byAccessPattern.<ap>.fromCursor.toComposite`.
pub fn cursor_to_composite_by_index(m: &Model, ix: &Index, cursor: &str) -> Result<Attrs> {
    keys_to_composite_by_index(m, ix, &cursor_to_keys(cursor)?)
}

/// Mirrors `byAccessPattern.<ap>.fromComposite.toCursor`.
pub fn composite_to_cursor_by_index(
    m: &Model,
    ix: &Index,
    provided: &Attrs,
) -> Result<Option<String>> {
    if provided.is_empty() {
        return Err(conv_err("composite is empty"));
    }
    let keys = supplied_pager(m, ix, provided, false, false)?.unwrap_or_default();
    keys_to_cursor_by_index(m, ix, &keys)
}

/// Mirrors `_findBestIndexKeyMatch`: the access pattern a find or match
/// should query, or `None` when none fits (a scan).
pub fn best_index(m: &Model, attrs: &Attrs) -> Option<String> {
    #[derive(Clone)]
    struct Slot {
        ix: usize,
        name: String,
        kt: KeyType,
        next: bool,
    }
    struct Match {
        ix: usize,
        all_keys: bool,
        has_sk: bool,
        done: bool,
        count: usize,
    }
    let n = m.indexes.len();
    // slots[j][s]: composite j of index s.
    let mut slots: Vec<Vec<Option<Slot>>> = Vec::new();
    for (s, ix) in m.indexes.iter().enumerate() {
        let comps = ix.composites();
        for (j, name) in comps.iter().enumerate() {
            while slots.len() <= j {
                slots.push(vec![None; n]);
            }
            let kt = if j >= ix.pk.composites.len() {
                KeyType::Sk
            } else {
                KeyType::Pk
            };
            slots[j][s] = Some(Slot {
                ix: ix.id,
                name: name.clone(),
                kt,
                next: j + 1 < comps.len(),
            });
        }
    }
    let mut matches: Vec<Option<Match>> = (0..n).map(|_| None).collect();
    for j in 0..slots.len() {
        for s in 0..n {
            let sl = slots[j][s].clone();
            if matches[s].is_none() {
                let Some(sl) = &sl else { continue };
                matches[s] = Some(Match {
                    ix: sl.ix,
                    all_keys: false,
                    has_sk: false,
                    done: false,
                    count: 0,
                });
            }
            let next_is_sk = j + 1 < slots.len()
                && slots[j + 1][s]
                    .as_ref()
                    .is_some_and(|x| x.kt == KeyType::Sk);
            let mt = matches[s].as_mut().unwrap();
            let provided = sl.as_ref().is_some_and(|sl| attrs.contains_key(&sl.name));
            if mt.done || sl.is_none() || !provided {
                mt.done = true;
                continue;
            }
            let sl = sl.unwrap();
            if sl.next && next_is_sk {
                mt.has_sk = true;
            } else if !sl.next {
                mt.all_keys = true;
            }
            mt.count += 1;
        }
    }
    let eligible: Vec<&Match> = matches
        .iter()
        .flatten()
        .filter(|mt| mt.has_sk || mt.all_keys)
        .collect();
    let max = eligible.iter().map(|mt| mt.count).max().unwrap_or(0);
    let mut ranked: [Option<usize>; 4] = [None; 4];
    for mt in eligible {
        let is_table = m.indexes[mt.ix].name.is_empty();
        if is_table && mt.all_keys {
            ranked[0] = Some(mt.ix);
        } else if is_table && mt.count == max {
            ranked[1] = Some(mt.ix);
        } else if mt.all_keys {
            ranked[2] = Some(mt.ix);
        } else if mt.count == max {
            ranked[3] = Some(mt.ix);
        }
    }
    ranked
        .iter()
        .flatten()
        .next()
        .map(|&ix| m.indexes[ix].access_pattern.clone())
}

/// The response forms `parse` accepts.
#[derive(Clone, Debug)]
pub enum ParseInput {
    Item(Attrs),
    Items(Vec<Attrs>),
    Attributes(Attrs),
}

/// What `parse` returns.
#[derive(Clone, Debug, PartialEq)]
pub enum ParseOutput {
    Item(Option<Attrs>),
    Items(Vec<Attrs>),
}

/// Mirrors `entity.parse`: format a raw response item, ignoring ownership
/// unless asked not to.
pub fn parse(
    m: &Model,
    resp: &ParseInput,
    opt: &FormatOptions,
    ignore_ownership: bool,
) -> Result<ParseOutput> {
    let owns = |item: &Attrs| {
        if ignore_ownership && !opt.attributes.is_empty() && !item_includes_keys(m, item) {
            return true;
        }
        if ignore_ownership && owns_keys(m, item) {
            return true;
        }
        owns_item(m, item)
    };
    match resp {
        ParseInput::Item(item) => {
            if !owns(item) {
                return Ok(ParseOutput::Item(None));
            }
            Ok(ParseOutput::Item(Some(format_item(m, item, opt)?)))
        }
        ParseInput::Items(items) => {
            let mut out = Vec::new();
            for it in items {
                if owns(it) {
                    out.push(format_item(m, it, opt)?);
                }
            }
            Ok(ParseOutput::Items(out))
        }
        ParseInput::Attributes(attrs) => {
            let data = format_item(m, attrs, opt)?;
            Ok(ParseOutput::Item(if data.is_empty() {
                None
            } else {
                Some(data)
            }))
        }
    }
}
