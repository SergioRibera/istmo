//! Rust ↔ mobile native interop framework.
//!
//! `istmo` lets a single Rust codebase drive Android and iOS through a
//! typed plugin contract. Plugin surfaces are declared once as async
//! Rust traits; codegen emits the matching Kotlin and Swift bindings,
//! and one wincode-framed wire protocol carries every call across the
//! FFI boundary.
//!
//! # Quick start
//!
//! Declare a plugin, wire the runtime, and call into it:
//!
//! ```
//! use istmo::{IstmoError, Runtime, message, plugin};
//!
//! #[message]
//! pub struct EchoInput {
//!     pub text: String,
//! }
//!
//! #[plugin(name = "demo.echo")]
//! pub trait Echo {
//!     async fn echo(&self, input: EchoInput) -> String;
//! }
//!
//! // Module-level wiring; the platform transport calls the generated
//! // `__istmo_configure_runtime` when the app starts.
//! istmo::runtime!(plugins: [EchoClient]);
//!
//! async fn greet(rt: &std::sync::Arc<Runtime>) -> Result<String, IstmoError> {
//!     let echo = EchoClient::from_runtime(rt)?;
//!     echo.echo(EchoInput { text: "hi".into() }).await
//! }
//! ```
//!
//! # Crate layout
//!
//! - [`Runtime`] and the wire protocol re-exported from
//!   [`istmo_core`](https://docs.rs/istmo-core).
//! - Proc-macros [`plugin`](macro@plugin), [`message`](macro@message),
//!   [`service`](macro@service), [`worker`](macro@worker),
//!   [`mobile_app`](macro@mobile_app), [`stream`](macro@stream),
//!   [`owned`](macro@owned), [`handle`](macro@handle) and the
//!   [`runtime!`](macro@runtime) wiring macro from
//!   [`istmo_macros`](https://docs.rs/istmo-macros).
//! - [`plugins`] — first-party plugin surfaces (lifecycle, deep links,
//!   permissions, notifications, ad slots, safe-area publisher, …).
//! - `istmo::android` / `istmo::ios` — platform transport crates,
//!   pulled in by target cfg. Not present on desktop targets. See
//!   [`istmo-android`](https://docs.rs/istmo-android) and
//!   [`istmo-ios`](https://docs.rs/istmo-ios).
//! - [`sync::lock`] — mutex locking that recovers from poisoning, for
//!   plugin state whose critical sections never leave it inconsistent.
//! - [`wincode`] — the encoding the wire derives use, re-exported so
//!   downstream crates don't need a direct `wincode` dependency.
//!
//! # Feature areas
//!
//! - **Typed plugins.** [`#[plugin]`](macro@plugin) turns an async
//!   trait into a client / host pair plus the wire codec. Streams are
//!   opt-in via [`#[stream]`](macro@stream); cooperative cancellation
//!   via [`CancelToken`].
//! - **Hosted lifecycles.** [`#[service]`](macro@service) models a
//!   long-running background service with `on_start` / `on_stop` hooks;
//!   [`#[worker]`](macro@worker) models a one-shot scheduled task.
//! - **Native handles.** [`NativeHandle<T>`](NativeHandle) carries an
//!   opaque platform-side reference (e.g. a Kotlin `Credential` object)
//!   across the wire and releases it on drop.
//! - **Early events.** Values published before the first subscriber
//!   attaches — used for launch-intent payloads such as cold-start deep
//!   links or notification taps.
//!
//! # Wire protocol
//!
//! All frames cross the FFI boundary as opaque wire-encoded byte
//! payloads. The current wire version is [`PROTOCOL_VERSION`]; runtimes
//! reject frames from mismatched peers.
//!
//! # License
//!
//! Dual-licensed under MIT or Apache-2.0.

#![doc(html_root_url = "https://docs.rs/istmo")]

pub use istmo_core::*;
pub use istmo_macros::{
    app_icon, handle, message, mobile_app, owned, plugin, runtime, service, stream, worker,
};

/// First-party plugin surfaces (lifecycle, deep links, permissions,
/// notifications, ad slots, safe-area, …).
pub use istmo_plugins as plugins;

/// Android JNI transport. Present on `target_os = "android"`.
#[cfg(target_os = "android")]
pub use istmo_android as android;

/// Apple platform transport. Present on iOS, tvOS, watchOS and visionOS.
#[cfg(any(
    target_os = "ios",
    target_os = "tvos",
    target_os = "watchos",
    target_os = "visionos",
))]
pub use istmo_ios as ios;

/// Paths the macros expand to (`::istmo::__private::…`). Declared here
/// so the facade names exactly what generated code needs; it shadows the
/// same-named module the glob above would bring in.
#[doc(hidden)]
pub mod __private {
    pub use istmo_core::__private::{block_on, flume, set_app_assets, set_app_info, tracing};
}

/// Instance key of the app crate for
/// [`plugins::DocumentsPublisher::start`]: `Some` when `istmo.toml`
/// enables `[app] single_instance` (on by default when the app declares
/// `[[app.document_type]]`), `None` otherwise.
///
/// Expands in the calling crate, so it reads the `ISTMO_APP_BUNDLE_ID`
/// and `ISTMO_SINGLE_INSTANCE` values `istmo-build` emitted for the app.
///
/// ```ignore
/// let key = istmo::single_instance!();
/// match istmo::plugins::DocumentsPublisher::start(&rt, std::env::args_os().skip(1), key.as_ref())? {
///     istmo::plugins::Startup::Forwarded => return Ok(()),
///     istmo::plugins::Startup::Primary(publisher) => run_app(publisher),
/// }
/// ```
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
#[macro_export]
macro_rules! single_instance {
    () => {
        $crate::plugins::instance_key_from_build(
            ::core::option_env!("ISTMO_APP_BUNDLE_ID"),
            ::core::option_env!("ISTMO_SINGLE_INSTANCE"),
        )
    };
}
