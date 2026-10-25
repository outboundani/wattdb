use std::collections::HashMap;
use std::sync::Arc;

use super::*;
use crate::error::{Error, Result};

impl Model {
    /// Normalizes a definition the way ElectroDB's `_parseModel` does,
    /// returning an error wherever ElectroDB throws on the same model.
    pub fn new(def: Definition) -> Result<Model> {
        if def.entity.is_empty() || def.service.is_empty() || def.version.is_empty() {
            return Err(Error::invalid("model requires entity, service and version"));
        }
        let mut identifiers = default_identifiers();
        if !def.identifiers.entity.is_empty() {
            identifiers.entity = def.identifiers.entity.clone();
        }
        if !def.identifiers.version.is_empty() {
            identifiers.version = def.identifiers.version.clone();
        }
        let mut b = Builder {
            entity: def.entity,
            version: def.version,
            service: def.service,
            identifiers,
            attrs: Vec::new(),
            attr_index: HashMap::new(),
            indexes: Vec::new(),
            by_name: HashMap::new(),
            by_pattern: HashMap::new(),
            facet_order: Vec::new(),
            facets: HashMap::new(),
            written_casts: Vec::new(),
        };
        for ad in &def.attributes {
            if b.attr_index.contains_key(&ad.name) {
                return Err(Error::invalid(format!("duplicate attribute {:?}", ad.name)));
            }
            let mut a = new_attribute(ad, None, "")?;
            a.is_root = true;
            b.attr_index.insert(a.name.clone(), b.attrs.len());
            b.attrs.push(a);
        }
        check_attributes(&mut b.attrs)?;
        b.normalize_indexes(&def.indexes)?;
        b.check_composites()?;
        b.check_key_field_attributes()?;
        b.make_key_fixings()?;
        b.apply_labels();
        Ok(b.finish())
    }
}

struct Builder {
    entity: String,
    version: String,
    service: String,
    identifiers: Identifiers,
    attrs: Vec<Attribute>,
    attr_index: HashMap<String, usize>,
    indexes: Vec<Index>,
    by_name: HashMap<String, usize>,
    by_pattern: HashMap<String, usize>,
    facet_order: Vec<String>,
    facets: HashMap<String, Vec<Facet>>,
    /// Each index's written pk and sk casts, until make_key_fixings
    /// resolves them.
    written_casts: Vec<(String, String)>,
}

fn new_attribute(
    ad: &AttributeDef,
    parent_type: Option<AttrType>,
    parent_path: &str,
) -> Result<Attribute> {
    let mut a = Attribute {
        name: ad.name.clone(),
        field: if ad.field.is_empty() {
            ad.name.clone()
        } else {
            ad.field.clone()
        },
        ty: ad.ty,
        enum_values: ad.enum_values.clone(),
        label: ad.label.clone(),
        required: ad.required,
        hidden: ad.hidden,
        read_only: ad.read_only,
        padding: ad.padding.clone(),
        cast: ad.cast.clone(),
        default: ad.default.clone(),
        get: ad.get.clone(),
        set: ad.set.clone(),
        validate: ad.validate.clone(),
        validate_regex: ad.validate_regex.clone(),
        watch: ad.watch.clone(),
        watch_all: ad.watch_all,
        ..Default::default()
    };
    if a.watch_all {
        a.watch.clear();
    }
    if !a.cast.is_empty() && a.cast != "string" && a.cast != "number" {
        return Err(Error::invalid(format!(
            "attribute {:?}: invalid cast {:?}",
            a.name, a.cast
        )));
    }
    a.path = attribute_path(&a.name, a.ty, parent_type, parent_path);

    match a.ty {
        AttrType::List | AttrType::Set => {
            let Some(items) = &ad.items else {
                return Err(Error::invalid(format!(
                    "attribute {:?}: {} needs items",
                    a.path, a.ty
                )));
            };
            let mut items = (**items).clone();
            if items.name.is_empty() {
                items.name = "*".into();
            }
            if a.ty == AttrType::Set
                && !matches!(
                    items.ty,
                    AttrType::String | AttrType::Number | AttrType::Boolean | AttrType::Enum
                )
            {
                return Err(Error::invalid(format!(
                    "attribute {:?}: invalid set items {:?}",
                    a.path,
                    items.ty.as_str()
                )));
            }
            a.items = Some(Arc::new(new_attribute(&items, Some(a.ty), &a.path)?));
        }
        AttrType::Map => {
            let Some(props) = &ad.properties else {
                return Err(Error::invalid(format!(
                    "attribute {:?}: map needs properties",
                    a.path
                )));
            };
            let mut out = Vec::with_capacity(props.len());
            for pd in props {
                out.push(new_attribute(pd, Some(a.ty), &a.path)?);
            }
            check_attributes(&mut out)?;
            a.properties = out.into_iter().map(Arc::new).collect();
        }
        _ => {}
    }
    Ok(a)
}

