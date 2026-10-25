//! The normalized model: the equivalent of ElectroDB's parsed model.

mod def;
mod new;

use std::collections::HashMap;
use std::sync::{Arc, LazyLock};

use regex::Regex;

pub use def::*;
pub(crate) use new::display_index;
pub use new::format_casing;

/// A resolved key casing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Casing {
    #[default]
    Lower,
    Upper,
    None,
}

/// How a key value is stored.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Cast {
    #[default]
    String,
    Number,
}

/// Partition or sort key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum KeyType {
    Pk,
    Sk,
}

/// ElectroDB's identifier field names.
pub fn default_identifiers() -> Identifiers {
    Identifiers {
        entity: "__edb_e__".into(),
        version: "__edb_v__".into(),
    }
}

/// A normalized entity.
#[derive(Clone)]
pub struct Model {
    pub entity: String,
    pub version: String,
    pub service: String,
    pub identifiers: Identifiers,
    /// The root attributes in definition order.
    pub attributes: Vec<Arc<Attribute>>,
    /// The access patterns in definition order. An index's `id` is its
    /// position here.
    pub indexes: Vec<Index>,

    attrs: HashMap<String, Arc<Attribute>>,
    by_path: HashMap<String, Arc<Attribute>>,
    by_name: HashMap<String, usize>,
    by_pattern: HashMap<String, usize>,
    /// Composite attribute names in the order ElectroDB first sees them
    /// (index order, pk before sk), and each one's uses.
    facet_order: Vec<String>,
    facets: HashMap<String, Vec<Facet>>,
}

/// A normalized attribute.
#[derive(Clone, Default)]
pub struct Attribute {
    pub name: String,
    pub field: String,
    pub ty: AttrType,
    pub enum_values: Vec<String>,
    pub label: Option<String>,
    pub required: bool,
    pub hidden: bool,
    /// True when written readOnly or when the attribute is a table-index
    /// composite.
    pub read_only: bool,
    pub padding: Option<Padding>,
    pub cast: String,
    /// ElectroDB's attribute path, e.g. `steps[*].name`.
    pub path: String,

    pub default: Option<DefaultFn>,
    pub get: Option<TransformFn>,
    pub set: Option<TransformFn>,
    pub validate: Option<ValidateFn>,
    pub validate_regex: Option<Regex>,

    pub watch: Vec<String>,
    pub watch_all: bool,
    /// The sibling attributes watching this one.
    pub watched_by: Vec<String>,

    /// The template text around this attribute when its field is also a key
    /// field; reads strip them.
    pub key_prefix: String,
    pub key_postfix: String,
    /// The key's explicitly written casing (lower/upper), which ElectroDB
    /// also applies to values compared with this attribute.
    pub key_casing: Option<Casing>,

    pub items: Option<Arc<Attribute>>,
    pub properties: Vec<Arc<Attribute>>,
    pub is_root: bool,
}

impl Attribute {
    /// Reports whether the attribute watches specific attributes.
    pub fn is_watcher(&self) -> bool {
        !self.watch.is_empty()
    }

    /// The map property with the given name.
    pub fn property(&self, name: &str) -> Option<&Arc<Attribute>> {
        self.properties.iter().find(|p| p.name == name)
    }
}

/// A normalized access pattern.
#[derive(Clone)]
pub struct Index {
    /// The index's position in `Model::indexes`.
    pub id: usize,
    pub access_pattern: String,
    /// The DynamoDB index name; empty for the table index.
    pub name: String,
    pub ty: IndexType,
    pub scope: String,
    pub collections: Vec<String>,
    /// True when the collections were written as an array.
    pub sub_collections: bool,
    pub has_sk: bool,
    pub pk: Key,
    pub sk: Key,
    /// The sparse-index callback.
    pub condition: Option<ConditionFn>,
    pub projection: Option<Projection>,
}

impl Index {
    /// All of the index's composites, pk first.
    pub fn composites(&self) -> Vec<String> {
        let mut out = self.pk.composites.clone();
        out.extend(self.sk.composites.iter().cloned());
        out
    }

    pub fn key(&self, kt: KeyType) -> &Key {
        match kt {
            KeyType::Pk => &self.pk,
            KeyType::Sk => &self.sk,
        }
    }

    pub fn is_table(&self) -> bool {
        self.name.is_empty()
    }
}

