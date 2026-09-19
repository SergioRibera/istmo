//! Build-script helpers for the [`istmo`](https://docs.rs/istmo)
//! framework.
//!
//! Plugin authors call the generators in this crate from a
//! `build.rs` to emit matching Kotlin and Swift bindings, aggregate
//! native-side dependencies, and hand off metadata to consuming apps
//! through Cargo's build-script `DEP_*` env-var mechanism.
//!
//! # Typical usage
//!
//! Most plugin crates only need the one-line
//! [`manifest::emit`] entry point plus [`extract_contract`] driven by
//! the `istmo.toml` manifest.
//!
//! ```ignore
//! fn main() {
//!     istmo_build::emit();
//! }
//! ```
//!
//! Downstream apps aggregate every plugin's contract and native-side
//! wiring by calling [`collect_dep_contracts`] +
//! [`collect_dep_native_deps`] plus [`emit_wiring_env`] so the
//! [`istmo::runtime!`](../../istmo_macros/macro.runtime.html) macro can
//! pick them up transparently.
//!
//! # Module map
//!
//! - [`contract`] — the [`Contract`] IR shared by every generator.
//! - [`extract`] — parse a plugin crate's `src/lib.rs` into a
//!   [`Contract`].
//! - [`manifest`] — parse and emit `istmo.toml` metadata.
//! - [`handover`] — hex-encoded metadata piped between plugin
//!   `build.rs`es and consuming apps via `DEP_*` env vars.
//! - [`kotlin`] / [`kotlin_client`] / [`kotlin_host`] / [`kotlin_types`]
//!   — Kotlin code generators.
//! - [`swift`] / [`swift_host`] / [`swift_types`] — Swift generators.
//! - [`rust`] — Rust-side type generators (used by the plugin schema
//!   crate).
//! - [`native_deps`] — Gradle / SwiftPM dependency aggregation.
//! - [`ios`] — iOS background mode + entitlements contract.
//! - [`service`] / [`worker`] — Android service / worker artefacts.
//! - [`desktop`] — systemd unit, launchd plist, Windows sc.exe and
//!   XDG `.desktop` generators.
//! - [`entitlements`] — Apple entitlements plist builder.

#![doc(html_root_url = "https://docs.rs/istmo-build")]

pub mod contract;
pub mod desktop;
pub mod emit_app;
pub mod extract;
pub mod entitlements;
pub mod handover;
pub mod ios;
pub mod kotlin;
pub mod kotlin_client;
pub mod kotlin_host;
pub mod kotlin_types;
pub mod manifest;
pub mod native_deps;
pub mod plugin_registry;
pub mod rust;
pub mod service;
pub mod swift;
pub mod swift_host;
pub mod swift_types;
pub mod worker;

pub use crate::contract::{
    Arg, Contract, EnumDef, EnumVariant, Field, Method, MethodKind, StructDef, TypeDef, TypeRef,
};
pub use crate::desktop::{
    DesktopAppContract, DesktopServiceContract, RestartPolicy, ServiceScope, StartType,
    WindowsServiceArtifacts, generate_desktop_entry, generate_launchd_plist, generate_systemd_unit,
    generate_windows_service,
};
pub use crate::emit_app::{
    AppOpts, AppPluginOpts, Platform, Role, detect_android_package, detect_ios_app_dir, emit_app,
    emit_app_with,
};
pub use crate::entitlements::{EntitlementValue, IosEntitlements};
pub use crate::extract::{ExtractError, extract_contract};
pub use crate::handover::{
    CONTRACT_KEY, HandoverError, MANIFEST_KEY, NATIVE_DEPS_KEY, collect_dep_contracts,
    collect_dep_manifests, collect_dep_native_deps, deserialize_contract, deserialize_manifest,
    deserialize_native_deps, emit_contract, emit_manifest, emit_native_deps, serialize_contract,
    serialize_manifest, serialize_native_deps,
};
pub use crate::ios::{
    BackgroundKind, ContinuousMode, IosBackgroundArtifacts, IosBackgroundContract,
    generate_ios_background, required_entitlements,
};
pub use crate::kotlin::generate_kotlin;
pub use crate::kotlin_client::generate_kotlin_client;
pub use crate::kotlin_host::{generate_kotlin_codecs_interface, generate_kotlin_host};
pub use crate::kotlin_types::{generate_kotlin_codecs, generate_kotlin_types};
pub use crate::manifest::{
    Deployment, Manifest, ManifestError, PluginEntry, RemoteOverride, ResolvedWiring, emit,
    emit_from, emit_manifest_metadata, emit_manifest_metadata_with_contract, emit_wiring_env,
    emit_with, resolve_wiring,
};
pub use crate::native_deps::{
    GradleCoord, GradleDep, GradleKey, GradleScope, NativeDeps, SwiftPackageDep, VersionConflict,
};
pub use crate::plugin_registry::{
    RegistryEntry, generate_kotlin_plugin_registry, generate_swift_plugin_registry,
};
pub use crate::rust::generate_rust_types;
pub use crate::service::{AndroidServiceArtifacts, ServiceContract, generate_android_service};
pub use crate::swift::{generate_swift, generate_swift_client};
pub use crate::swift_host::generate_swift_host;
pub use crate::swift_types::{generate_swift_codecs, generate_swift_types};
pub use crate::worker::{WorkerContract, generate_android_worker};

