//! Entry-point re-exports consumed by the `istmo::runtime!` macro on Apple
//! targets.
//!
//! Same rationale as the Android sibling: `--gc-sections` on a release build
//! strips `extern "C"` symbols that are only referenced through the linker,
//! not from Rust code. Re-exporting them at the downstream cdylib's crate
//! root keeps them alive.
//!
//! Users never write `@_cdecl` shims or `no_mangle` themselves.

pub use crate::{
    IstmoIosCallbacks, istmo_ios_shutdown, istmo_ios_start, istmo_ios_submit_call,
    istmo_ios_submit_early_latest, istmo_ios_submit_early_queue, istmo_ios_submit_event,
    istmo_ios_submit_response, istmo_ios_submit_stream_end,
};
