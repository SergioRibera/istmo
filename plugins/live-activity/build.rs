//! Publishes native deps + the `LiveActivity` `Contract` through Cargo's
//! cross-crate metadata channel. Trait declaration in `src/lib.rs` is the
//! single source of truth; `istmo_build::emit` derives the contract from
//! it at build time.

fn main() {
    istmo_build::emit();
}
