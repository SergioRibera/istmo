//! Rust ↔ mobile native interop framework.
//!
//! `istmo` lets a single Rust codebase drive Android and iOS through a
//! typed plugin contract. Plugin surfaces are declared once as async
//! Rust traits; codegen emits the matching Kotlin and Swift bindings,
//! and one bincode-framed wire protocol carries every call across the
//! FFI boundary.
//!
//! # Quick start
//!
//! Declare a plugin, wire the runtime, and call into it:
//!
//! ```ignore
//! use istmo::{message, plugin, runtime, IstmoError};
//!
//! #[message]
//! pub struct EchoInput { pub text: String }
//!
//! #[plugin(id = "demo.echo")]
//! pub trait Echo {
//!     async fn echo(&self, input: EchoInput) -> Result<String, IstmoError>;
//! }
//!
//! # async fn demo() -> Result<(), IstmoError> {
//! let rt = runtime! { plugins: [EchoClient] };
//! let echo = EchoClient::from_runtime(&rt)?;
//! let reply = echo.echo(EchoInput { text: "hi".into() }).await?;
//! # Ok(()) }
//! ```
//!
//! # Crate layout
//!
//! - [`Runtime`] and the wire protocol re-exported from
//!   [`istmo_core`](https://docs.rs/istmo-core).
//! - Proc-macros [`plugin`](macro@plugin), [`message`](macro@message),
//!   [`service`](macro@service), [`worker`](macro@worker),
//!   [`mobile_app`](macro@mobile_app), [`stream`](macro@stream) and the
//!   [`runtime!`](macro@runtime) wiring macro from
//!   [`istmo_macros`](https://docs.rs/istmo-macros).
//! - [`plugins`] — first-party plugin surfaces (lifecycle, deep links,
//!   permissions, notifications, ad slots, safe-area publisher, …).
//! - `istmo::android` / `istmo::ios` — platform transport crates,
//!   pulled in by target cfg. Not present on desktop targets. See
//!   [`istmo-android`](https://docs.rs/istmo-android) and
//!   [`istmo-ios`](https://docs.rs/istmo-ios).
//! - [`bincode`] — the encoding primitives used on the wire, re-exported
//!   so downstream crates don't need a direct `bincode` dependency.
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
//! All frames cross the FFI boundary as opaque bincode-encoded byte
//! payloads. The current wire version is [`PROTOCOL_VERSION`]; runtimes
//! reject frames from mismatched peers.
//!
//! # License
//!
//! Dual-licensed under MIT or Apache-2.0.

#![doc(html_root_url = "https://docs.rs/istmo")]

pub use istmo_core::*;
pub use istmo_macros::{message, mobile_app, plugin, runtime, service, stream, worker};

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

/// Wire-encoding primitives re-exported from [`bincode`](::bincode).
///
/// Message types derive [`Encode`](bincode::Encode) and
/// [`Decode`](bincode::Decode) via [`#[message]`](macro@message);
/// downstream crates rarely need to touch this module directly, but it
/// is available so users don't have to pin a matching `bincode`
/// version.
pub mod bincode {
    pub use ::bincode::error;
    pub use ::bincode::{Decode, Encode, config, decode_from_slice, encode_to_vec};
}
