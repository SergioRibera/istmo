//! Start the app when the user logs in, for [`istmo`](https://docs.rs/istmo),
//! and tell the app whether this launch was such a start.
//!
//! Desktop only. Android has no "start my UI at login" and iOS and the
//! web have nothing; those targets register [`UnavailableAutostart`],
//! which reports [`AutostartState::Unsupported`].
//!
//! | Platform | Registration |
//! |---|---|
//! | macOS 13+, inside an `.app` | `SMAppService.mainApp` |
//! | macOS < 13, or a bare binary | `~/Library/LaunchAgents/<id>.istmo-autostart.plist` |
//! | Windows, MSIX | `StartupTask` (`[app.autostart] windows_task_id`) |
//! | Windows, unpackaged | `HKCU\…\CurrentVersion\Run` value |
//! | Linux, Flatpak | `org.freedesktop.portal.Background` |
//! | Linux | `$XDG_CONFIG_HOME/autostart/<id>.desktop` |
//!
//! Nothing enables autostart on its own: [`Autostart::enable`] is meant
//! to run from a user action (a "Start at login" switch), and a start the
//! user turned off in the system settings stays off — `enable` then opens
//! the settings page instead of flipping it back.
//!
//! # Was this a login start?
//!
//! [`AutostartPublisher::install`] publishes a [`LaunchReason`] on the
//! latest-value lane [`LAUNCH_CHANNEL`]; read it with
//! [`LaunchReasonClient`]. The OS is asked first (macOS login-item Apple
//! Event, MSIX activation kind, the systemd `@autostart.service` unit);
//! the `[app.autostart] marker` argument is only used where the OS says
//! nothing.
//!
//! ```rust,ignore
//! let config = istmo_autostart::autostart_config!();
//! // Strip the marker before anything else parses the command line.
//! let args = AutostartPublisher::install(&runtime, &config, std::env::args_os().skip(1))?;
//! runtime.register_host(AutostartHost::new(DesktopAutostart::new(config)));
//! ```

#![doc(html_root_url = "https://docs.rs/istmo-autostart")]

use std::fmt;

use istmo_macros::{message, plugin};

mod config;
#[cfg(not(any(
    target_os = "android",
    target_os = "ios",
    all(target_arch = "wasm32", target_os = "unknown")
)))]
mod desktop;
mod launch;

pub use config::AutostartConfig;
#[cfg(not(any(
    target_os = "android",
    target_os = "ios",
    all(target_arch = "wasm32", target_os = "unknown")
)))]
pub use desktop::{AutostartPublisher, DesktopAutostart, StrippedArgs};
pub use launch::{LaunchReasonClient, LaunchReasonStream};

/// Wire identifier for the autostart plugin.
pub const AUTOSTART_PLUGIN_ID: &str = "istmo.autostart";

/// Latest-value early-event lane the [`LaunchReason`] of this process is
/// published on.
pub const LAUNCH_CHANNEL: &str = "istmo.autostart.launch";

/// `uap5:StartupTask` `TaskId` used when `[app.autostart] windows_task_id`
/// is not set.
pub const DEFAULT_WINDOWS_TASK_ID: &str = "IstmoAutostart";

/// Whether the app starts at login.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AutostartState {
    /// Registered and allowed to run.
    Enabled,
    /// Not registered.
    Disabled,
    /// macOS: registered, waiting for the user in System Settings (also
    /// what macOS reports once the user turned the login item off).
    RequiresApproval,
    /// Registered, but the user turned it off in the system settings.
    /// Only the user turns it back on; [`Autostart::enable`] opens the
    /// settings page.
    DisabledByUser,
    /// An administrator policy keeps it off.
    DisabledByPolicy,
    /// An administrator policy keeps it on; [`Autostart::disable`] cannot
    /// turn it off.
    EnabledByPolicy,
    /// This platform (or this build) has no autostart.
    Unsupported,
}

impl AutostartState {
    /// Whether the app will start at the next login.
    #[must_use]
    pub const fn is_enabled(self) -> bool {
        matches!(self, Self::Enabled | Self::EnabledByPolicy)
    }
}

/// What [`Autostart::enable`] registers.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct AutostartOptions {
    /// Extra arguments after the executable. The `[app.autostart]
    /// marker` is added by the plugin. Ignored by `SMAppService` and
    /// `StartupTask`, which start the app without arguments.
    pub args: Vec<String>,
}

