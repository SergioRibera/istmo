//! Full-Rust mobile demo — Android + iOS from the same Cargo package.
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
//! Every plugin is native-hosted (Kotlin / Swift backends live in the
//! demo's `android/` and `ios/` directories). The Rust code is otherwise
//! self-contained: no `Java_*` symbols, no `JNIEnv`, no `@_cdecl` — the
//! transport trampolines fall out of `istmo::runtime!`, and the app entry
//! point falls out of `#[istmo::mobile_app]`. **No `#[cfg(target_os = ...)]`
//! anywhere in this crate.**

use istmo::plugins::{NotificationsClient, PermissionsClient, SafeArea};
use istmo_plugins::admob::AdMobClient;
use istmo_plugins::google_sign_in::SignInClient;

istmo::runtime!(
    plugins: [SignInClient, PermissionsClient, NotificationsClient, AdMobClient, SafeArea],
);

// `app` runs on any target that has an eframe backend. The
// `#[istmo::mobile_app]` attribute emits the platform-specific entry
// points (`android_main` on Android, `istmo_run_ios` on iOS family).
#[cfg(any(
    target_os = "android",
    target_os = "ios",
    target_os = "tvos",
    target_os = "watchos",
    target_os = "visionos",
))]
mod app;
