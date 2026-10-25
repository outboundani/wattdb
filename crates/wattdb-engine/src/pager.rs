//! Cursors, ownership, read formatting and the page loop of queries and
//! scans (ElectroDB's executeQuery, hydrate and cursor formatting).

use std::collections::HashMap;
use std::future::Future;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use regex::Regex;

use crate::error::{Error, Result};
use crate::item::{FormatOptions, format_item, owns_item};
use crate::keys::{KeyOptions, index_keys, primary_key};
use crate::model::{AttrType, Cast, Index, Key, Model};
use crate::query::QueryRequest;
use crate::request::Params;
use crate::value::{Attrs, Number, Value, key_string};

/// Mirrors ElectroDB's `cursorFormatter.serialize`: base64url of the JSON
/// key. Keys are written sorted (decision D32). An empty key is no cursor.
pub fn encode_cursor(key: &Attrs) -> Result<Option<String>> {
    if key.is_empty() {
        return Ok(None);
    }
    let mut b = String::from("{");
    for (i, (k, v)) in key.iter().enumerate() {
        if i > 0 {
            b.push(',');
        }
        b.push_str(&serde_json::to_string(k).unwrap_or_default());
        b.push(':');
        match v {
            Value::String(s) => b.push_str(&serde_json::to_string(s).unwrap_or_default()),
            Value::Number(n) => b.push_str(n.as_str()),
            v => {
                return Err(Error::invalid(format!(
                    "wattdb: can't put {v:?} in a cursor"
                )));
            }
        }
    }
    b.push('}');
    Ok(Some(URL_SAFE_NO_PAD.encode(b)))
}

/// Mirrors `cursorFormatter.deserialize`.
pub fn decode_cursor(cursor: &str) -> Result<Option<Attrs>> {
    if cursor.is_empty() {
        return Ok(None);
    }
    let raw = URL_SAFE_NO_PAD
        .decode(cursor.trim_end_matches('='))
        .map_err(|e| Error::InvalidCursor(e.to_string()))?;
    let obj: std::collections::BTreeMap<String, Box<serde_json::value::RawValue>> =
        serde_json::from_slice(&raw).map_err(|e| Error::InvalidCursor(e.to_string()))?;
    let mut out = Attrs::new();
    for (k, v) in obj {
        let text = v.get().trim();
        if text.starts_with('"') {
            let s: String =
                serde_json::from_str(text).map_err(|e| Error::InvalidCursor(e.to_string()))?;
            out.insert(k, Value::String(s));
        } else if serde_json::from_str::<serde_json::Number>(text).is_ok() {
            out.insert(k, Value::Number(Number::new(text)));
        } else {
            return Err(Error::InvalidCursor(format!(
                "unsupported key value for {k:?}"
            )));
        }
    }
    Ok(Some(out))
}

/// Mirrors `_trimKeysToIndex`: keep the index's and the table's key fields.
fn trim_keys(m: &Model, ix: &Index, keys: &Attrs) -> Attrs {
    let table = m.table_index();
    let mut out = Attrs::new();
    for f in [&ix.pk.field, &ix.sk.field, &table.pk.field, &table.sk.field] {
        if f.is_empty() {
            continue;
        }
        if let Some(v) = keys.get(f) {
            out.insert(f.clone(), v.clone());
        }
    }
    out
}

/// Mirrors `_createKeyDeconstructor`.
struct KeyDeconstructor {
    re: Regex,
    names: Vec<String>,
    types: Vec<Option<AttrType>>,
    numeric: bool,
}

impl KeyDeconstructor {
    fn new(m: &Model, k: &Key) -> Self {
        let mut pattern = format!("^{}", regex::escape(&k.prefix));
        let mut names = Vec::new();
        let mut types = Vec::new();
        for l in &k.labels {
            if k.custom && l.name.is_empty() && !l.label.is_empty() {
                pattern.push_str(&regex::escape(&l.label));
            } else if k.custom {
                pattern.push_str(&regex::escape(&l.label));
                pattern.push_str("(.+)");
            } else {
                pattern.push('#');
                pattern.push_str(&regex::escape(&l.label));
                pattern.push_str("_(.+)");
            }
            names.push(l.name.clone());
            types.push(m.attribute(&l.name).map(|a| a.ty));
        }
        if k.has_postfix {
            pattern.push_str(&regex::escape(&k.postfix));
        }
        let re = Regex::new(&format!("(?i){pattern}$")).expect("key pattern");
        KeyDeconstructor {
            re,
            names,
            types,
            numeric: k.cast == Cast::Number,
        }
    }

