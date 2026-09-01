//! `#[istmo::message]` attribute macro.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{Item, parse2};

#[allow(unreachable_pub)]
pub fn expand(_attr: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    let item: Item = parse2(item)?;
    match &item {
        Item::Struct(_) | Item::Enum(_) => {}
        _ => {
            return Err(syn::Error::new_spanned(
                &item,
                "#[istmo::message] can only be applied to struct or enum declarations",
            ));
        }
    }
    Ok(quote! {
        #[derive(::istmo::bincode::Encode, ::istmo::bincode::Decode)]
        #item
    })
}