/// A normalized pk or sk.
#[derive(Clone, Debug, Default)]
pub struct Key {
    pub field: String,
    /// The key's attribute names, in order.
    pub composites: Vec<String>,
    /// The composite segments: for standard keys, one per composite with its
    /// label; for templates, the literal text before each attribute (and a
    /// trailing entry with an empty name for text after the last one).
    pub labels: Vec<Label>,
    pub template: Option<String>,
    /// True when the key field is also an attribute's field.
    pub field_ref: bool,
    /// The casing as written in the schema (empty if unset).
    pub written_casing: String,

    // The fixings below come from ElectroDB's _makeKeyFixings. When another
    // index already uses the same field, they are copied from that index.
    pub custom: bool,
    pub casing: Casing,
    pub cast: Cast,
    pub prefix: String,
    pub postfix: String,
    pub has_postfix: bool,
}

/// One composite segment of a key.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Label {
    pub name: String,
    pub label: String,
}

/// One use of an attribute as a key composite.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Facet {
    pub index: usize,
    pub kt: KeyType,
}

static GENERIC_PATH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[\d+\]").unwrap());

impl Model {
    /// The root attribute with the given name.
    pub fn attribute(&self, name: &str) -> Option<&Arc<Attribute>> {
        self.attrs.get(name)
    }

    /// The index with the given DynamoDB name (`""` for the table).
    pub fn index(&self, name: &str) -> Option<&Index> {
        self.by_name.get(name).map(|&i| &self.indexes[i])
    }

    /// The index for the given access pattern.
    pub fn access_pattern(&self, name: &str) -> Option<&Index> {
        self.by_pattern.get(name).map(|&i| &self.indexes[i])
    }

    /// The table's own index.
    pub fn table_index(&self) -> &Index {
        &self.indexes[self.by_name[""]]
    }

    /// The names of attributes used as key composites, in the order
    /// ElectroDB iterates them.
    pub fn facet_attributes(&self) -> &[String] {
        &self.facet_order
    }

    /// Every key use of the named attribute.
    pub fn facets(&self, name: &str) -> &[Facet] {
        self.facets.get(name).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Finds an attribute by its ElectroDB path, with list indexes
    /// generalized (`steps[0].name` finds `steps[*].name`), as ElectroDB's
    /// attribute traverser does.
    pub fn attribute_at_path(&self, path: &str) -> Option<&Arc<Attribute>> {
        self.by_path
            .get(GENERIC_PATH.replace_all(path, "[*]").as_ref())
    }

    /// Finds an attribute by its exact ElectroDB path, without generalizing
    /// list indexes (ElectroDB's checkUpdate lookup).
    pub fn attribute_at_exact_path(&self, path: &str) -> Option<&Arc<Attribute>> {
        self.by_path.get(path)
    }

    fn index_paths(&mut self) {
        fn walk(a: &Arc<Attribute>, out: &mut HashMap<String, Arc<Attribute>>) {
            out.insert(a.path.clone(), a.clone());
            if let Some(items) = &a.items {
                walk(items, out);
            }
            for p in &a.properties {
                walk(p, out);
            }
        }
        let mut by_path = HashMap::new();
        for a in &self.attributes {
            walk(a, &mut by_path);
        }
        self.by_path = by_path;
    }

    /// A copy of the first model whose attributes are the union of all the
    /// models' (the first definition of a name wins), as a collection's
    /// filters see them.
    pub fn merged(models: &[&Model]) -> Model {
        let mut out = models[0].clone();
        out.attrs = HashMap::new();
        out.attributes = Vec::new();
        for m in models {
            for a in &m.attributes {
                if !out.attrs.contains_key(&a.name) {
                    out.attrs.insert(a.name.clone(), a.clone());
                    out.attributes.push(a.clone());
                }
            }
        }
        out.index_paths();
        out
    }

    /// The index defining the named collection, and the collection's
    /// sub-collection path (the collection names up to it).
    pub fn collection_index(&self, name: &str) -> Option<(&Index, Vec<String>)> {
        for ix in &self.indexes {
            for (i, c) in ix.collections.iter().enumerate() {
                if c != name {
                    continue;
                }
                if !ix.sub_collections {
                    return Some((ix, vec![c.clone()]));
                }
                return Some((ix, ix.collections[..=i].to_vec()));
            }
        }
        None
    }
}