    /// The composites in a key, or `None` when it doesn't match.
    fn parse(&self, key: &Value) -> Option<Attrs> {
        let (s, is_num) = match key {
            Value::String(s) => (s.as_str(), false),
            Value::Number(n) => (n.as_str(), true),
            _ => return None,
        };
        let numeric = self.numeric && is_num;
        let caps = self.re.captures(s);
        if caps.is_none() && !numeric {
            return None;
        }
        let mut out = Attrs::new();
        for (i, name) in self.names.iter().enumerate() {
            let v: String = if numeric {
                s.to_string()
            } else {
                match caps.as_ref().and_then(|c| c.get(i + 1)) {
                    Some(m) => m.as_str().to_string(),
                    None => continue,
                }
            };
            let value = match self.types[i] {
                Some(AttrType::Number) => parse_float(&v),
                Some(AttrType::Boolean) => Value::Bool(v == "true"),
                _ => Value::String(v),
            };
            if !name.is_empty() {
                out.insert(name.clone(), value);
            }
        }
        Some(out)
    }
}

/// Mirrors JS `parseFloat` for numeric key composites; NaN is null.
fn parse_float(s: &str) -> Value {
    let s = s.trim_start();
    let end = s
        .bytes()
        .position(|c| !(c.is_ascii_digit() || matches!(c, b'.' | b'-' | b'+' | b'e' | b'E')))
        .unwrap_or(s.len());
    let mut prefix = &s[..end];
    while !prefix.is_empty() {
        if let Ok(f) = prefix.parse::<f64>() {
            return Value::Number(Number::from_f64(f));
        }
        prefix = &prefix[..prefix.len() - 1];
    }
    Value::Null
}

/// Mirrors `_deconstructIndex`.
pub(crate) fn deconstruct_index(m: &Model, ix: &Index, keys: &Attrs) -> Option<Attrs> {
    let pk = keys.get(&ix.pk.field)?;
    let mut out = KeyDeconstructor::new(m, &ix.pk).parse(pk)?;
    if ix.has_sk {
        let sk = keys.get(&ix.sk.field)?;
        out.extend(KeyDeconstructor::new(m, &ix.sk).parse(sk)?);
    }
    Some(out)
}

/// Mirrors `_formatKeysToItem`: the composites in an item's keys, or `None`
/// when they don't form a complete table key.
pub fn keys_to_item(m: &Model, ix: &Index, keys: &Attrs) -> Option<Attrs> {
    if keys.is_empty() {
        return None;
    }
    let mut parts = deconstruct_index(m, ix, keys)?;
    let table = m.table_index();
    if ix.id != table.id {
        parts.extend(deconstruct_index(m, table, keys)?);
    }
    if table.composites().iter().any(|n| !parts.contains_key(n)) {
        return None;
    }
    Some(parts)
}

/// Mirrors `ownsKeys`: the item's table keys have this entity's shape.
pub(crate) fn owns_keys(m: &Model, item: &Attrs) -> bool {
    let table = m.table_index();
    let pk = &table.pk;
    let pk_match = match item.get(&pk.field) {
        Some(Value::String(v)) => v.starts_with(&pk.prefix),
        Some(Value::Number(_)) => pk.cast == Cast::Number,
        _ => false,
    };
    let mut sk_match = pk_match && !table.has_sk;
    if pk_match && table.has_sk {
        let sk = &table.sk;
        sk_match = match item.get(&sk.field) {
            Some(Value::Number(_)) => sk.cast == Cast::Number,
            Some(Value::String(v)) => {
                (sk.prefix.is_empty() || v.starts_with(&sk.prefix))
                    && (sk.postfix.is_empty() || v.ends_with(&sk.postfix))
            }
            _ => false,
        };
    }
    pk_match && sk_match && keys_to_item(m, table, item).is_some()
}

