//! `#[derive(Enum)]`: a string enum (decision D19).

use proc_macro2::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, Fields, Result};

use crate::attrs;
use crate::util::{err, rename};

pub fn derive(input: DeriveInput) -> Result<TokenStream> {
    let c = attrs::container(&input.attrs)?;
    let name = &input.ident;
    let Data::Enum(e) = &input.data else {
        return Err(err(name.span(), "#[derive(Enum)] needs an enum"));
    };
    let mut variants = Vec::new();
    let mut values = Vec::new();
    let mut other = None;
    for v in &e.variants {
        let o = attrs::variant(&v.attrs)?;
        if o.other {
            let one_string = matches!(&v.fields, Fields::Unnamed(f) if f.unnamed.len() == 1);
            if !one_string || o.rename.is_some() {
                return Err(err(v.ident.span(), "an `other` variant holds the value: `Other(String)`"));
            }
            if other.replace(&v.ident).is_some() {
                return Err(err(v.ident.span(), "only one variant can be `other`"));
            }
            continue;
        }
        if !matches!(v.fields, Fields::Unit) {
            return Err(err(v.ident.span(), "enum variants can't hold data, except one `#[wattdb(other)]` variant"));
        }
        let value = match (o.rename, &c.rename_all) {
            (Some(r), _) => r,
            (None, Some(rule)) => rename(rule, &v.ident.to_string())
                .ok_or_else(|| err(v.ident.span(), format!("unknown rename_all rule {rule:?}")))?,
            (None, None) => v.ident.to_string(),
        };
        if values.contains(&value) {
            return Err(err(v.ident.span(), format!("value {value:?} is used twice")));
        }
        variants.push(&v.ident);
        values.push(value);
    }
    let msg = format!("{{:?}} isn't a {name} value");
    let (other_str, other_parse) = match other {
        Some(o) => (quote!(#name::#o(s) => s.as_str(),), quote!(::core::result::Result::Ok(#name::#o(s.to_string())))),
        None => (quote!(), quote!(::core::result::Result::Err(::wattdb::Error::Decode(::std::format!(#msg, s))))),
    };
    let values_doc = if other.is_some() {
        "Every named value, in declaration order (not the `other` variant)."
    } else {
        "Every value, in declaration order."
    };
    let serde = if c.serde {
        quote! {
            impl ::wattdb::__private::serde::Serialize for #name {
                fn serialize<S: ::wattdb::__private::serde::Serializer>(&self, s: S) -> ::core::result::Result<S::Ok, S::Error> {
                    s.serialize_str(self.as_str())
                }
            }

            impl<'de> ::wattdb::__private::serde::Deserialize<'de> for #name {
                fn deserialize<D: ::wattdb::__private::serde::Deserializer<'de>>(d: D) -> ::core::result::Result<Self, D::Error> {
                    let s = <::std::string::String as ::wattdb::__private::serde::Deserialize>::deserialize(d)?;
                    s.parse().map_err(<D::Error as ::wattdb::__private::serde::de::Error>::custom)
                }
            }
        }
    } else {
        quote!()
    };
    Ok(quote! {
        #serde

        impl #name {
            #[doc = #values_doc]
            pub const VALUES: &'static [#name] = &[#(#name::#variants),*];

            /// The stored value.
            pub fn as_str(&self) -> &str {
                match self {
                    #(#name::#variants => #values,)*
                    #other_str
                }
            }
        }

        impl ::core::fmt::Display for #name {
            fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl ::core::str::FromStr for #name {
            type Err = ::wattdb::Error;

            fn from_str(s: &str) -> ::core::result::Result<Self, Self::Err> {
                match s {
                    #(#values => ::core::result::Result::Ok(#name::#variants),)*
                    _ => #other_parse,
                }
            }
        }

        impl ::wattdb::FieldType for #name {
            type Attr<E: 'static> = ::wattdb::Attr<E, #name>;

            fn type_def() -> ::wattdb::__private::AttributeDef {
                ::wattdb::__private::AttributeDef {
                    ty: ::wattdb::__private::AttrType::Enum,
                    enum_values: ::std::vec![#(#values.to_string()),*],
                    ..::core::default::Default::default()
                }
            }

            fn encode(&self) -> ::wattdb::Result<::wattdb::Value> {
                ::core::result::Result::Ok(::wattdb::Value::String(self.as_str().to_string()))
            }

            fn decode(v: &::wattdb::Value) -> ::wattdb::Result<Self> {
                match v {
                    ::wattdb::Value::String(s) => s.parse(),
                    _ => ::core::result::Result::Err(::wattdb::Error::Decode(::std::format!("expected a {} string", ::core::stringify!(#name)))),
                }
            }
        }
    })
}