/// Mirrors ElectroDB's `Attribute.buildPath`.
fn attribute_path(
    name: &str,
    ty: AttrType,
    parent_type: Option<AttrType>,
    parent_path: &str,
) -> String {
    if parent_path.is_empty() {
        return name.to_string();
    }
    if matches!(parent_type, Some(AttrType::List | AttrType::Set)) {
        return format!("{parent_path}[*]");
    }
    if ty == AttrType::Any {
        return format!("{parent_path}.*");
    }
    format!("{parent_path}.{name}")
}

/// Mirrors the checks `Schema.normalizeAttributes` makes on one level of
/// attributes: unique fields and valid watchers.
fn check_attributes(attrs: &mut [Attribute]) -> Result<()> {
    let mut used: HashMap<String, String> = HashMap::new();
    let mut by_name: HashMap<String, usize> = HashMap::new();
    for (i, a) in attrs.iter().enumerate() {
        if let Some(prev) = used.get(&a.field) {
            return Err(Error::invalid(format!(
                "attribute {:?}: field {:?} is already used by attribute {:?}",
                a.name, a.field, prev
            )));
        }
        if let Some(prev) = used.get(&a.name) {
            return Err(Error::invalid(format!(
                "attribute {:?}: name is already used as a field by attribute {:?}",
                a.name, prev
            )));
        }
        used.insert(a.field.clone(), a.name.clone());
        by_name.insert(a.name.clone(), i);
    }
    let mut watched_by: Vec<(usize, String)> = Vec::new();
    for a in attrs.iter() {
        for w in &a.watch {
            let Some(&wi) = by_name.get(w) else {
                return Err(Error::invalid(format!(
                    "attribute {:?} watches unknown attribute {:?}",
                    a.name, w
                )));
            };
            if attrs[wi].is_watcher() {
                return Err(Error::invalid(format!(
                    "attribute {:?} watches {:?}, which is itself a watcher",
                    a.name, w
                )));
            }
            watched_by.push((wi, a.name.clone()));
        }
    }
    for (wi, name) in watched_by {
        attrs[wi].watched_by.push(name);
    }
    Ok(())
}

fn key_type_name(kt: KeyType) -> &'static str {
    match kt {
        KeyType::Pk => "pk",
        KeyType::Sk => "sk",
    }
}

fn fmt_list(v: &[String]) -> String {
    format!("[{}]", v.join(" "))
}

impl Builder {
    fn attr(&self, name: &str) -> Option<&Attribute> {
        self.attr_index.get(name).map(|&i| &self.attrs[i])
    }

    fn attr_type(&self, name: &str) -> Option<AttrType> {
        self.attr(name).map(|a| a.ty)
    }

    fn is_attribute_field(&self, field: &str) -> bool {
        self.attrs.iter().any(|a| a.field == field)
    }

    /// Resolves a key's composites the way ElectroDB's
    /// `_applyCompositeToFacetConversion`, `_optimizeIndexKey` and
    /// `_parseTemplateAttributes` do, in that order.
    fn key_composites(&self, pattern: &str, kd: &KeyDef, kt: KeyType) -> Result<Key> {
        let mut k = Key {
            field: kd.field.clone(),
            template: kd.template.clone(),
            ..Default::default()
        };
        if kd.composite.is_none() && kd.template.is_none() {
            return Err(Error::invalid(format!(
                "access pattern {pattern:?}: {} has no composite or template",
                key_type_name(kt)
            )));
        }
        let mut template = kd.template.clone();
        if template.is_none()
            && let Some(c) = &kd.composite
            && c.len() == 1
            && c[0] == kd.field
            && self.is_attribute_field(&kd.field)
        {
            // _optimizeIndexKey: an attribute used directly as the key field.
            template = Some(format!("${{{}}}", kd.field));
        }
        if let Some(t) = &template {
            if !t.is_empty() {
                k.custom = true;
                k.labels = parse_template(t);
                k.composites = k
                    .labels
                    .iter()
                    .filter(|l| !l.name.is_empty())
                    .map(|l| l.name.clone())
                    .collect();
                if let (Some(_), Some(c)) = (&kd.template, &kd.composite)
                    && !string_array_match(c, &k.composites)
                {
                    return Err(Error::invalid(format!(
                        "access pattern {pattern:?}: {} template attributes {} don't match composite {}",
                        key_type_name(kt),
                        fmt_list(&k.composites),
                        fmt_list(c)
                    )));
                }
                return Ok(k);
            }
            // An empty template is treated like an empty composite.
            return Ok(k);
        }
        k.composites = kd.composite.clone().unwrap_or_default();
        k.labels = k
            .composites
            .iter()
            .map(|n| Label {
                name: n.clone(),
                label: String::new(),
            })
            .collect();
        Ok(k)
    }