pub(crate) fn item_includes_keys(m: &Model, item: &Attrs) -> bool {
    let table = m.table_index();
    if !item.contains_key(&table.pk.field) {
        return false;
    }
    !table.has_sk || item.contains_key(&table.sk.field)
}

fn projection_hides_identifiers(m: &Model, ix: &Index) -> bool {
    let Some(p) = &ix.projection else {
        return false;
    };
    if p.attributes.is_empty() {
        return p.kind == "keys_only";
    }
    let has_e = p.attributes.contains(&m.identifiers.entity);
    let has_v = p.attributes.contains(&m.identifiers.version);
    !(has_e && has_v)
}

/// Mirrors `Entity.is` with the ownership options of a read.
pub(crate) fn owns_as(
    m: &Model,
    item: &Attrs,
    ignore: bool,
    hydrate: bool,
    attributes: &[String],
) -> bool {
    if ignore && !attributes.is_empty() && !item_includes_keys(m, item) {
        return true;
    }
    if (ignore || hydrate) && owns_keys(m, item) {
        return true;
    }
    owns_item(m, item)
}

/// Formats one batch get item; `ignore` is the user's ignoreOwnership.
pub fn format_batch_item(
    m: &Model,
    item: &Attrs,
    ignore: bool,
    attributes: &[String],
) -> Result<Option<Attrs>> {
    let owned = owns_item(m, item) || (ignore && owns_keys(m, item));
    if !owned {
        return Ok(None);
    }
    Ok(Some(format_item(
        m,
        item,
        &FormatOptions {
            include_keys: false,
            attributes: attributes.to_vec(),
        },
    )?))
}

/// Builds the table keys for a batch get of composites.
pub fn batch_get_keys(m: &Model, composites: &[Attrs]) -> Result<Vec<Attrs>> {
    composites.iter().map(|c| primary_key(m, c)).collect()
}

/// Performs the DynamoDB calls of a query, in engine values.
pub trait Runner {
    type Error: From<Error>;

    /// Runs the request (with its ExclusiveStartKey, if any), returning the
    /// items and LastEvaluatedKey.
    fn page(
        &mut self,
        params: Params,
    ) -> impl Future<Output = Result<(Vec<Attrs>, Option<Attrs>), Self::Error>> + Send;

    /// Fetches table keys, returning the items found; missing and
    /// unprocessed keys are simply absent.
    fn batch_get(
        &mut self,
        table: &str,
        keys: Vec<Attrs>,
    ) -> impl Future<Output = Result<Vec<Attrs>, Self::Error>> + Send;
}

/// Controls how many pages `run` reads.
#[derive(Clone, Debug, Default)]
pub struct RunOptions {
    /// The number of pages to read; 0 means all.
    pub pages: usize,
    /// Stops after this many items (0 means no limit).
    pub count: usize,
    /// The user's own ignoreOwnership, used when hydrating.
    pub ignore_ownership: bool,
    pub attributes: Vec<String>,
}

/// What `run` returns.
#[derive(Clone, Debug, Default)]
pub struct RunResult {
    pub items: Vec<Attrs>,
    /// The LastEvaluatedKey to continue from.
    pub next: Option<Attrs>,
    /// The keys hydrate couldn't fetch.
    pub unprocessed: Vec<Attrs>,
    /// A collection query's items, per member.
    pub groups: Vec<Vec<Attrs>>,
}

impl<'m> QueryRequest<'m> {
    /// Turns a cursor into the request's ExclusiveStartKey.
    pub fn start_key(&self, cursor: &str) -> Result<Option<Attrs>> {
        Ok(decode_cursor(cursor)?.map(|keys| trim_keys(self.q.m, self.q.index(), &keys)))
    }

    /// Mirrors how ElectroDB decides ignoreOwnership: an index projecting
    /// only keys, or attributes without the identifiers, can't be checked
    /// by identifiers.
    pub fn ignores_ownership(&self) -> bool {
        self.opts.ignore_ownership || projection_hides_identifiers(self.q.m, self.q.index())
    }

