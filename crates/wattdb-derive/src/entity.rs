//! `#[derive(Entity)]`.

use std::collections::HashMap;

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{DeriveInput, Ident, Result};

use crate::attrs::{self, IndexOpts, KeyOpts, ProjectionOpts};
use crate::fields::{self, Field, attribute_def, decode_struct, encode_struct, serde_attrs, serde_field, stored_of, suffixed};
use crate::util::{err, ident, pascal, snake};

/// Methods the generated client gets from `Table` (or adds itself); access
/// patterns can't take these names.
const RESERVED: &[&str] = &[
    "get", "put", "create", "delete", "remove", "update", "patch", "upsert", "batch_get", "batch_put", "batch_delete",
    "check", "find", "matching", "parse", "conv", "scan", "query", "scan_index", "name", "config", "definition",
    "put_item", "new", "inner",
];

struct Pattern<'a> {
    opts: &'a IndexOpts,
    /// The pk and sk composites as fields.
    pk: Vec<&'a Field>,
    sk: Vec<&'a Field>,
}

fn key_fields<'a>(fields: &'a [Field], k: &KeyOpts) -> Result<Vec<&'a Field>> {
    let mut out = Vec::new();
    if let Some(c) = &k.composite {
        for id in c {
            out.push(field_named(fields, id)?);
        }
    } else if let Some(t) = &k.template {
        for name in template_vars(&t.value()) {
            let f = fields
                .iter()
                .find(|f| f.ident == name.as_str() || f.stored == name)
                .ok_or_else(|| err(t.span(), format!("template names unknown field `{name}`")))?;
            out.push(f);
        }
    }
    Ok(out)
}

fn field_named<'a>(fields: &'a [Field], id: &Ident) -> Result<&'a Field> {
    fields.iter().find(|f| f.ident == *id).ok_or_else(|| err(id.span(), format!("`{id}` isn't a field")))
}

fn template_vars(t: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = t;
    while let Some(i) = rest.find("${") {
        let after = &rest[i + 2..];
        match after.find('}') {
            Some(j) => {
                out.push(after[..j].to_string());
                rest = &after[j + 1..];
            }
            None => break,
        }
    }
    out
}

/// Rewrites `${name}` template variables, Rust field or stored names, to
/// stored names.
fn convert_template(t: &syn::LitStr, names: &HashMap<String, String>) -> Result<String> {
    let mut out = String::new();
    let mut rest = t.value();
    while let Some(i) = rest.find("${") {
        out.push_str(&rest[..i + 2]);
        let after = rest[i + 2..].to_string();
        match after.find('}') {
            Some(j) => {
                let name = &after[..j];
                // A Rust field name or a stored name.
                let stored = names
                    .get(name)
                    .or_else(|| names.values().find(|s| *s == name))
                    .ok_or_else(|| err(t.span(), format!("template names unknown field `{name}`")))?;
                out.push_str(stored);
                rest = after[j..].to_string();
            }
            None => {
                rest = after;
                break;
            }
        }
    }
    out.push_str(&rest);
    Ok(out)
}