    /// Mirrors ElectroDB's `_normalizeIndexes`.
    fn normalize_indexes(&mut self, defs: &[IndexDef]) -> Result<()> {
        let mut collections: HashMap<String, String> = HashMap::new();
        let mut by_field: HashMap<String, Vec<(usize, KeyType)>> = HashMap::new();
        let mut written_casts: Vec<(String, String)> = Vec::new();

        for d in defs {
            let pattern = d.access_pattern.clone();
            if self.by_pattern.contains_key(&pattern) {
                return Err(Error::invalid(format!(
                    "duplicate access pattern {pattern:?}"
                )));
            }
            let id = self.indexes.len();
            let has_sk = d.sk.is_some();
            let display = display_index(&d.index);

            if d.index.is_empty() && d.condition.is_some() {
                return Err(Error::invalid(format!(
                    "access pattern {pattern:?}: 'condition' is only allowed on secondary indexes"
                )));
            }
            if d.ty == IndexType::Composite {
                if d.index.is_empty() {
                    return Err(Error::invalid(format!(
                        "access pattern {pattern:?}: the table index can't be a composite index"
                    )));
                } else if d.condition.is_some() {
                    return Err(Error::invalid(format!(
                        "access pattern {pattern:?}: composite indexes don't support a condition"
                    )));
                } else if !d.scope.is_empty() {
                    return Err(Error::invalid(format!(
                        "access pattern {pattern:?}: composite indexes don't support scope"
                    )));
                } else if !d.pk.field.is_empty()
                    || d.sk.as_ref().is_some_and(|s| !s.field.is_empty())
                {
                    return Err(Error::invalid(format!(
                        "access pattern {pattern:?}: composite indexes don't take a field"
                    )));
                }
            } else if d.pk.field.is_empty() || d.sk.as_ref().is_some_and(|s| s.field.is_empty()) {
                return Err(Error::invalid(format!(
                    "access pattern {pattern:?}: pk and sk need a field"
                )));
            }
            if self.by_name.contains_key(&d.index) {
                return Err(Error::invalid(format!(
                    "access pattern {pattern:?}: duplicate index {display}"
                )));
            }
            if !has_sk && !d.collection.is_empty() {
                return Err(Error::invalid(format!(
                    "access pattern {pattern:?}: collections require an sk"
                )));
            }

            let mut pk = self.key_composites(&pattern, &d.pk, KeyType::Pk)?;
            pk.casing = resolve_casing(&d.pk.casing);
            pk.written_casing = d.pk.casing.clone();
            let sk = if let Some(skd) = &d.sk {
                let mut sk = self.key_composites(&pattern, skd, KeyType::Sk)?;
                sk.casing = resolve_casing(&skd.casing);
                sk.written_casing = skd.casing.clone();
                if sk.composites.len() > 1 {
                    for c in &pk.composites {
                        if sk.composites.contains(c) {
                            return Err(Error::invalid(format!(
                                "access pattern {pattern:?}: composite {c:?} is used in both pk and sk"
                            )));
                        }
                    }
                }
                sk
            } else {
                Key {
                    casing: Casing::Lower,
                    ..Default::default()
                }
            };
            written_casts.push((
                d.pk.cast.clone(),
                d.sk.as_ref().map(|s| s.cast.clone()).unwrap_or_default(),
            ));

            let projection = match &d.projection {
                Some(p) => {
                    let mut p = p.clone();
                    if p.attributes.is_empty() {
                        let kind = p.kind.to_lowercase();
                        if kind != "all" && kind != "keys_only" {
                            return Err(Error::invalid(format!(
                                "access pattern {pattern:?}: invalid projection {:?}",
                                p.kind
                            )));
                        }
                        p.kind = kind;
                    }
                    Some(p)
                }
                None => None,
            };

            let mut ix = Index {
                id,
                access_pattern: pattern.clone(),
                name: d.index.clone(),
                ty: d.ty,
                scope: d.scope.clone(),
                collections: Vec::new(),
                sub_collections: false,
                has_sk,
                pk,
                sk,
                condition: d.condition.clone(),
                projection,
            };
            if !d.collection.is_empty() {
                ix.collections = d.collection.clone();
                ix.sub_collections = d.collection_array;
                for c in &ix.collections {
                    if c.is_empty() {
                        return Err(Error::invalid(format!(
                            "access pattern {pattern:?}: invalid collection"
                        )));
                    }
                    if let Some(prev) = collections.get(c) {
                        return Err(Error::invalid(format!(
                            "collection {c:?} is defined on both {prev:?} and {pattern:?}"
                        )));
                    }
                    collections.insert(c.clone(), pattern.clone());
                }
            }

            if ix.ty != IndexType::Composite {
                let mut kts = vec![KeyType::Pk];
                if ix.has_sk {
                    if ix.sk.field == ix.pk.field {
                        return Err(Error::invalid(format!(
                            "access pattern {pattern:?} uses field {:?} for both pk and sk",
                            ix.pk.field
                        )));
                    }
                    kts.push(KeyType::Sk);
                }
                for kt in kts {
                    let k = ix.key(kt);
                    if let Some(uses) = by_field.get(&k.field) {
                        for &(uid, ukt) in uses {
                            if uid == id {
                                continue;
                            }
                            let other = &self.indexes[uid];
                            let uk = other.key(ukt);
                            if !string_array_match(&k.composites, &uk.composites) {
                                return Err(Error::invalid(format!(
                                    "access pattern {pattern:?}: field {:?} is defined with composites {}, but {:?} uses {}",
                                    k.field,
                                    fmt_list(&k.composites),
                                    other.access_pattern,
                                    fmt_list(&uk.composites)
                                )));
                            }
                            if k.template != uk.template {
                                return Err(Error::invalid(format!(
                                    "access pattern {pattern:?}: field {:?} has a different template than on {:?}",
                                    k.field, other.access_pattern
                                )));
                            }
                            break;
                        }
                    }
                    by_field.entry(k.field.clone()).or_default().push((id, kt));
                }
            }

            for kt in [KeyType::Pk, KeyType::Sk] {
                for name in &ix.key(kt).composites {
                    if !self.facets.contains_key(name) {
                        self.facet_order.push(name.clone());
                    }
                    self.facets
                        .entry(name.clone())
                        .or_default()
                        .push(Facet { index: id, kt });
                }
            }

            self.by_name.insert(ix.name.clone(), id);
            self.by_pattern.insert(pattern, id);
            self.indexes.push(ix);
        }

        if !self.by_name.contains_key("") {
            return Err(Error::invalid(
                "model is missing the table index (an access pattern without an index name)",
            ));
        }
        self.written_casts = written_casts;
        Ok(())
    }
}

