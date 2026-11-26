//! Wire types (`#[istmo::message]`), shared by every host and client.

mod caps;
mod config;
mod consent;
mod error;
mod meta;
mod source;

pub use caps::*;
pub use config::*;
pub use consent::*;
pub use error::*;
pub use meta::*;
pub use source::*;
