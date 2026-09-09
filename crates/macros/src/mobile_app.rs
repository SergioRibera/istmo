//! `#[istmo::mobile_app]` attribute macro — the mobile analogue of
//! `#[tokio::main]`.
//!
//! Applied to a user-owned function taking no arguments:
//!
//! ```ignore
//! #[istmo::mobile_app]
//! fn app() {
//!     // Boot eframe / winit / iced / freya / raw wgpu — user's choice.
//!     // The transport pump is already running: `IstmoRuntime.start()`
//!     // fires on the native side before this ever executes.
//! }
//! ```
//!
//! The macro is UI-agnostic. It preserves the user function verbatim and
//! emits two additional platform entry points that delegate to it:
//!
//! * `android_main(app: AndroidApp)` — the `no_mangle` symbol the
//!   `android-activity` NDK glue expects. Stores the passed `AndroidApp`
//!   handle in `istmo::mobile` so user code can retrieve it with
//!   `istmo::mobile::android_app()` when it is ready to hand it to the
//!   event-loop builder.
//! * `istmo_run_ios() -> i32` — a C-callable symbol iOS Swift shells
//!   invoke after `IstmoRuntime.shared.start()` has installed the
//!   transport pump.
//!
//! Neither variant embeds a UI framework or opinionated event loop —
//! istmo does not pick eframe for you any more than tokio picks a HTTP
//! server for you. The macro is purely a naming / registration
//! convenience.

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

    // Preserve the user's fn verbatim so the compiler still type-checks
    // its body under the caller's own imports. The emitted entry points
    // reference it by name.
    let user_fn_tokens = user_fn.to_token_stream();

    Ok(quote! {
        #user_fn_tokens

        // --- Android ---
        //
        // `android_main` is the symbol name `android-activity`'s NDK glue
        // dlsym's on process start. Stashing the `AndroidApp` handle in the
        // `istmo::android` globals lets the user's function retrieve it
        // later without threading it through argument lists.
        #[cfg(target_os = "android")]
        #[unsafe(no_mangle)]
        pub fn android_main(app: ::istmo::android::android_activity::AndroidApp) {
            ::istmo::android::set_android_app(app);
            #user_ident();
        }

        // --- iOS family ---
        //
        // Swift's `@main App` invokes this from its `init` after calling
        // `IstmoRuntime.shared.start()`. Returns a POSIX-style exit code so
        // a test harness / non-standard shell can inspect success; a real
        // app's event loop typically never returns here.
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

        // Desktop targets get no emitted entry points — a plain `cargo run`
        // on a laptop calls the user fn directly by name.
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
