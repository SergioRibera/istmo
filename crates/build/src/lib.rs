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
pub mod kotlin;
pub mod swift;

pub use crate::contract::{Arg, Contract, Method, MethodKind, TypeRef};
pub use crate::kotlin::generate_kotlin;
pub use crate::swift::generate_swift;
