//! Parsing `#[wattdb(...)]` attributes.

use proc_macro2::Span;
use syn::ext::IdentExt;
use syn::meta::ParseNestedMeta;
use syn::{Attribute, Expr, Ident, LitInt, LitStr, Result, Token, Type};

fn lit_str(meta: &ParseNestedMeta) -> Result<LitStr> {
    meta.value()?.parse()
}

fn string(meta: &ParseNestedMeta) -> Result<String> {
    Ok(lit_str(meta)?.value())
}

fn expr(meta: &ParseNestedMeta) -> Result<Expr> {
    meta.value()?.parse()
}

/// A parenthesized identifier list, possibly empty: `(a, b)` or `()`.
fn idents(meta: &ParseNestedMeta) -> Result<Vec<Ident>> {
    let content;
    syn::parenthesized!(content in meta.input);
    let list = content.parse_terminated(Ident::parse_any, Token![,])?;
    Ok(list.into_iter().collect())
}

fn strings(meta: &ParseNestedMeta) -> Result<Vec<String>> {
    let content;
    syn::parenthesized!(content in meta.input);
    let list = content.parse_terminated(|p| p.parse::<LitStr>(), Token![,])?;
    Ok(list.iter().map(LitStr::value).collect())
}

fn key_name(meta: &ParseNestedMeta) -> String {
    meta.path
        .get_ident()
        .map(|i| i.to_string())
        .unwrap_or_default()
}

/// A pk or sk.
#[derive(Default)]
pub struct KeyOpts {
    pub field: String,
    pub composite: Option<Vec<Ident>>,
    pub template: Option<LitStr>,
    pub casing: String,
    pub cast: String,
}

fn parse_key(meta: &ParseNestedMeta) -> Result<KeyOpts> {
    let mut k = KeyOpts::default();
    if meta.input.is_empty() || meta.input.peek(Token![,]) {
        return Ok(k);
    }
    meta.parse_nested_meta(|m| {
        match key_name(&m).as_str() {
            "field" => k.field = string(&m)?,
            "composite" => k.composite = Some(idents(&m)?),
            "template" => k.template = Some(lit_str(&m)?),
            "casing" => k.casing = string(&m)?,
            "cast" => k.cast = string(&m)?,
            other => return Err(m.error(format!("unknown key option `{other}`"))),
        }
        Ok(())
    })?;
    Ok(k)
}

pub enum ProjectionOpts {
    Kind(String),
    Include(Vec<Ident>),
}

/// One `index(...)`.
pub struct IndexOpts {
    pub span: Span,
    pub access_pattern: String,
    pub index: String,
    pub ty: String,
    pub collections: Vec<String>,
    pub sub_collections: bool,
    pub scope: String,
    pub condition: Option<Expr>,
    pub pk: KeyOpts,
    pub sk: Option<KeyOpts>,
    pub projection: Option<ProjectionOpts>,
}

fn parse_index(meta: &ParseNestedMeta) -> Result<IndexOpts> {
    let mut ix = IndexOpts {
        span: meta
            .path
            .get_ident()
            .map(|i| i.span())
            .unwrap_or_else(Span::call_site),
        access_pattern: String::new(),
        index: String::new(),
        ty: String::new(),
        collections: Vec::new(),
        sub_collections: false,
        scope: String::new(),
        condition: None,
        pk: KeyOpts::default(),
        sk: None,
        projection: None,
    };
    let mut has_pk = false;
    meta.parse_nested_meta(|m| {
        match key_name(&m).as_str() {
            "access_pattern" => ix.access_pattern = string(&m)?,
            "index" => ix.index = string(&m)?,
            "type" => ix.ty = string(&m)?,
            "clustered" => ix.ty = "clustered".into(),
            "composite" => ix.ty = "composite".into(),
            "isolated" => ix.ty = "isolated".into(),
            "collection" => {
                ix.collections = vec![string(&m)?];
                ix.sub_collections = false;
            }
            "sub_collections" => {
                ix.collections = strings(&m)?;
                ix.sub_collections = true;
            }
            "scope" => ix.scope = string(&m)?,
            "condition" => ix.condition = Some(expr(&m)?),
            "pk" => {
                ix.pk = parse_key(&m)?;
                has_pk = true;
            }
            "sk" => ix.sk = Some(parse_key(&m)?),
            "projection" => {
                if m.input.peek(Token![=]) {
                    ix.projection = Some(ProjectionOpts::Kind(string(&m)?));
                } else {
                    ix.projection = Some(ProjectionOpts::Include(idents(&m)?));
                }
            }
            other => return Err(m.error(format!("unknown index option `{other}`"))),
        }
        Ok(())
    })?;
    if ix.access_pattern.is_empty() {
        return Err(meta.error("index needs an access_pattern"));
    }
    if !has_pk {
        return Err(meta.error(format!(
            "access pattern {:?} needs a pk(...)",
            ix.access_pattern
        )));
    }
    Ok(ix)
}

