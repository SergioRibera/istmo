//! Contract metadata and Kotlin/Swift code generators for istmo plugins.
//!
//! This crate is pure — no I/O, no macros. It defines the [`Contract`] data
//! model produced by `#[istmo::plugin]` at macro-expand time and consumed by
//! `build.rs` scripts to emit native glue.
//!
//! At **M1** the generators emit interface / protocol declarations only.
//! The concrete implementation classes that call the istmo runtime land
//! together with the Android and iOS bindings in later milestones.

pub mod contract;
pub mod entitlements;
pub mod ios;
pub mod kotlin;
pub mod kotlin_host;
pub mod kotlin_types;
pub mod native_deps;
pub mod rust;
pub mod service;
pub mod swift;
pub mod swift_host;
pub mod swift_types;
pub mod worker;

pub use crate::contract::{
    Arg, Contract, EnumDef, EnumVariant, Field, Method, MethodKind, StructDef, TypeDef, TypeRef,
};
pub use crate::entitlements::{EntitlementValue, IosEntitlements};
pub use crate::ios::{
    BackgroundKind, ContinuousMode, IosBackgroundArtifacts, IosBackgroundContract,
    generate_ios_background, required_entitlements,
};
pub use crate::kotlin::generate_kotlin;
pub use crate::kotlin_host::generate_kotlin_host;
pub use crate::kotlin_types::{generate_kotlin_codecs, generate_kotlin_types};
pub use crate::native_deps::{
    GradleCoord, GradleDep, GradleKey, GradleScope, NativeDeps, SwiftPackageDep, VersionConflict,
};
pub use crate::rust::generate_rust_types;
pub use crate::service::{AndroidServiceArtifacts, ServiceContract, generate_android_service};
pub use crate::swift::{generate_swift, generate_swift_client};
pub use crate::swift_host::generate_swift_host;
pub use crate::swift_types::{generate_swift_codecs, generate_swift_types};
pub use crate::worker::{WorkerContract, generate_android_worker};