fn key_def(k: &KeyOpts, names: &HashMap<String, String>) -> Result<TokenStream> {
    let field = &k.field;
    let composite = match (&k.composite, &k.template) {
        (Some(c), _) => {
            let c = c.iter().map(|id| stored_of(names, id)).collect::<Result<Vec<_>>>()?;
            quote!(::core::option::Option::Some(::std::vec![#(#c.to_string()),*]))
        }
        (None, Some(_)) => quote!(::core::option::Option::None),
        (None, None) => quote!(::core::option::Option::Some(::std::vec::Vec::new())),
    };
    let template = match &k.template {
        Some(t) => {
            let t = convert_template(t, names)?;
            quote!(::core::option::Option::Some(#t.to_string()))
        }
        None => quote!(::core::option::Option::None),
    };
    let (casing, cast) = (&k.casing, &k.cast);
    Ok(quote!(::wattdb::__private::KeyDef {
        field: #field.into(),
        composite: #composite,
        template: #template,
        casing: #casing.into(),
        cast: #cast.into(),
    }))
}

fn index_def(ix: &IndexOpts, names: &HashMap<String, String>, partial: &Ident) -> Result<TokenStream> {
    let (pattern, index, scope) = (&ix.access_pattern, &ix.index, &ix.scope);
    let ty = match ix.ty.as_str() {
        "" | "isolated" => quote!(::wattdb::__private::IndexType::Isolated),
        "clustered" => quote!(::wattdb::__private::IndexType::Clustered),
        "composite" => quote!(::wattdb::__private::IndexType::Composite),
        other => return Err(err(ix.span, format!("unknown index type {other:?}"))),
    };
    let collections = &ix.collections;
    let sub = ix.sub_collections;
    let condition = match &ix.condition {
        Some(c) => quote!(::core::option::Option::Some(::wattdb::__private::condition_fn::<#partial>(#c))),
        None => quote!(::core::option::Option::None),
    };
    let pk = key_def(&ix.pk, names)?;
    let sk = match &ix.sk {
        Some(k) => {
            let k = key_def(k, names)?;
            quote!(::core::option::Option::Some(#k))
        }
        None => quote!(::core::option::Option::None),
    };
    let projection = match &ix.projection {
        None => quote!(::core::option::Option::None),
        Some(ProjectionOpts::Kind(k)) => quote!(::core::option::Option::Some(::wattdb::__private::Projection {
            kind: #k.into(),
            attributes: ::std::vec::Vec::new(),
        })),
        Some(ProjectionOpts::Include(ids)) => {
            let a = ids.iter().map(|id| stored_of(names, id)).collect::<Result<Vec<_>>>()?;
            quote!(::core::option::Option::Some(::wattdb::__private::Projection {
                kind: ::std::string::String::new(),
                attributes: ::std::vec![#(#a.to_string()),*],
            }))
        }
    };
    Ok(quote!(::wattdb::__private::IndexDef {
        access_pattern: #pattern.into(),
        index: #index.into(),
        ty: #ty,
        collection: ::std::vec![#(#collections.to_string()),*],
        collection_array: #sub,
        scope: #scope.into(),
        condition: #condition,
        pk: #pk,
        sk: #sk,
        projection: #projection,
    }))
}

pub fn derive(input: DeriveInput) -> Result<TokenStream> {
    let c = attrs::container(&input.attrs)?;
    let name = &input.ident;
    let vis = &input.vis;
    let entity = c.entity.clone().ok_or_else(|| err(name.span(), "missing #[wattdb(entity = \"...\")]"))?;
    let version = c.version.clone().ok_or_else(|| err(name.span(), "missing #[wattdb(version = \"...\")]"))?;
    let service = c.service.clone().ok_or_else(|| err(name.span(), "missing #[wattdb(service = ...)]"))?;
    let fields = fields::parse(&input, c.rename_all.as_deref())?;
    let names = fields::stored_names(&fields);
    let e = quote!(#name);

    let input_ty = suffixed(name, "Input");
    let partial_ty = suffixed(name, "Partial");
    let filter_ty = suffixed(name, "Filter");
    let key_ty = suffixed(name, "Key");
    let client_ty = suffixed(name, "Client");
    let conv_ty = suffixed(name, "Conv");

    // Indexes.
    let mut patterns = Vec::new();
    for ix in &c.indexes {
        let pk = key_fields(&fields, &ix.pk)?;
        let sk = match &ix.sk {
            Some(k) => key_fields(&fields, k)?,
            None => Vec::new(),
        };
        patterns.push(Pattern { opts: ix, pk, sk });
    }
    let table = patterns
        .iter()
        .find(|p| p.opts.index.is_empty())
        .ok_or_else(|| err(name.span(), "the entity needs a table index: an index(...) without `index = \"...\"`"))?;
    let index_defs = c.indexes.iter().map(|ix| index_def(ix, &names, &partial_ty)).collect::<Result<Vec<_>>>()?;

    // Definition.
    let attr_defs = fields.iter().map(|f| attribute_def(f, &quote!(#partial_ty), &names)).collect::<Result<Vec<_>>>()?;
    let stored: Vec<&String> = fields.iter().map(|f| &f.stored).collect();

    // Generated types.
    let serde = serde_attrs(c.serde);
    let input_fields = fields.iter().map(|f| {
        let (ident, inner) = (&f.ident, &f.inner);
        let s = serde_field(c.serde, f, !f.plain_in_input());
        if f.plain_in_input() {
            quote!(#s pub #ident: #inner)
        } else {
            quote!(#s pub #ident: ::core::option::Option<#inner>)
        }
    });
    let partial_fields = fields.iter().map(|f| {
        let (ident, inner) = (&f.ident, &f.inner);
        let s = serde_field(c.serde, f, true);
        quote!(#s pub #ident: ::core::option::Option<#inner>)
    });
    let key_composites: Vec<&Field> = table.pk.iter().chain(table.sk.iter()).copied().collect();
    let key_fields_ts = key_composites.iter().map(|f| {
        let (ident, inner) = (&f.ident, &f.inner);
        let s = serde_field(c.serde, f, false);
        quote!(#s pub #ident: #inner)
    });

    let decode_self = decode_struct(&quote!(#name), &fields, false);
    let decode_partial = decode_struct(&quote!(#partial_ty), &fields, true);
    let encode_input = encode_struct(quote!(input), &fields, |f| !f.plain_in_input());
    let encode_partial = encode_struct(quote!(partial), &fields, |_| true);
    let encode_key = {
        let stmts = key_composites.iter().map(|f| {
            let (ident, inner, codec, stored) = (&f.ident, &f.inner, &f.codec, &f.stored);
            quote!(::wattdb::__private::encode::<#inner, #codec>(&mut out, #stored, &key.#ident)?;)
        });
        quote!({
            let mut out = ::wattdb::Attrs::new();
            #(#stmts)*
            ::core::result::Result::Ok(out)
        })
    };

    // Descriptors.
    let descriptors = fields.iter().map(|f| {
        let d = f.descriptor();
        let at = f.attr_ty(&e);
        let (inner, codec, stored) = (&f.inner, &f.codec, &f.stored);
        let doc = format!("The `{stored}` attribute, for conditions and updates.");
        quote! {
            #[doc = #doc]
            #vis fn #d() -> #at {
                <#at as ::wattdb::Descriptor<#e>>::new(::std::vec![#stored.to_string()], <#codec as ::wattdb::Codec<#inner>>::encode)
            }
        }
    });

    // Client: queries, scans and conversions.
    let mut client_methods = Vec::new();
    let mut extra_items = Vec::new();
    let mut conv_methods = Vec::new();
    for p in &patterns {
        let pattern = &p.opts.access_pattern;
        let method = snake(pattern);
        if RESERVED.contains(&method.as_str()) {
            return Err(err(p.opts.span, format!("access pattern {pattern:?} clashes with the client's `{method}` method")));
        }
        let method_id = ident(&method);
        let pp = pascal(pattern);
        if p.opts.ty != "composite" {
            let scan = format_ident!("scan_{}", method);
            if RESERVED.contains(&scan.to_string().as_str()) {
                return Err(err(p.opts.span, format!("access pattern {pattern:?} clashes with `{scan}`")));
            }
            let doc = format!("Reads every {} item in the {pattern} index.", entity.value());
            client_methods.push(quote! {
                #[doc = #doc]
                pub fn #scan(&self) -> ::wattdb::Scan<#e> { self.table.scan_index(#pattern) }
            });
        }
        let params = p.pk.iter().map(|f| {
            let (ident, inner) = (&f.ident, &f.inner);
            quote!(#ident: impl ::core::convert::Into<#inner>)
        });
        let facets = p.pk.iter().map(|f| {
            let (ident, inner, stored) = (&f.ident, &f.inner, &f.stored);
            let enc = f.encode_value(quote!(&::core::convert::Into::<#inner>::into(#ident)));
            quote!(::wattdb::__private::facet(#stored, #enc))
        });
        let doc = format!("Queries the {pattern} access pattern.");
        if p.sk.is_empty() {
            client_methods.push(quote! {
                #[doc = #doc]
                pub fn #method_id(&self, #(#params),*) -> ::wattdb::Query<#e> {
                    self.table.query(#pattern, ::std::vec![#(#facets),*])
                }
            });
        } else {
            let holder = format_ident!("{}{}Query", name, pp);
            client_methods.push(quote! {
                #[doc = #doc]
                pub fn #method_id(&self, #(#params),*) -> #holder {
                    #holder(self.table.query(#pattern, ::std::vec![#(#facets),*]))
                }
            });
            extra_items.push(query_steps(name, &pp, pattern, &p.sk, vis, &e));
        }
        let doc = format!("Narrows the conversions to the {pattern} access pattern.");
        conv_methods.push(quote! {
            #[doc = #doc]
            pub fn #method_id(&self) -> ::wattdb::IndexConv<#e> {
                self.0.index(#pattern).expect("a generated access pattern exists")
            }
        });
        for coll in &p.opts.collections {
            extra_items.push(collection_items(name, coll, p, vis, c.serde));
        }
    }

    let doc_client = format!("Reads and writes {} items in one table.", entity.value());
    let partial_doc = format!(
        "Every {} attribute as an `Option`. Callbacks receive it as their siblings; find/match take it as their filter.",
        entity.value()
    );
    let input_doc = format!(
        "A {} item to write. Required attributes without a default are plain values; the rest are `Option`s, where `None` means missing (the default applies).",
        entity.value()
    );
    let key_doc = format!("Addresses one {} item.", entity.value());
    let entity_lit = &entity;

    Ok(quote! {
        #[doc = #input_doc]
        #[derive(::core::clone::Clone, ::core::fmt::Debug, ::core::default::Default, ::core::cmp::PartialEq)]
        #serde
        #vis struct #input_ty {
            #(#input_fields,)*
        }

        #[doc = #partial_doc]
        #[derive(::core::clone::Clone, ::core::fmt::Debug, ::core::default::Default, ::core::cmp::PartialEq)]
        #serde
        #vis struct #partial_ty {
            #(#partial_fields,)*
        }

        /// Selects items by attribute values for find and match; `None` fields are ignored.
        #vis type #filter_ty = #partial_ty;

        #[doc = #key_doc]
        #[derive(::core::clone::Clone, ::core::fmt::Debug, ::core::default::Default, ::core::cmp::PartialEq)]
        #serde
        #vis struct #key_ty {
            #(#key_fields_ts,)*
        }

        impl ::wattdb::Entity for #name {
            type Input = #input_ty;
            type Partial = #partial_ty;
            type Key = #key_ty;

            fn definition() -> ::wattdb::Result<&'static ::wattdb::EntityDef> {
                static DEF: ::wattdb::__private::OnceLock<::core::result::Result<::wattdb::EntityDef, ::std::string::String>> =
                    ::wattdb::__private::OnceLock::new();
                ::wattdb::__private::entity_def(&DEF, || {
                    let mut def = ::wattdb::__private::Definition {
                        entity: #entity_lit.into(),
                        version: #version.into(),
                        service: ::wattdb::__private::service_name(#service),
                        ..::core::default::Default::default()
                    };
                    #(def.attributes.push(#attr_defs);)*
                    #(def.indexes.push(#index_defs);)*
                    ::core::result::Result::Ok(def)
                })
            }

            fn decode(attrs: &::wattdb::Attrs) -> ::wattdb::Result<Self> {
                #decode_self
            }

            fn decode_partial(attrs: &::wattdb::Attrs) -> ::wattdb::Result<#partial_ty> {
                #decode_partial
            }

            fn encode_input(input: &#input_ty) -> ::wattdb::Result<::wattdb::Attrs> {
                #encode_input
            }

            fn encode_partial(partial: &#partial_ty) -> ::wattdb::Result<::wattdb::Attrs> {
                #encode_partial
            }

            fn encode_key(key: &#key_ty) -> ::wattdb::Result<::wattdb::Attrs> {
                #encode_key
            }

            fn fields() -> &'static [&'static str] {
                &[#(#stored),*]
            }
        }

        impl ::wattdb::__private::FromAttrs for #partial_ty {
            fn from_attrs(attrs: &::wattdb::Attrs) -> ::wattdb::Result<Self> {
                <#name as ::wattdb::Entity>::decode_partial(attrs)
            }
        }

        impl #name {
            #(#descriptors)*
        }

        #[doc = #doc_client]
        #[derive(::core::clone::Clone, ::core::fmt::Debug)]
        #vis struct #client_ty {
            table: ::wattdb::Table<#name>,
        }

        impl ::core::ops::Deref for #client_ty {
            type Target = ::wattdb::Table<#name>;

            fn deref(&self) -> &::wattdb::Table<#name> {
                &self.table
            }
        }

        impl #client_ty {
            /// A client for the items in `table`.
            pub fn new(cfg: &::wattdb::Config, table: impl ::core::convert::Into<::std::string::String>) -> ::wattdb::Result<Self> {
                ::core::result::Result::Ok(#client_ty { table: ::wattdb::Table::new(cfg, table)? })
            }

            /// The underlying table.
            pub fn inner(&self) -> &::wattdb::Table<#name> {
                &self.table
            }

            /// Converts between composite attributes, keys and cursors.
            pub fn conv(&self) -> #conv_ty {
                #conv_ty(self.table.conv())
            }

            #(#client_methods)*
        }

        /// Converts between composite attributes, keys and cursors; its
        /// access-pattern methods narrow it to one index.
        #vis struct #conv_ty(::wattdb::Conv<#name>);

        impl ::core::ops::Deref for #conv_ty {
            type Target = ::wattdb::Conv<#name>;

            fn deref(&self) -> &::wattdb::Conv<#name> {
                &self.0
            }
        }

        impl #conv_ty {
            #(#conv_methods)*
        }

        #(#extra_items)*
    })
}

/// The sort-key steps of a query: one holder and one step per composite
/// (decision D29).
fn query_steps(name: &Ident, pp: &str, pattern: &str, sk: &[&Field], vis: &syn::Visibility, e: &TokenStream) -> TokenStream {
    let mut out = Vec::new();
    for (i, f) in sk.iter().enumerate() {
        let holder = if i == 0 {
            format_ident!("{}{}Query", name, pp)
        } else {
            format_ident!("{}{}Query{}", name, pp, pascal(&sk[i - 1].ident.to_string()))
        };
        let sp = pascal(&f.ident.to_string());
        let step = format_ident!("{}{}{}Step", name, pp, sp);
        let method = f.descriptor();
        let stored = &f.stored;
        let inner = &f.inner;
        let enc = |v: TokenStream| f.encode_value(quote!(&::core::convert::Into::<#inner>::into(#v)));
        let (ev, lo, hi) = (enc(quote!(v)), enc(quote!(lo)), enc(quote!(hi)));
        let holder_doc = if i == 0 {
            format!("A {pattern} query; `{method}()` adds a sort-key condition.")
        } else {
            format!("A {pattern} query with `{}` fixed; `{method}()` adds a sort-key condition.", sk[i - 1].ident)
        };
        let eq = if i < sk.len() - 1 {
            let next = format_ident!("{}{}Query{}", name, pp, sp);
            quote! {
                /// Fixes the composite and moves to the next one.
                pub fn eq(self, v: impl ::core::convert::Into<#inner>) -> #next { #next(self.0.key_eq(#stored, #ev)) }
            }
        } else {
            quote! {
                /// Fixes the composite, completing the sort key.
                pub fn eq(self, v: impl ::core::convert::Into<#inner>) -> ::wattdb::Query<#e> { self.0.key_eq(#stored, #ev) }
            }
        };
        let step_doc = format!("A sort-key condition on `{stored}`.");
        out.push(quote! {
            #[doc = #holder_doc]
            #vis struct #holder(::wattdb::Query<#e>);

            impl #holder {
                #[doc = #step_doc]
                pub fn #method(self) -> #step { #step(self.0) }
            }

            ::wattdb::__forward_query!(#holder, #e);

            #[doc = #step_doc]
            #vis struct #step(::wattdb::Query<#e>);

            impl #step {
                #eq
                pub fn gt(self, v: impl ::core::convert::Into<#inner>) -> ::wattdb::Query<#e> { self.0.key_compare("gt", #stored, #ev) }
                pub fn gte(self, v: impl ::core::convert::Into<#inner>) -> ::wattdb::Query<#e> { self.0.key_compare("gte", #stored, #ev) }
                pub fn lt(self, v: impl ::core::convert::Into<#inner>) -> ::wattdb::Query<#e> { self.0.key_compare("lt", #stored, #ev) }
                pub fn lte(self, v: impl ::core::convert::Into<#inner>) -> ::wattdb::Query<#e> { self.0.key_compare("lte", #stored, #ev) }
                /// An inclusive range.
                pub fn between(self, lo: impl ::core::convert::Into<#inner>, hi: impl ::core::convert::Into<#inner>) -> ::wattdb::Query<#e> {
                    self.0.key_between(#stored, #lo, #hi)
                }
                pub fn begins_with(self, v: impl ::core::convert::Into<#inner>) -> ::wattdb::Query<#e> { self.0.key_begins(#stored, #ev) }
            }
        });
    }
    quote!(#(#out)*)
}

/// The key and query types of one collection the entity is in (decision
/// D43). The collection derive uses the first member's.
fn collection_items(name: &Ident, coll: &str, p: &Pattern<'_>, vis: &syn::Visibility, serde_on: bool) -> TokenStream {
    let cp = pascal(coll);
    let key = format_ident!("{}{}CollectionKey", name, cp);
    let query = format_ident!("{}{}CollectionQuery", name, cp);
    let id = crate::util::collection_id(coll);
    let serde = serde_attrs(serde_on);
    let key_fields = p.pk.iter().map(|f| {
        let (ident, inner) = (&f.ident, &f.inner);
        let s = serde_field(serde_on, f, false);
        quote!(#s pub #ident: #inner)
    });
    let facets = p.pk.iter().map(|f| {
        let (ident, stored) = (&f.ident, &f.stored);
        let enc = f.encode_value(quote!(&self.#ident));
        quote!(::wattdb::__private::facet(#stored, #enc))
    });
    let key_doc = format!("The {coll} collection's partition-key composites.");
    let query_doc = format!("A {coll} collection query.");
    let steps = if p.opts.ty != "" && p.opts.ty != "isolated" && !p.sk.is_empty() {
        collection_steps(name, &cp, coll, &p.sk, vis)
    } else {
        quote!()
    };
    quote! {
        #[doc = #key_doc]
        #[derive(::core::clone::Clone, ::core::fmt::Debug, ::core::default::Default, ::core::cmp::PartialEq)]
        #serde
        #vis struct #key {
            #(#key_fields,)*
        }

        impl ::wattdb::CollectionKey for #key {
            fn facets(&self) -> ::std::vec::Vec<(::std::string::String, ::wattdb::__private::Encoded)> {
                ::std::vec![#(#facets),*]
            }
        }

        #[doc = #query_doc]
        #vis struct #query<R: ::wattdb::CollectionResult>(::wattdb::CollectionQuery<R>);

        impl<R: ::wattdb::CollectionResult> #query<R> {
            #[doc(hidden)]
            pub fn __new(q: ::wattdb::CollectionQuery<R>) -> Self { Self(q) }
        }

        ::wattdb::__forward_collection_query!(#query);

        impl ::wattdb::__private::InCollection<#id> for #name {
            type Key = #key;
            type Query<R: ::wattdb::CollectionResult> = #query<R>;

            fn query<R: ::wattdb::CollectionResult>(q: ::wattdb::CollectionQuery<R>) -> #query<R> {
                #query(q)
            }
        }

        #steps
    }
}

fn collection_steps(name: &Ident, cp: &str, coll: &str, sk: &[&Field], vis: &syn::Visibility) -> TokenStream {
    let mut out = Vec::new();
    let full = quote!(::wattdb::CollectionQuery<R>);
    for (i, f) in sk.iter().enumerate() {
        let holder = if i == 0 {
            format_ident!("{}{}CollectionQuery", name, cp)
        } else {
            format_ident!("{}{}CollectionQuery{}", name, cp, pascal(&sk[i - 1].ident.to_string()))
        };
        let sp = pascal(&f.ident.to_string());
        let step = format_ident!("{}{}Collection{}Step", name, cp, sp);
        let method = f.descriptor();
        let stored = &f.stored;
        let inner = &f.inner;
        let enc = |v: TokenStream| f.encode_value(quote!(&::core::convert::Into::<#inner>::into(#v)));
        let (ev, lo, hi) = (enc(quote!(v)), enc(quote!(lo)), enc(quote!(hi)));
        let eq = if i < sk.len() - 1 {
            let next = format_ident!("{}{}CollectionQuery{}", name, cp, sp);
            quote! {
                /// Fixes the composite and moves to the next one.
                pub fn eq(self, v: impl ::core::convert::Into<#inner>) -> #next<R> { #next(self.0.key_eq(#stored, #ev)) }
            }
        } else {
            quote! {
                /// Fixes the composite, completing the sort key.
                pub fn eq(self, v: impl ::core::convert::Into<#inner>) -> #full { self.0.key_eq(#stored, #ev) }
            }
        };
        let holder_def = if i == 0 {
            quote!()
        } else {
            let doc = format!("A {coll} collection query with `{}` fixed.", sk[i - 1].ident);
            quote! {
                #[doc = #doc]
                #vis struct #holder<R: ::wattdb::CollectionResult>(::wattdb::CollectionQuery<R>);
                ::wattdb::__forward_collection_query!(#holder);
            }
        };
        let step_doc = format!("A sort-key condition on `{stored}`.");
        out.push(quote! {
            #holder_def

            impl<R: ::wattdb::CollectionResult> #holder<R> {
                #[doc = #step_doc]
                pub fn #method(self) -> #step<R> { #step(self.0) }
            }

            #[doc = #step_doc]
            #vis struct #step<R: ::wattdb::CollectionResult>(::wattdb::CollectionQuery<R>);

            impl<R: ::wattdb::CollectionResult> #step<R> {
                #eq
                pub fn gt(self, v: impl ::core::convert::Into<#inner>) -> #full { self.0.key_compare("gt", #stored, #ev) }
                pub fn gte(self, v: impl ::core::convert::Into<#inner>) -> #full { self.0.key_compare("gte", #stored, #ev) }
                pub fn lt(self, v: impl ::core::convert::Into<#inner>) -> #full { self.0.key_compare("lt", #stored, #ev) }
                pub fn lte(self, v: impl ::core::convert::Into<#inner>) -> #full { self.0.key_compare("lte", #stored, #ev) }
                /// An inclusive range.
                pub fn between(self, lo: impl ::core::convert::Into<#inner>, hi: impl ::core::convert::Into<#inner>) -> #full {
                    self.0.key_between(#stored, #lo, #hi)
                }
                pub fn begins_with(self, v: impl ::core::convert::Into<#inner>) -> #full { self.0.key_begins(#stored, #ev) }
            }
        });
    }
    quote!(#(#out)*)
}