/// The container options of an entity or map.
#[derive(Default)]
pub struct ContainerOpts {
    pub entity: Option<LitStr>,
    pub version: Option<LitStr>,
    pub service: Option<Expr>,
    pub rename_all: Option<String>,
    pub indexes: Vec<IndexOpts>,
    pub serde: bool,
    pub collection: Option<LitStr>,
}

pub fn container(attrs: &[Attribute]) -> Result<ContainerOpts> {
    let mut c = ContainerOpts::default();
    for a in attrs.iter().filter(|a| a.path().is_ident("wattdb")) {
        a.parse_nested_meta(|m| {
            match key_name(&m).as_str() {
                "entity" => c.entity = Some(lit_str(&m)?),
                "version" => c.version = Some(lit_str(&m)?),
                "service" => c.service = Some(expr(&m)?),
                "rename_all" => c.rename_all = Some(string(&m)?),
                "index" => c.indexes.push(parse_index(&m)?),
                "serde" => c.serde = true,
                "collection" => c.collection = Some(lit_str(&m)?),
                other => return Err(m.error(format!("unknown wattdb option `{other}`"))),
            }
            Ok(())
        })?;
    }
    Ok(c)
}

/// The options of one field.
#[derive(Default)]
pub struct FieldOpts {
    pub field: Option<String>,
    pub required: bool,
    pub default: Option<Expr>,
    pub label: Option<String>,
    pub hidden: bool,
    pub read_only: bool,
    pub padding: Option<(LitInt, String)>,
    pub cast: Option<String>,
    pub validate: Option<Expr>,
    pub validate_regex: Option<LitStr>,
    pub get: Option<Expr>,
    pub set: Option<Expr>,
    pub watch: Vec<Ident>,
    pub watch_all: bool,
    pub codec: Option<Type>,
    pub as_set: bool,
    pub items_validate: Option<Expr>,
    pub items_validate_regex: Option<LitStr>,
    pub json: Option<String>,
    pub descriptor: Option<Ident>,
}

pub fn field(attrs: &[Attribute]) -> Result<FieldOpts> {
    let mut f = FieldOpts::default();
    for a in attrs.iter().filter(|a| a.path().is_ident("wattdb")) {
        a.parse_nested_meta(|m| {
            match key_name(&m).as_str() {
                "field" => f.field = Some(string(&m)?),
                "required" => f.required = true,
                "default" => f.default = Some(expr(&m)?),
                "label" => f.label = Some(string(&m)?),
                "hidden" => f.hidden = true,
                "read_only" => f.read_only = true,
                "padding" => {
                    let (mut length, mut ch) = (None, None);
                    m.parse_nested_meta(|p| {
                        match key_name(&p).as_str() {
                            "length" => length = Some(p.value()?.parse::<LitInt>()?),
                            "char" => ch = Some(string(&p)?),
                            other => {
                                return Err(p.error(format!("unknown padding option `{other}`")));
                            }
                        }
                        Ok(())
                    })?;
                    let length = length.ok_or_else(|| m.error("padding needs a length"))?;
                    f.padding = Some((length, ch.unwrap_or_else(|| "0".into())));
                }
                "cast" => f.cast = Some(string(&m)?),
                "validate" => f.validate = Some(expr(&m)?),
                "validate_regex" => f.validate_regex = Some(lit_str(&m)?),
                "get" => f.get = Some(expr(&m)?),
                "set" => f.set = Some(expr(&m)?),
                "watch" => f.watch = idents(&m)?,
                "watch_all" => f.watch_all = true,
                "codec" => f.codec = Some(m.value()?.parse()?),
                "as_set" => f.as_set = true,
                "items" => m.parse_nested_meta(|p| {
                    match key_name(&p).as_str() {
                        "validate" => f.items_validate = Some(expr(&p)?),
                        "validate_regex" => f.items_validate_regex = Some(lit_str(&p)?),
                        other => return Err(p.error(format!("unknown items option `{other}`"))),
                    }
                    Ok(())
                })?,
                "json" => f.json = Some(string(&m)?),
                "descriptor" => f.descriptor = Some(m.value()?.parse::<LitStr>()?.parse()?),
                other => return Err(m.error(format!("unknown field option `{other}`"))),
            }
            Ok(())
        })?;
    }
    Ok(f)
}

/// The options of one enum variant.
#[derive(Default)]
pub struct VariantOpts {
    pub rename: Option<String>,
    /// Holds the values no other variant names.
    pub other: bool,
}

pub fn variant(attrs: &[Attribute]) -> Result<VariantOpts> {
    let mut v = VariantOpts::default();
    for a in attrs.iter().filter(|a| a.path().is_ident("wattdb")) {
        a.parse_nested_meta(|m| {
            match key_name(&m).as_str() {
                "rename" => v.rename = Some(string(&m)?),
                "other" => v.other = true,
                other => return Err(m.error(format!("unknown variant option `{other}`"))),
            }
            Ok(())
        })?;
    }
    Ok(v)
}
