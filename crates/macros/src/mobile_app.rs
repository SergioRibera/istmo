use proc_macro2::TokenStream;
use quote::{ToTokens, quote};
use syn::{ItemFn, parse::Parse, parse::ParseStream, parse2};

#[allow(unreachable_pub)]
pub fn expand(attr: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    let _args = parse2::<MobileAppArgs>(attr)?;
    let user_fn = parse2::<ItemFn>(item)?;
    let user_ident = user_fn.sig.ident.clone();

    if !user_fn.sig.inputs.is_empty() {
        return Err(syn::Error::new_spanned(
            &user_fn.sig,
            "`#[istmo::mobile_app]` targets a fn with no arguments; use `istmo::mobile::android_app()` inside the body to reach the AndroidApp handle when needed",
        ));
    }

    let user_fn_tokens = user_fn.to_token_stream();

    Ok(quote! {
        #user_fn_tokens

        #[cfg(target_os = "android")]
        #[unsafe(no_mangle)]
        pub fn android_main(app: ::istmo::android::android_activity::AndroidApp) {
            ::istmo::android::set_android_app(app);
            #user_ident();
        }

        #[cfg(any(
            target_os = "ios",
            target_os = "tvos",
            target_os = "watchos",
            target_os = "visionos",
        ))]
        #[unsafe(no_mangle)]
        pub extern "C" fn istmo_run_ios() -> i32 {
            #user_ident();
            0
        }

    })
}

struct MobileAppArgs;

impl Parse for MobileAppArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        if !input.is_empty() {
            return Err(
                input.error("`#[istmo::mobile_app]` does not take arguments in this version")
            );
        }
        Ok(Self)
    }
}