    /// Formats one page's items (formatResponse for Items), dropping items
    /// this entity doesn't own. With hydrate, items keep their keys.
    pub fn format_items(&self, items: &[Attrs]) -> Result<Vec<Attrs>> {
        let ignore = self.ignores_ownership() || self.opts.hydrate;
        let opt = FormatOptions {
            include_keys: self.opts.include_keys || self.opts.hydrate,
            attributes: self.opts.attributes.clone(),
        };
        let m = self.q.m;
        let mut out = Vec::new();
        for item in items {
            if !owns_as(m, item, ignore, self.opts.hydrate, &self.opts.attributes) {
                continue;
            }
            out.push(format_item(m, item, &opt)?);
        }
        Ok(out)
    }

    /// Mirrors `_fromCompositeToKeysByIndex` (relaxed): the index and table
    /// keys for a formatted item, used as the cursor when Count stops early.
    pub fn keys_from_item(&self, item: &Attrs) -> Result<Attrs> {
        let m = self.q.m;
        let build = |ix: &Index| -> Result<Attrs> {
            let pick = |names: &[String]| -> Attrs {
                names
                    .iter()
                    .filter_map(|n| item.get(n).map(|v| (n.clone(), v.clone())))
                    .collect()
            };
            let (pk, sk) = (pick(&ix.pk.composites), pick(&ix.sk.composites));
            let keys = index_keys(m, ix, &pk, &[&sk], KeyOptions::default())?;
            let mut out = Attrs::new();
            out.insert(ix.pk.field.clone(), keys.pk.unwrap_or(Value::Null));
            if ix.has_sk
                && let Some(sk) = keys.sk.into_iter().next()
            {
                out.insert(ix.sk.field.clone(), sk);
            }
            Ok(out)
        };
        let ix = self.q.index();
        let mut out = build(ix)?;
        if ix.id != m.table_index().id {
            out.extend(build(m.table_index())?);
        }
        Ok(out)
    }

    /// Mirrors `executeQuery` for queries and scans.
    pub async fn run<R: Runner>(
        &self,
        start: Option<Attrs>,
        opt: &RunOptions,
        run: &mut R,
    ) -> Result<RunResult, R::Error> {
        if self.q.coll.is_some() {
            return self.run_collection(start, opt, run).await;
        }
        let mut res = RunResult::default();
        let mut esk = start;
        let (mut iterations, mut count) = (0usize, 0usize);
        loop {
            let (items, lek) = run.page(self.with_start_key(esk.as_ref())).await?;
            esk = lek;
            let mut formatted = self.format_items(&items)?;
            let prev = count;
            if opt.count > 0 {
                count += formatted.len();
            }
            let more = opt.count > 0 && count > opt.count;
            if more {
                formatted.truncate(opt.count - prev);
            }
            if self.opts.hydrate {
                let (data, unprocessed) = self.hydrate_for(self.q.m, &formatted, opt, run).await?;
                formatted = data;
                res.unprocessed.extend(unprocessed);
            }
            res.items.extend(formatted);
            if more || (opt.count > 0 && count == opt.count) {
                esk = match res.items.last() {
                    Some(last) => Some(self.keys_from_item(last)?),
                    None => None,
                };
                break;
            }
            iterations += 1;
            let has_more = esk.as_ref().is_some_and(|k| !k.is_empty());
            if !(has_more
                && (opt.pages == 0 || opt.count > 0 || iterations < opt.pages)
                && (opt.count == 0 || count < opt.count))
            {
                break;
            }
        }
        res.next = esk.filter(|k| !k.is_empty());
        Ok(res)
    }

