//! iOS demo staticlib — the frame protocol is the only crossing.
//!
//! One `Echo` trait, one `EchoImpl`, one `istmo::runtime!` invocation. Every
//! Swift ↔ Rust call goes through the C symbols `runtime!` re-exports from
//! `::istmo::ios::entrypoint`. No `@_cdecl` wrapper, no `Bincode` in the
//! plugin author's Cargo.toml.
//!
//! Kept intentionally smaller than `android-demo` — this demo's job is to
//! prove the iOS transport end-to-end. Cross-plugin round-trips (using
//! `PermissionsClient`, `AppLifecycle` etc. from inside a hosted method)
//! land after the platform plugins get iOS-side impls of their own.

use istmo::{message, plugin};

#[message]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EchoError {
    pub reason: String,
}

impl core::fmt::Display for EchoError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.reason)
    }
}

impl std::error::Error for EchoError {}

#[plugin(name = "dev.istmo.demo.ios.echo")]
pub trait Echo {
    async fn echo(&self, text: String) -> Result<String, EchoError>;
}

#[derive(Debug, Default)]
pub struct EchoImpl;

impl Echo for EchoImpl {
    async fn echo(&self, text: String) -> Result<String, EchoError> {
        if text.is_empty() {
            return Err(EchoError {
                reason: "empty input".to_owned(),
            });
        }
        Ok(format!("echo: {text}"))
    }
}

istmo::runtime!(
    hosts: [
        Echo => EchoImpl,
    ],
);
