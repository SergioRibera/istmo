//! Open URLs with the system, for [`istmo`](https://docs.rs/istmo).
//!
//! [`Opener::open`] hands a URL to whatever the platform uses for it:
//! the browser for `https:`, the mail app for `mailto:`, maps for
//! `geo:`, the file manager for `file://` folders.
//!
//! * Android — an `ACTION_VIEW` intent started from the app context.
//! * iOS — `UIApplication.shared.open`.
//! * Desktop — [`DesktopOpener`]: `xdg-open`, `open` or `start`.
//!
//! The generated `IstmoPluginRegistry` registers both mobile backends;
//! desktop apps register [`DesktopOpener`] themselves.

#![doc(html_root_url = "https://docs.rs/istmo-opener")]

#[cfg(feature = "codegen")]
pub mod codegen;

use istmo_macros::{message, plugin};

/// Wire identifier for the opener plugin.
pub const OPENER_PLUGIN_ID: &str = "istmo.opener";

/// Why a URL did not open.
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum OpenError {
    /// Nothing on the device handles this kind of URL.
    NoHandler,
    /// The URL could not be parsed by the platform.
    Invalid,
    /// Any other platform failure, with its message.
    Backend(String),
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoHandler => f.write_str("no app handles this url"),
            Self::Invalid => f.write_str("invalid url"),
            Self::Backend(msg) => write!(f, "opener backend error: {msg}"),
        }
    }
}

impl std::error::Error for OpenError {}

/// Recover the typed error from a failed [`OpenerClient::open`].
impl From<istmo_core::IstmoError> for OpenError {
    fn from(err: istmo_core::IstmoError) -> Self {
        match err {
            istmo_core::IstmoError::PluginError { bytes } => {
                bincode::decode_from_slice::<Self, _>(&bytes, bincode::config::standard())
                    .map_or_else(
                        |e| Self::Backend(format!("decode plugin error payload: {e}")),
                        |(err, _)| err,
                    )
            }
            other => Self::Backend(other.to_string()),
        }
    }
}

/// The plugin trait.
#[plugin(name = "istmo.opener", crate = "::istmo_core")]
pub trait Opener {
    /// Hand `url` to the system.
    async fn open(&self, url: String) -> Result<(), OpenError>;
}

/// Desktop backend over the system opener command.
#[derive(Debug, Clone, Copy, Default)]
pub struct DesktopOpener;

impl DesktopOpener {
    fn command(url: &str) -> std::process::Command {
        #[cfg(target_os = "macos")]
        {
            let mut cmd = std::process::Command::new("open");
            cmd.arg(url);
            cmd
        }
        #[cfg(target_os = "windows")]
        {
            let mut cmd = std::process::Command::new("cmd");
            cmd.args(["/C", "start", "", url]);
            cmd
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            let mut cmd = std::process::Command::new("xdg-open");
            cmd.arg(url);
            cmd
        }
    }
}

impl Opener for DesktopOpener {
    fn open(
        &self,
        url: String,
    ) -> impl std::future::Future<Output = Result<(), OpenError>> + Send + '_ {
        let result = if url.trim().is_empty() {
            Err(OpenError::Invalid)
        } else {
            Self::command(&url)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .map(drop)
                .map_err(|e| match e.kind() {
                    std::io::ErrorKind::NotFound => OpenError::NoHandler,
                    _ => OpenError::Backend(e.to_string()),
                })
        };
        std::future::ready(result)
    }
}
