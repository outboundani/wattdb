//! `#[derive(Map)]`: a struct stored as a DynamoDB map.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{DeriveInput, Result};

use crate::attrs;
use crate::fields::{self, attribute_def, decode_struct, encode_struct, suffixed};

pub fn derive(input: DeriveInput) -> Result<TokenStream> {
    let c = attrs::container(&input.attrs)?;
    let name = &input.ident;
    let vis = &input.vis;
    let fields = fields::parse(&input, c.rename_all.as_deref())?;
    let names = fields::stored_names(&fields);
    let attr_ty = suffixed(name, "Attr");

    let props = fields.iter().map(|f| attribute_def(f, &quote!(#name), &names)).collect::<Result<Vec<_>>>()?;
    let decode = decode_struct(&quote!(#name), &fields, false);
    let encode = encode_struct(quote!(self), &fields, |f| f.optional);
    let e = quote!(E);
    let accessors = fields.iter().map(|f| {
        let d = f.descriptor();
        let at = f.attr_ty(&e);
        let (inner, codec, stored) = (&f.inner, &f.codec, &f.stored);
        let doc = format!("The `{stored}` property.");
        quote! {
            #[doc = #doc]
            pub fn #d(&self) -> #at {
                <#at as ::wattdb::Descriptor<E>>::new(self.base.child(#stored), <#codec as ::wattdb::Codec<#inner>>::encode)
            }
        }
    });
    let doc = format!("Describes a `{name}` map attribute; its methods address the map's properties.");

    Ok(quote! {
        impl ::wattdb::FieldType for #name {
            type Attr<E: 'static> = #attr_ty<E>;

            fn type_def() -> ::wattdb::__private::AttributeDef {
                let mut a = ::wattdb::__private::AttributeDef {
                    ty: ::wattdb::__private::AttrType::Map,
                    ..::core::default::Default::default()
                };
                a.properties = ::core::option::Option::Some(::std::vec![#(#props),*]);
                a
            }

            fn encode(&self) -> ::wattdb::Result<::wattdb::Value> {
                let out: ::wattdb::Result<::wattdb::Attrs> = #encode;
                out.map(::wattdb::Value::Map)
            }

            fn decode(v: &::wattdb::Value) -> ::wattdb::Result<Self> {
                match v {
                    ::wattdb::Value::Map(attrs) => <Self as ::wattdb::__private::FromAttrs>::from_attrs(attrs),
                    _ => ::core::result::Result::Err(::wattdb::Error::Decode(::std::format!("expected a map for {}", ::core::stringify!(#name)))),
                }
            }
        }

        impl ::wattdb::__private::FromAttrs for #name {
            fn from_attrs(attrs: &::wattdb::Attrs) -> ::wattdb::Result<Self> {
                #decode
            }
        }

        #[doc = #doc]
        #vis struct #attr_ty<E> {
            base: ::wattdb::Attr<E, #name>,
        }

        impl<E> ::core::clone::Clone for #attr_ty<E> {
            fn clone(&self) -> Self {
                #attr_ty { base: self.base.clone() }
            }
        }

        impl<E: 'static> ::core::ops::Deref for #attr_ty<E> {
            type Target = ::wattdb::Attr<E, #name>;

            fn deref(&self) -> &::wattdb::Attr<E, #name> {
                &self.base
            }
        }

        impl<E: 'static> ::wattdb::Descriptor<E> for #attr_ty<E> {
            type Value = #name;

            fn new(path: ::std::vec::Vec<::std::string::String>, encode: ::wattdb::Encoder<#name>) -> Self {
                #attr_ty { base: <::wattdb::Attr<E, #name> as ::wattdb::Descriptor<E>>::new(path, encode) }
            }

            fn path(&self) -> &[::std::string::String] {
                ::wattdb::Descriptor::<E>::path(&self.base)
            }

            fn encode(&self, v: &#name) -> ::wattdb::Result<::wattdb::Value> {
                ::wattdb::Descriptor::<E>::encode(&self.base, v)
            }
        }

        impl<E: 'static> ::wattdb::AttrList<E> for #attr_ty<E> {
            fn root_fields(&self) -> ::std::vec::Vec<::std::string::String> {
                ::wattdb::AttrList::<E>::root_fields(&self.base)
            }
        }

        impl<E: 'static> #attr_ty<E> {
            #(#accessors)*
        }
    })
}
