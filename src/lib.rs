//! Facade crate for the istmo framework.
//!
//! This crate re-exports the pieces application and plugin authors interact
//! with, so consumers can depend on a single `istmo` crate without pinning
//! the versions of every internal crate.
//!
//! At **M0** only the transport, protocol and per-process runtime from
//! [`istmo_core`] are exposed. The `#[plugin]` macro, build.rs codegen and
//! platform-specific backends land in later milestones.

pub use istmo_core::*;