impl Builder {
    /// Mirrors Schema checks on key composites: each must be a defined
    /// string, number, boolean or enum attribute. Table-index composites
    /// become readOnly, and projections must name known attributes.
    fn check_composites(&mut self) -> Result<()> {
        for ix in &self.indexes {
            for name in ix.composites() {
                let Some(&ai) = self.attr_index.get(&name) else {
                    return Err(Error::invalid(format!(
                        "access pattern {:?}: composite {name:?} is not an attribute",
                        ix.access_pattern
                    )));
                };
                let a = &mut self.attrs[ai];
                if !matches!(
                    a.ty,
                    AttrType::String | AttrType::Number | AttrType::Boolean | AttrType::Enum
                ) {
                    return Err(Error::invalid(format!(
                        "attribute {name:?} is type {}, but composites must be string, number, boolean or enum",
                        a.ty
                    )));
                }
                if ix.name.is_empty() {
                    a.read_only = true;
                }
            }
            if let Some(p) = &ix.projection {
                for name in &p.attributes {
                    if !self.attr_index.contains_key(name)
                        && *name != self.identifiers.entity
                        && *name != self.identifiers.version
                    {
                        return Err(Error::invalid(format!(
                            "access pattern {:?}: unknown projection attribute {name:?}",
                            ix.access_pattern
                        )));
                    }
                }
            }
        }
        Ok(())
    }