    /// Mirrors `Entity.hydrate`: fetch the full items behind keys.
    async fn hydrate_for<R: Runner>(
        &self,
        m: &Model,
        items: &[Attrs],
        opt: &RunOptions,
        run: &mut R,
    ) -> Result<(Vec<Attrs>, Vec<Attrs>), R::Error> {
        let table = m.table_index();
        let mut composites = Vec::new();
        let mut valid_keys = Vec::new();
        for it in items {
            if let Some(c) = keys_to_item(m, table, it) {
                composites.push(c);
                valid_keys.push(it.clone());
            }
        }
        if composites.is_empty() {
            return Ok((Vec::new(), Vec::new()));
        }
        let keys = batch_get_keys(m, &composites)?;
        let table_name = self.params.table_name.clone().unwrap_or_default();
        let found = run.batch_get(&table_name, keys.clone()).await?;
        // preserveBatchOrder: match results to keys by their table key values.
        let id = |k: &Attrs| {
            format!(
                "{}\u{0}{}",
                key_string(k.get(&table.pk.field).unwrap_or(&Value::Null)),
                key_string(k.get(&table.sk.field).unwrap_or(&Value::Null))
            )
        };
        let mut slots: HashMap<String, Attrs> = HashMap::new();
        for it in &found {
            if let Some(data) = format_batch_item(m, it, opt.ignore_ownership, &opt.attributes)? {
                slots.insert(id(it), data);
            }
        }
        let mut data = Vec::new();
        let mut unprocessed = Vec::new();
        for (i, k) in keys.iter().enumerate() {
            match slots.get(&id(k)) {
                Some(d) => data.push(d.clone()),
                None => unprocessed.push(valid_keys[i].clone()),
            }
        }
        Ok((data, unprocessed))
    }

    /// Mirrors `matchToEntityAlias` for collection items: the first member
    /// whose identifiers match, or that otherwise owns the item.
    fn member_of(&self, item: &Attrs, ignore: bool, hydrate: bool) -> Option<usize> {
        for (i, mem) in self.members().iter().enumerate() {
            let m = mem.model;
            let e = item.get(&m.identifiers.entity);
            let v = item.get(&m.identifiers.version);
            if e == Some(&Value::String(m.entity.clone()))
                && v == Some(&Value::String(m.version.clone()))
            {
                return Some(i);
            }
            if owns_as(m, item, ignore, hydrate, &self.opts.attributes) {
                return Some(i);
            }
        }
        None
    }

    /// Mirrors `cleanseRetrievedData`: one page's items, grouped by member.
    /// With hydrate, items are kept as they are, for hydration.
    pub fn format_groups(&self, items: &[Attrs]) -> Result<Vec<Vec<Attrs>>> {
        let hydrate = self.opts.hydrate;
        let ignore = self.ignores_ownership() || hydrate;
        let opt = FormatOptions {
            include_keys: self.opts.include_keys || hydrate,
            attributes: self.opts.attributes.clone(),
        };
        let members = self.members();
        let mut groups: Vec<Vec<Attrs>> = vec![Vec::new(); members.len()];
        for item in items {
            let Some(i) = self.member_of(item, ignore, hydrate) else {
                continue;
            };
            if hydrate {
                groups[i].push(item.clone());
                continue;
            }
            let m = members[i].model;
            if owns_as(m, item, ignore, hydrate, &self.opts.attributes) {
                groups[i].push(format_item(m, item, &opt)?);
            }
        }
        Ok(groups)
    }

    /// Mirrors `executeQuery` for collection queries: pages are merged per
    /// member; Count doesn't limit them.
    async fn run_collection<R: Runner>(
        &self,
        start: Option<Attrs>,
        opt: &RunOptions,
        run: &mut R,
    ) -> Result<RunResult, R::Error> {
        let members = self.members();
        let mut res = RunResult {
            groups: vec![Vec::new(); members.len()],
            ..Default::default()
        };
        let mut esk = start;
        let mut iterations = 0usize;
        loop {
            let (items, lek) = run.page(self.with_start_key(esk.as_ref())).await?;
            esk = lek;
            let groups = self.format_groups(&items)?;
            for (i, g) in groups.into_iter().enumerate() {
                let mut g = g;
                if self.opts.hydrate && !g.is_empty() {
                    let (data, unprocessed) =
                        self.hydrate_for(members[i].model, &g, opt, run).await?;
                    g = data;
                    res.unprocessed.extend(unprocessed);
                }
                res.groups[i].extend(g);
            }
            iterations += 1;
            let has_more = esk.as_ref().is_some_and(|k| !k.is_empty());
            if !(has_more && (opt.pages == 0 || opt.count > 0 || iterations < opt.pages)) {
                break;
            }
        }
        res.next = esk.filter(|k| !k.is_empty());
        Ok(res)
    }
}
