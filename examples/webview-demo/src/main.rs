//! Desktop istmo demo: a small browser on `istmo-webview`, a bridged
//! local page and Microsoft sign-in. See `README.md`.
//!
//! `cargo run -p webview-demo`

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
mod browser;

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    browser::run()
}

/// The demo is desktop-only; other targets build an empty binary.
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
const fn main() {}
