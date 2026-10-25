//! The fields of entity and map structs, and their attribute definitions.

use std::collections::HashMap;

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::spanned::Spanned;
use syn::{Data, DeriveInput, Fields, Ident, Result, Type};

use crate::attrs::{self, FieldOpts};
use crate::util::{err, option_inner, rename, vec_inner};

pub struct Field {
    pub ident: Ident,
    /// The type without `Option`.
    pub inner: Type,
    /// Declared as an `Option`.
    pub optional: bool,
    /// The stored field name, also the ElectroDB attribute name.
    pub stored: String,
    pub opts: FieldOpts,
    /// The codec type.
    pub codec: TokenStream,
}

impl Field {
    /// The descriptor type for entity `e`.
    pub fn attr_ty(&self, e: &TokenStream) -> TokenStream {
        let (inner, codec) = (&self.inner, &self.codec);
        quote!(<#codec as ::wattdb::Codec<#inner>>::Attr<#e>)
    }

    /// The descriptor function's name.
    pub fn descriptor(&self) -> Ident {
        self.opts.descriptor.clone().unwrap_or_else(|| self.ident.clone())
    }

    /// Encodes `value` (a `&T`) for a key condition.
    pub fn encode_value(&self, value: TokenStream) -> TokenStream {
        let (inner, codec) = (&self.inner, &self.codec);
        quote!(::wattdb::__private::encode_value::<#inner, #codec>(#value))
    }

    /// A required attribute without a default is a plain value in `Input`.
    pub fn plain_in_input(&self) -> bool {
        self.opts.required && self.opts.default.is_none()
    }
}

/// Parses a struct's named fields.
pub fn parse(input: &DeriveInput, rename_all: Option<&str>) -> Result<Vec<Field>> {
    let Data::Struct(s) = &input.data else {
        return Err(err(input.ident.span(), "wattdb derives need a struct with named fields"));
    };
    let Fields::Named(named) = &s.fields else {
        return Err(err(input.ident.span(), "wattdb derives need a struct with named fields"));
    };
    if !input.generics.params.is_empty() {
        return Err(err(input.generics.span(), "wattdb derives don't support generic structs"));
    }
    let mut out = Vec::new();
    for f in &named.named {
        let ident = f.ident.clone().unwrap();
        let opts = attrs::field(&f.attrs)?;
        let name = ident.to_string();
        let name = name.strip_prefix("r#").unwrap_or(&name).to_string();
        let stored = match (&opts.field, rename_all) {
            (Some(s), _) => s.clone(),
            (None, Some(rule)) => rename(rule, &name).ok_or_else(|| err(f.span(), format!("unknown rename_all rule {rule:?}")))?,
            (None, None) => name,
        };
        let (inner, optional) = match option_inner(&f.ty) {
            Some(t) => (t.clone(), true),
            None => (f.ty.clone(), false),
        };
        if opts.as_set && opts.codec.is_some() {
            return Err(err(f.span(), "`as_set` and `codec` can't be combined"));
        }
        if opts.as_set && vec_inner(&inner).is_none() {
            return Err(err(f.span(), "`as_set` needs a Vec"));
        }
        let codec = match (&opts.codec, opts.as_set) {
            (Some(c), _) => quote!(#c),
            (None, true) => quote!(::wattdb::codec::VecSet),
            (None, false) => quote!(::wattdb::DefaultCodec),
        };
        out.push(Field { ident, inner, optional, stored, opts, codec });
    }
    let mut seen = HashMap::new();
    for f in &out {
        if let Some(prev) = seen.insert(f.stored.clone(), f.ident.to_string()) {
            return Err(err(f.ident.span(), format!("field {:?} is already used by `{prev}`", f.stored)));
        }
    }
    Ok(out)
}

/// Maps Rust field names to stored names.
pub fn stored_names(fields: &[Field]) -> HashMap<String, String> {
    fields.iter().map(|f| (f.ident.to_string(), f.stored.clone())).collect()
}

pub fn stored_of(names: &HashMap<String, String>, id: &Ident) -> Result<String> {
    names.get(&id.to_string()).cloned().ok_or_else(|| err(id.span(), format!("`{id}` isn't a field")))
}

/// The expression building a field's attribute definition. Callbacks
/// receive `siblings` (an entity's Partial, or the map struct).
pub fn attribute_def(f: &Field, siblings: &TokenStream, names: &HashMap<String, String>) -> Result<TokenStream> {
    let (inner, codec, stored) = (&f.inner, &f.codec, &f.stored);
    let o = &f.opts;
    let mut set = Vec::new();
    if o.required {
        set.push(quote!(a.required = true;));
    }
    if o.hidden {
        set.push(quote!(a.hidden = true;));
    }
    if o.read_only {
        set.push(quote!(a.read_only = true;));
    }
    if let Some(l) = &o.label {
        set.push(quote!(a.label = ::core::option::Option::Some(#l.into());));
    }
    if let Some((len, ch)) = &o.padding {
        set.push(quote!(a.padding = ::core::option::Option::Some(::wattdb::__private::Padding { length: #len, char: #ch.into() });));
    }
    if let Some(c) = &o.cast {
        set.push(quote!(a.cast = #c.into();));
    }
    if let Some(d) = &o.default {
        set.push(quote!(a.default = ::core::option::Option::Some(::wattdb::__private::default_fn::<#inner, #codec>(|| ::core::convert::Into::<#inner>::into(#d)));));
    }
    if let Some(g) = &o.get {
        set.push(quote!(a.get = ::core::option::Option::Some(::wattdb::__private::transform_fn::<#inner, #codec, #siblings>(#g));));
    }
    if let Some(s) = &o.set {
        set.push(quote!(a.set = ::core::option::Option::Some(::wattdb::__private::transform_fn::<#inner, #codec, #siblings>(#s));));
    }
    if let Some(v) = &o.validate {
        set.push(quote!(a.validate = ::core::option::Option::Some(::wattdb::__private::validate_fn::<#inner, #codec, _>(#v));));
    }
    if let Some(r) = &o.validate_regex {
        check_regex(r)?;
        set.push(quote!(a.validate_regex = ::core::option::Option::Some(::wattdb::__private::regex(#r));));
    }
    if !o.watch.is_empty() {
        let w = o.watch.iter().map(|id| stored_of(names, id)).collect::<Result<Vec<_>>>()?;
        set.push(quote!(a.watch = ::std::vec![#(#w.to_string()),*];));
    }
    if o.watch_all {
        set.push(quote!(a.watch_all = true;));
    }
    if o.items_validate.is_some() || o.items_validate_regex.is_some() {
        let item = vec_inner(inner).ok_or_else(|| err(f.ident.span(), "`items(...)` needs a Vec"))?;
        let mut it = Vec::new();
        if let Some(v) = &o.items_validate {
            it.push(quote!(items.validate = ::core::option::Option::Some(::wattdb::__private::validate_fn::<#item, ::wattdb::DefaultCodec, _>(#v));));
        }
        if let Some(r) = &o.items_validate_regex {
            check_regex(r)?;
            it.push(quote!(items.validate_regex = ::core::option::Option::Some(::wattdb::__private::regex(#r));));
        }
        set.push(quote!(if let ::core::option::Option::Some(items) = a.items.as_mut() { #(#it)* }));
    }
    Ok(quote!({
        let mut a: ::wattdb::__private::AttributeDef = <#codec as ::wattdb::Codec<#inner>>::type_def();
        a.name = #stored.into();
        a.field = #stored.into();
        #(#set)*
        a
    }))
}

/// Checks a `validate_regex` pattern when the schema compiles (Rust regex
/// syntax).
fn check_regex(r: &syn::LitStr) -> Result<()> {
    regex::Regex::new(&r.value()).map(|_| ()).map_err(|e| err(r.span(), format!("invalid validate_regex: {e}")))
}

/// Decodes the fields of `ty` from `attrs`, keeping the declared shapes:
/// `Option` fields stay optional, others default when missing.
pub fn decode_struct(ty: &TokenStream, fields: &[Field], all_optional: bool) -> TokenStream {
    let inits = fields.iter().map(|f| {
        let (ident, inner, codec, stored) = (&f.ident, &f.inner, &f.codec, &f.stored);
        if f.optional || all_optional {
            quote!(#ident: ::wattdb::__private::decode::<#inner, #codec>(attrs, #stored)?)
        } else {
            quote!(#ident: ::wattdb::__private::decode_or_default::<#inner, #codec>(attrs, #stored)?)
        }
    });
    quote!(::core::result::Result::Ok(#ty { #(#inits),* }))
}

/// Encodes `src`'s fields; `optional(f)` says which are `Option`s there.
pub fn encode_struct(src: TokenStream, fields: &[Field], optional: impl Fn(&Field) -> bool) -> TokenStream {
    let stmts = fields.iter().map(|f| {
        let (ident, inner, codec, stored) = (&f.ident, &f.inner, &f.codec, &f.stored);
        if optional(f) {
            quote!(::wattdb::__private::encode_opt::<#inner, #codec>(&mut out, #stored, &#src.#ident)?;)
        } else {
            quote!(::wattdb::__private::encode::<#inner, #codec>(&mut out, #stored, &#src.#ident)?;)
        }
    });
    quote!({
        let mut out = ::wattdb::Attrs::new();
        #(#stmts)*
        ::core::result::Result::Ok(out)
    })
}

/// Serde attributes for a generated struct and its fields.
pub fn serde_attrs(enabled: bool) -> TokenStream {
    if !enabled {
        return quote!();
    }
    quote! {
        #[derive(::wattdb::__private::serde::Serialize, ::wattdb::__private::serde::Deserialize)]
        #[serde(crate = "::wattdb::__private::serde")]
    }
}

pub fn serde_field(enabled: bool, f: &Field, optional: bool) -> TokenStream {
    if !enabled {
        return quote!();
    }
    let name = f.opts.json.clone().unwrap_or_else(|| f.stored.clone());
    if optional {
        quote!(#[serde(rename = #name, default, skip_serializing_if = "Option::is_none")])
    } else {
        quote!(#[serde(rename = #name)])
    }
}

pub fn suffixed(base: &Ident, suffix: &str) -> Ident {
    format_ident!("{}{}", base, suffix)
}