    /// Mirrors the checks ElectroDB's Schema makes on attributes whose field
    /// is also a key field.
    fn check_key_field_attributes(&mut self) -> Result<()> {
        for a in &mut self.attrs {
            for ix in &mut self.indexes {
                if ix.ty == IndexType::Composite {
                    continue;
                }
                let display = display_index(&ix.name);
                let collections = !ix.collections.is_empty();
                for k in [&mut ix.pk, &mut ix.sk] {
                    if k.field.is_empty() || k.field != a.field {
                        continue;
                    }
                    k.field_ref = true;
                    if k.composites.len() > 1 {
                        return Err(Error::invalid(format!(
                            "index {display}: key field {:?} is an attribute's field, so it can't have other composites",
                            k.field
                        )));
                    }
                    if !k.custom {
                        return Err(Error::invalid(format!(
                            "index {display}: key field {:?} is an attribute's field, so it needs a template",
                            k.field
                        )));
                    }
                    if k.labels.len() > 2 {
                        return Err(Error::invalid(format!(
                            "index {display}: key field {:?} is an attribute's field, so its template can have at most a prefix and a postfix",
                            k.field
                        )));
                    }
                    let has_fixings = k.labels.iter().any(|l| !l.label.is_empty());
                    for l in &k.labels {
                        if l.name == a.field {
                            a.key_prefix = l.label.clone();
                        } else {
                            a.key_postfix = l.label.clone();
                        }
                    }
                    a.key_casing = match k.written_casing.as_str() {
                        "lower" => Some(Casing::Lower),
                        "upper" => Some(Casing::Upper),
                        _ => None,
                    };
                    if has_fixings && a.ty != AttrType::String && a.ty != AttrType::Enum {
                        return Err(Error::invalid(format!(
                            "index {display}: key field {:?} has a prefix or postfix, so attribute {:?} must be a string or enum",
                            k.field, a.name
                        )));
                    }
                    if collections {
                        return Err(Error::invalid(format!(
                            "index {display}: key field {:?} is an attribute's field, so the index can't be in a collection",
                            k.field
                        )));
                    }
                    if a.padding.is_some() {
                        return Err(Error::invalid(format!(
                            "attribute {:?}: padding isn't supported on attributes used as key fields",
                            a.name
                        )));
                    }
                }
            }
        }
        Ok(())
    }