/// Why an autostart call failed.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AutostartError {
    /// No autostart on this platform, or not for this kind of install.
    Unsupported,
    /// The system or the user refused (portal dialog, `StartupTask`
    /// prompt).
    Denied,
    /// Any other platform failure, with its message.
    Backend(String),
}

impl fmt::Display for AutostartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported => f.write_str("autostart is not supported here"),
            Self::Denied => f.write_str("autostart was refused"),
            Self::Backend(msg) => write!(f, "autostart backend error: {msg}"),
        }
    }
}

impl std::error::Error for AutostartError {}

/// Recover the typed error from a failed [`AutostartClient`] call.
impl From<istmo_core::IstmoError> for AutostartError {
    fn from(err: istmo_core::IstmoError) -> Self {
        match err {
            istmo_core::IstmoError::PluginError { bytes } => {
                istmo_core::codec::decode::<Self>(&bytes).map_or_else(
                    |e| Self::Backend(format!("decode plugin error payload: {e}")),
                    |(err, _)| err,
                )
            }
            other => Self::Backend(other.to_string()),
        }
    }
}

/// Why this process started.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LaunchReason {
    /// Started by the user (or anything else that is not a login start):
    /// this platform has a signal, and it was absent.
    User,
    /// Started at login.
    Login(Detection),
    /// No OS signal on this platform and no `[app.autostart] marker`.
    Unknown,
}

impl LaunchReason {
    /// Whether this was a login start.
    #[must_use]
    pub const fn is_login(self) -> bool {
        matches!(self, Self::Login(_))
    }
}

/// How a login start was recognised.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Detection {
    /// The OS said so.
    System,
    /// The `[app.autostart] marker` argument was on the command line.
    Marker,
}

/// The plugin trait.
#[plugin(name = "istmo.autostart", crate = "::istmo_core", remote = false)]
pub trait Autostart {
    /// Whether the app starts at login.
    // Files, the registry, D-Bus or `SMAppService`.
    #[istmo_macros::blocking]
    async fn status(&self) -> Result<AutostartState, AutostartError>;

    /// Start the app at login. Call it from a user action only. On
    /// [`AutostartState::DisabledByUser`] and
    /// [`AutostartState::RequiresApproval`] it opens the system settings
    /// page and returns the state unchanged.
    #[istmo_macros::blocking]
    async fn enable(&self, options: AutostartOptions) -> Result<AutostartState, AutostartError>;

    /// Stop starting the app at login.
    #[istmo_macros::blocking]
    async fn disable(&self) -> Result<AutostartState, AutostartError>;

    /// Open the system page listing login items: macOS
    /// `SMAppService.openSystemSettingsLoginItems`, Windows
    /// `ms-settings:startupapps`. [`AutostartError::Unsupported`] on
    /// Linux.
    #[istmo_macros::blocking]
    async fn open_settings(&self) -> Result<(), AutostartError>;
}

/// Host for targets without autostart (Android, iOS, the web):
/// [`Autostart::status`] reports [`AutostartState::Unsupported`], every
/// other call fails with [`AutostartError::Unsupported`].
///
/// ```rust,ignore
/// runtime.register_host(AutostartHost::new(UnavailableAutostart));
/// ```
#[derive(Debug, Clone, Copy, Default)]
pub struct UnavailableAutostart;

impl Autostart for UnavailableAutostart {
    async fn status(&self) -> Result<AutostartState, AutostartError> {
        Ok(AutostartState::Unsupported)
    }

    async fn enable(&self, _options: AutostartOptions) -> Result<AutostartState, AutostartError> {
        Err(AutostartError::Unsupported)
    }

    async fn disable(&self) -> Result<AutostartState, AutostartError> {
        Err(AutostartError::Unsupported)
    }

    async fn open_settings(&self) -> Result<(), AutostartError> {
        Err(AutostartError::Unsupported)
    }
}

/// [`AutostartConfig::from_build`] with the build-time settings of the app crate.
///
/// The values are
/// `ISTMO_APP_BUNDLE_ID`, `ISTMO_AUTOSTART_MARKER` and
/// `ISTMO_AUTOSTART_TASK_ID`, which `istmo-build` emits from
/// `istmo.toml`. Expand it in the app crate.
#[macro_export]
macro_rules! autostart_config {
    () => {
        $crate::AutostartConfig::from_build(
            ::core::option_env!("ISTMO_APP_BUNDLE_ID"),
            ::core::option_env!("ISTMO_AUTOSTART_MARKER"),
            ::core::option_env!("ISTMO_AUTOSTART_TASK_ID"),
        )
    };
}
