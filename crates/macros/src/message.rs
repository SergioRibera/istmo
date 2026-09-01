//! `#[istmo::message]` attribute macro.

use proc_macro2::TokenStream;
use quote::quote;
use syn::punctuated::Punctuated;
use syn::{
    Expr, ExprLit, Item, Lit, LitStr, MetaNameValue, Path, Token, parse_quote, parse_str, parse2,
};

#[allow(unreachable_pub)]
pub fn expand(attr: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    let args: MessageArgs = parse2(attr)?;
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
    let bincode = &args.bincode_path;
    Ok(quote! {
        #[derive(#bincode::Encode, #bincode::Decode)]
        #item
    })
}

struct MessageArgs {
    bincode_path: Path,
}

impl syn::parse::Parse for MessageArgs {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let pairs = Punctuated::<MetaNameValue, Token![,]>::parse_terminated(input)?;
        let mut bincode_path: Option<Path> = None;
        for pair in pairs {
            let key = pair
                .path
                .get_ident()
                .ok_or_else(|| syn::Error::new_spanned(&pair.path, "expected identifier"))?
                .to_string();
            match key.as_str() {
                "bincode" => {
                    let raw = expect_lit_str(&pair.value)?;
                    bincode_path = Some(parse_str::<Path>(&raw.value()).map_err(|e| {
                        syn::Error::new_spanned(&pair.value, format!("invalid `bincode` path: {e}"))
                    })?);
                }
                other => {
                    return Err(syn::Error::new_spanned(
                        &pair.path,
                        format!("unknown argument `{other}`; expected `bincode`"),
                    ));
                }
            }
        }
        Ok(Self {
            bincode_path: bincode_path.unwrap_or_else(|| parse_quote!(::istmo::bincode)),
        })
    }
}

fn expect_lit_str(expr: &Expr) -> syn::Result<LitStr> {
    match expr {
        Expr::Lit(ExprLit {
            lit: Lit::Str(s), ..
        }) => Ok(s.clone()),
        _ => Err(syn::Error::new_spanned(expr, "expected string literal")),
    }
}
