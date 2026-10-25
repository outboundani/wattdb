//! Derive macros for wattdb. See the `wattdb` crate for the attribute
//! reference.

mod attrs;
mod collection;
mod entity;
mod enums;
mod fields;
mod map;
mod util;

use proc_macro::TokenStream;
use syn::{DeriveInput, parse_macro_input};

fn run(
    input: TokenStream,
    f: fn(DeriveInput) -> syn::Result<proc_macro2::TokenStream>,
) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    f(input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Derives an entity: its `Input`, `Partial`, `Key` and client types, and a
/// descriptor per attribute.
#[proc_macro_derive(Entity, attributes(wattdb))]
pub fn derive_entity(input: TokenStream) -> TokenStream {
    run(input, entity::derive)
}

/// Derives a struct stored as a DynamoDB map attribute.
#[proc_macro_derive(Map, attributes(wattdb))]
pub fn derive_map(input: TokenStream) -> TokenStream {
    run(input, map::derive)
}

/// Derives a string enum attribute.
#[proc_macro_derive(Enum, attributes(wattdb))]
pub fn derive_enum(input: TokenStream) -> TokenStream {
    run(input, enums::derive)
}

/// Derives a collection query's result struct: one `Vec` per member entity.
#[proc_macro_derive(Collection, attributes(wattdb))]
pub fn derive_collection(input: TokenStream) -> TokenStream {
    run(input, collection::derive)
}
