use proc_macro::TokenStream;

#[proc_macro_derive(Entity, attributes(wattdb))]
pub fn derive_entity(_input: TokenStream) -> TokenStream {
    TokenStream::new()
}
#[proc_macro_derive(Map, attributes(wattdb))]
pub fn derive_map(_input: TokenStream) -> TokenStream {
    TokenStream::new()
}
#[proc_macro_derive(Enum, attributes(wattdb))]
pub fn derive_enum(_input: TokenStream) -> TokenStream {
    TokenStream::new()
}
#[proc_macro_derive(Collection, attributes(wattdb))]
pub fn derive_collection(_input: TokenStream) -> TokenStream {
    TokenStream::new()
}
