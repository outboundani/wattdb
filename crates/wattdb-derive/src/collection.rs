//! `#[derive(Collection)]`: the result struct of a collection query.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, Fields, Result};

use crate::attrs;
use crate::fields::suffixed;
use crate::util::{collection_id, err, vec_inner};

pub fn derive(input: DeriveInput) -> Result<TokenStream> {
    let c = attrs::container(&input.attrs)?;
    let name = &input.ident;
    let vis = &input.vis;
    let coll = c
        .collection
        .ok_or_else(|| err(name.span(), "missing #[wattdb(collection = \"...\")]"))?;
    let Data::Struct(s) = &input.data else {
        return Err(err(name.span(), "#[derive(Collection)] needs a struct"));
    };
    let Fields::Named(named) = &s.fields else {
        return Err(err(
            name.span(),
            "#[derive(Collection)] needs named fields, one Vec per member entity",
        ));
    };
    let mut members = Vec::new();
    for f in &named.named {
        let ty = vec_inner(&f.ty).ok_or_else(|| {
            err(
                f.ident.as_ref().unwrap().span(),
                "each field must be a Vec of a member entity",
            )
        })?;
        members.push((f.ident.clone().unwrap(), ty.clone()));
    }
    let Some((_, first)) = members.first() else {
        return Err(err(name.span(), "a collection needs at least one member"));
    };
    let id = collection_id(&coll.value());
    let in_coll = quote!(<#first as ::wattdb::__private::InCollection<#id>>);
    let key_alias = suffixed(name, "Key");
    let client = suffixed(name, "Client");
    let tys: Vec<_> = members.iter().map(|(_, t)| t).collect();
    let pushes = members
        .iter()
        .enumerate()
        .map(|(i, (f, t))| quote!(#i => self.#f.push(<#t as ::wattdb::Entity>::decode(item)?),));
    let doc = format!("Queries the {} collection in one table.", coll.value());
    let key_doc = format!(
        "The {} collection's partition-key composites.",
        coll.value()
    );
    Ok(quote! {
        impl ::wattdb::CollectionResult for #name {
            const NAME: &'static str = #coll;

            fn members() -> ::wattdb::Result<::std::vec::Vec<(::std::string::String, &'static ::wattdb::engine::Model)>> {
                ::core::result::Result::Ok(::std::vec![#(::wattdb::__private::member::<#tys>()?),*])
            }

            fn push(&mut self, member: usize, item: &::wattdb::Attrs) -> ::wattdb::Result<()> {
                match member {
                    #(#pushes)*
                    _ => {}
                }
                ::core::result::Result::Ok(())
            }
        }

        #[doc = #key_doc]
        #vis type #key_alias = #in_coll::Key;

        #[doc = #doc]
        #[derive(::core::clone::Clone)]
        #vis struct #client {
            c: ::wattdb::Collection<#name>,
        }

        impl #client {
            /// A client for the collection in `table`.
            pub fn new(cfg: &::wattdb::Config, table: impl ::core::convert::Into<::std::string::String>) -> ::wattdb::Result<Self> {
                ::core::result::Result::Ok(#client { c: ::wattdb::Collection::new(cfg, table)? })
            }

            /// Queries the collection for one partition.
            pub fn query(&self, key: #key_alias) -> #in_coll::Query<#name> {
                #in_coll::query(self.c.query(&key))
            }
        }
    })
}