    /// Mirrors ElectroDB's `_normalizeKeyFixings`/`_makeKeyFixings`.
    fn make_key_fixings(&mut self) -> Result<()> {
        for i in 0..self.indexes.len() {
            let mut prev_pk: Option<(usize, KeyType)> = None;
            let mut prev_sk: Option<(usize, KeyType)> = None;
            {
                let ix = &self.indexes[i];
                for p in 0..i {
                    let pi = &self.indexes[p];
                    if pi.pk.field == ix.pk.field {
                        prev_pk = Some((p, KeyType::Pk));
                    } else if pi.has_sk && pi.sk.field == ix.pk.field {
                        prev_pk = Some((p, KeyType::Sk));
                    }
                    if ix.has_sk {
                        if pi.pk.field == ix.sk.field {
                            prev_sk = Some((p, KeyType::Pk));
                        } else if pi.has_sk && pi.sk.field == ix.sk.field {
                            prev_sk = Some((p, KeyType::Sk));
                        }
                    }
                    if prev_pk.is_some() && (prev_sk.is_some() || !ix.has_sk) {
                        break;
                    }
                }
            }

            let (entity, version, service) = (
                self.entity.clone(),
                self.version.clone(),
                self.service.clone(),
            );
            let (pk_cast, sk_cast) = self.written_casts[i].clone();
            let pk_type = {
                let ix = &self.indexes[i];
                let mut sk = make_collection_prefix(&ix.collections);
                let mut entity_keys = if sk.is_empty() {
                    format!("${entity}")
                } else {
                    format!("#{entity}")
                };
                entity_keys.push('_');
                entity_keys.push_str(&version);

                let mut pk = format!("${service}");
                let mut postfix = String::new();
                let clustered = ix.ty == IndexType::Clustered;
                if clustered {
                    postfix = entity_keys;
                } else {
                    sk.push_str(&entity_keys);
                }
                if !ix.has_sk {
                    pk.push_str(&sk);
                    if clustered {
                        pk.push_str(&postfix);
                    }
                }
                (pk, sk, postfix)
            };
            let pk_cast = self.resolve_cast(i, KeyType::Pk, &pk_cast)?;
            let sk_cast = if self.indexes[i].has_sk {
                Some(self.resolve_cast(i, KeyType::Sk, &sk_cast)?)
            } else {
                None
            };

            let ix = &mut self.indexes[i];
            let (mut pk, sk, postfix) = pk_type;
            if !ix.pk.custom {
                if !ix.scope.is_empty() {
                    pk.push('_');
                    pk.push_str(&ix.scope);
                }
                ix.pk.prefix = format_casing(&pk, ix.pk.casing);
            }
            if !ix.sk.custom {
                ix.sk.prefix = format_casing(&sk, ix.sk.casing);
                ix.sk.postfix = format_casing(&postfix, ix.sk.casing);
                ix.sk.has_postfix = true;
            }
            ix.pk.cast = pk_cast;
            if let Some(c) = sk_cast {
                ix.sk.cast = c;
            }

            if let Some((p, kt)) = prev_pk {
                let src = self.indexes[p].key(kt).clone();
                let ix = &mut self.indexes[i];
                if src.casing != ix.pk.casing {
                    return Err(Error::invalid(format!(
                        "access pattern {:?}: pk field {:?} has a different casing than on another index",
                        ix.access_pattern, ix.pk.field
                    )));
                }
                copy_fixings(&mut ix.pk, &src);
            }
            if let Some((p, kt)) = prev_sk {
                let src = self.indexes[p].key(kt).clone();
                let ix = &mut self.indexes[i];
                if src.casing != ix.sk.casing {
                    return Err(Error::invalid(format!(
                        "access pattern {:?}: sk field {:?} has a different casing than on another index",
                        ix.access_pattern, ix.sk.field
                    )));
                }
                copy_fixings(&mut ix.sk, &src);
            }
        }
        Ok(())
    }

    /// Mirrors the cast rules in `_makeKeyFixings`.
    fn resolve_cast(&self, i: usize, kt: KeyType, written: &str) -> Result<Cast> {
        let ix = &self.indexes[i];
        let k = ix.key(kt);
        let first = k.composites.first().and_then(|c| self.attr_type(c));
        if written == "string" {
            return Ok(Cast::String);
        }
        if written.is_empty()
            && k.custom
            && k.composites.len() == 1
            && k.labels.iter().all(|l| l.label.is_empty())
            && first == Some(AttrType::Number)
        {
            // Custom keys with one numeric composite and no labels are numeric.
            return Ok(Cast::Number);
        }
        if written == "number"
            && k.composites.len() == 1
            && matches!(
                first,
                Some(AttrType::Number | AttrType::String | AttrType::Boolean)
            )
        {
            return Ok(Cast::Number);
        }
        if written == "number" && k.composites.len() > 1 {
            return Err(Error::invalid(format!(
                "index {}: keys can only be cast to number if they are a composite of one numeric attribute",
                display_index(&ix.name)
            )));
        }
        Ok(Cast::String)
    }

    /// Mirrors ElectroDB's `_mergeKeyDefinitions`: standard keys take each
    /// attribute's label, falling back to its name. The pk treats an empty
    /// label as missing; the sk keeps it.
    fn apply_labels(&mut self) {
        for i in 0..self.indexes.len() {
            for kt in [KeyType::Pk, KeyType::Sk] {
                let labels: Vec<String> = {
                    let k = self.indexes[i].key(kt);
                    if k.custom {
                        continue;
                    }
                    k.labels
                        .iter()
                        .map(|l| {
                            let mut label = l.name.clone();
                            if let Some(Some(al)) = self.attr(&l.name).map(|a| a.label.as_ref())
                                && (kt == KeyType::Sk || !al.is_empty())
                            {
                                label = al.clone();
                            }
                            label
                        })
                        .collect()
                };
                let ix = &mut self.indexes[i];
                let k = if kt == KeyType::Pk {
                    &mut ix.pk
                } else {
                    &mut ix.sk
                };
                for (l, label) in k.labels.iter_mut().zip(labels) {
                    l.label = label;
                }
            }
        }
    }

