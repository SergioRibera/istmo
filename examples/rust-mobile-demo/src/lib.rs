//! Full-Rust mobile demo.
//!
//! Everything the user sees is Rust code driving `egui`. The demo has one
//! button — "Sign in with Google" — and one status line. Tapping the button
//! chains three istmo plugins:
//!
//! 1. `PermissionsClient` requests `POST_NOTIFICATIONS` on Android 13+ so
//!    the follow-up notification can actually show.
//! 2. `SignInClient` (`istmo.google_sign_in`) runs the Credential Manager
//!    flow with `acquire_with(SignInConfig)` and returns an owned account.
//! 3. `NotificationsClient` posts a local notification announcing the
//!    signed-in user.
//!
//! Every plugin is native-hosted (Kotlin backends live in the demo's
//! Android module). The Rust cdylib is otherwise self-contained: no
//! `Java_*` symbols, no `JNIEnv`. `istmo::runtime!` supplies the JNI
//! trampolines from `istmo-android::entrypoint`.
//!
//! On non-android targets the crate compiles to a stub library
//! (`android_main` is `cfg`-gated) so `cargo check --workspace` on a Linux
//! dev host stays green without an NDK toolchain.

use istmo::plugins::{NotificationsClient, PermissionsClient};
use istmo_plugins::google_sign_in::SignInClient;

// Register the three plugins the demo consumes. `SignIn` requires
// `acquire_with(config)` — the macro emits `SignInClient::from_runtime_with`
// but the plugin declaration in `plugins:` is the same shape as any other
// client. Native side must ship dispatchers for all three plugin ids
// (`istmo.google_sign_in`, `istmo.notifications`, `istmo.permissions`).
istmo::runtime!(
    plugins: [SignInClient, PermissionsClient, NotificationsClient],
);

#[cfg(target_os = "android")]
mod android;

#[cfg(target_os = "android")]
pub use android::android_main;