    fn finish(self) -> Model {
        let attributes: Vec<Arc<Attribute>> = self.attrs.into_iter().map(Arc::new).collect();
        let attrs = attributes
            .iter()
            .map(|a| (a.name.clone(), a.clone()))
            .collect();
        let mut m = Model {
            entity: self.entity,
            version: self.version,
            service: self.service,
            identifiers: self.identifiers,
            attributes,
            indexes: self.indexes,
            attrs,
            by_path: HashMap::new(),
            by_name: self.by_name,
            by_pattern: self.by_pattern,
            facet_order: self.facet_order,
            facets: self.facets,
        };
        m.index_paths();
        m
    }
}

fn copy_fixings(dst: &mut Key, src: &Key) {
    dst.custom = src.custom;
    dst.casing = src.casing;
    dst.cast = src.cast;
    dst.prefix = src.prefix.clone();
    dst.postfix = src.postfix.clone();
    dst.has_postfix = src.has_postfix;
}

/// Mirrors ElectroDB's `_parseTemplateKey`.
pub(crate) fn parse_template(template: &str) -> Vec<Label> {
    let t = template.as_bytes();
    let mut out = Vec::new();
    let mut label: Vec<u8> = Vec::new();
    let mut name: Vec<u8> = Vec::new();
    let mut in_name = false;
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    for i in 0..t.len() {
        let c = t[i];
        let last = if i > 0 { t[i - 1] } else { 0 };
        let next = if i + 1 < t.len() { t[i + 1] } else { 0 };
        if c == b'{' && last == b'$' && !in_name {
            in_name = true;
        } else if c == b'}' && in_name {
            out.push(Label {
                name: text(&name),
                label: text(&label),
            });
            name.clear();
            label.clear();
            in_name = false;
        } else if c == b'$' && next == b'{' && !in_name {
            continue;
        } else if in_name {
            name.push(c);
        } else {
            label.push(c);
        }
    }
    if !name.is_empty() || !label.is_empty() {
        out.push(Label {
            name: text(&name),
            label: text(&label),
        });
    }
    out
}

fn make_collection_prefix(collections: &[String]) -> String {
    let mut prefix = String::new();
    for (i, c) in collections.iter().enumerate() {
        prefix.push(if i == 0 { '$' } else { '#' });
        prefix.push_str(c);
    }
    prefix
}

fn resolve_casing(c: &str) -> Casing {
    match c {
        "upper" => Casing::Upper,
        "none" => Casing::None,
        _ => Casing::Lower,
    }
}

/// Applies a key casing. Casing is ASCII-only (decision D6).
pub fn format_casing(s: &str, c: Casing) -> String {
    match c {
        Casing::Upper => s.to_ascii_uppercase(),
        Casing::None => s.to_string(),
        Casing::Lower => s.to_ascii_lowercase(),
    }
}

fn string_array_match(a: &[String], b: &[String]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| !x.is_empty() && x == y)
}

pub(crate) fn display_index(name: &str) -> String {
    if name.is_empty() {
        "(Primary Index)".into()
    } else {
        format!("{name:?}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn label(name: &str, label: &str) -> Label {
        Label {
            name: name.into(),
            label: label.into(),
        }
    }

    #[test]
    fn parses_templates() {
        let cases = vec![
            ("user#${userId}", vec![label("userId", "user#")]),
            ("${a}#${b}", vec![label("a", ""), label("b", "#")]),
            ("a${x}b", vec![label("x", "a"), label("", "b")]),
            ("$x${y}", vec![label("y", "$x")]),
            ("${open", vec![label("open", "")]),
        ];
        for (input, want) in cases {
            assert_eq!(parse_template(input), want, "parse_template({input:?})");
        }
    }

    #[test]
    fn casing_is_ascii() {
        assert_eq!(format_casing("AbC-ÄÖ", Casing::Lower), "abc-ÄÖ");
        assert_eq!(format_casing("abc-äö", Casing::Upper), "ABC-äö");
        assert_eq!(format_casing("AbC", Casing::None), "AbC");
    }

    fn strs(names: &[&str]) -> Vec<AttributeDef> {
        names
            .iter()
            .map(|n| AttributeDef {
                name: n.to_string(),
                ..Default::default()
            })
            .collect()
    }

    fn key(field: &str, composite: &[&str]) -> KeyDef {
        KeyDef {
            field: field.into(),
            composite: Some(composite.iter().map(|s| s.to_string()).collect()),
            ..Default::default()
        }
    }

    fn base(indexes: Vec<IndexDef>) -> Definition {
        Definition {
            entity: "e".into(),
            version: "1".into(),
            service: "s".into(),
            attributes: strs(&["a", "b"]),
            indexes,
            ..Default::default()
        }
    }

    fn ix(pattern: &str, index: &str, pk: KeyDef, sk: Option<KeyDef>) -> IndexDef {
        IndexDef {
            access_pattern: pattern.into(),
            index: index.into(),
            pk,
            sk,
            ..Default::default()
        }
    }

    fn with_attrs(attributes: Vec<AttributeDef>, indexes: Vec<IndexDef>) -> Definition {
        Definition {
            attributes,
            ..base(indexes)
        }
    }

    #[test]
    fn rejects_what_electrodb_rejects() {
        let primary = ix("p", "", key("pk", &["a"]), Some(key("sk", &[])));
        let cond: ConditionFn = Arc::new(|_| Ok(true));
        let cases: Vec<(&str, Definition, &str)> = vec![
            (
                "missing table index",
                base(vec![ix("g", "gsi1", key("g", &["a"]), None)]),
                "missing the table index",
            ),
            (
                "collection without sk",
                base(vec![
                    primary.clone(),
                    IndexDef {
                        collection: vec!["c".into()],
                        ..ix("g", "gsi1", key("g", &["a"]), None)
                    },
                ]),
                "collections require an sk",
            ),
            (
                "cast number with two composites",
                base(vec![ix(
                    "p",
                    "",
                    KeyDef {
                        cast: "number".into(),
                        ..key("pk", &["a", "b"])
                    },
                    None,
                )]),
                "cast to number",
            ),
            (
                "condition on table index",
                base(vec![IndexDef {
                    condition: Some(cond),
                    ..ix("p", "", key("pk", &["a"]), None)
                }]),
                "only allowed on secondary",
            ),
            (
                "same field for pk and sk",
                base(vec![ix("p", "", key("k", &["a"]), Some(key("k", &["b"])))]),
                "both pk and sk",
            ),
            (
                "attribute key field with two composites",
                base(vec![ix("p", "", key("a", &["a", "b"]), None)]),
                "can't have other composites",
            ),
            (
                "unknown composite",
                base(vec![ix("p", "", key("pk", &["zz"]), None)]),
                "not an attribute",
            ),
            (
                "map composite",
                with_attrs(
                    vec![AttributeDef {
                        name: "m".into(),
                        ty: AttrType::Map,
                        properties: Some(vec![]),
                        ..Default::default()
                    }],
                    vec![ix("p", "", key("pk", &["m"]), None)],
                ),
                "composites must be",
            ),
            (
                "duplicate field",
                with_attrs(
                    vec![
                        AttributeDef {
                            name: "a".into(),
                            field: "f".into(),
                            ..Default::default()
                        },
                        AttributeDef {
                            name: "b".into(),
                            field: "f".into(),
                            ..Default::default()
                        },
                    ],
                    vec![ix("p", "", key("pk", &["a"]), None)],
                ),
                "already used",
            ),
            (
                "watching a watcher",
                with_attrs(
                    vec![
                        AttributeDef {
                            name: "a".into(),
                            ..Default::default()
                        },
                        AttributeDef {
                            name: "b".into(),
                            watch: vec!["a".into()],
                            ..Default::default()
                        },
                        AttributeDef {
                            name: "c".into(),
                            watch: vec!["b".into()],
                            ..Default::default()
                        },
                    ],
                    vec![ix("p", "", key("pk", &["a"]), None)],
                ),
                "itself a watcher",
            ),
            (
                "watching an unknown attribute",
                with_attrs(
                    vec![AttributeDef {
                        name: "a".into(),
                        watch: vec!["zz".into()],
                        ..Default::default()
                    }],
                    vec![ix("p", "", key("pk", &["a"]), None)],
                ),
                "unknown attribute",
            ),
            (
                "template mismatch",
                base(vec![ix(
                    "p",
                    "",
                    KeyDef {
                        template: Some("x#${b}".into()),
                        ..key("pk", &["a"])
                    },
                    None,
                )]),
                "don't match",
            ),
        ];
        for (name, def, want) in cases {
            match Model::new(def) {
                Ok(_) => panic!("{name}: accepted, want an error containing {want:?}"),
                Err(e) => assert!(
                    e.to_string().contains(want),
                    "{name}: err = {e}, want {want:?}"
                ),
            }
        }
    }
}
