//! Everything attached to the app's icon where the system launches it,
//! for [`istmo`](https://docs.rs/istmo).
//!
//! * **Actions**: entries in the icon's menu. Android app shortcuts,
//!   iOS Home Screen quick actions, Windows Jump List tasks and the
//!   macOS Dock menu. Static ones come from `[[app.launcher_action]]`
//!   in `istmo.toml`; [`LauncherActions::set_actions`] replaces the
//!   dynamic ones.
//! * **Badge**: a count or a short label on the icon ([`Badge`]).
//! * **Progress**: a progress bar on the icon ([`Progress`]).
//! * **Attention**: bounce, flash or mark urgent ([`Attention`]).
//! * **Recent documents**: the Jump List and Dock menu "Recent" lists.
//!
//! # Activation is a deep link
//!
//! An action carries a URL. Activating it publishes a `DeepLink` on the
//! `istmo.deeplinks` queue (`istmo_plugins::DeepLinks`), so the app
//! routes launcher actions with the router it uses for links. Where the
//! platform names the action (Android, iOS, macOS) the link's `source` is
//! `launcher-action:<id>` ([`action_source`]); on Windows and Linux the URL arrives as a
//! command-line argument, so `source` is the argv source (`launch` /
//! `running`) and apps that care route by URL.
//!
//! # Backends
//!
//! * Android: `ShortcutManager` (`native/android`), plus the
//!   `IstmoLauncherActionActivity` trampoline that turns a shortcut
//!   into a deep link.
//! * iOS: `UIApplication.shortcutItems` and the badge
//!   (`native/ios`); the app forwards quick actions with
//!   `IstmoLauncherActions.perform(_:)`.
//! * Desktop: [`DesktopLauncherActions`].
//!
//! [`LauncherActions::capabilities`] tells what the running platform
//! supports; everything else answers [`LauncherError::Unsupported`].

#![doc(html_root_url = "https://docs.rs/istmo-launcher-actions")]

use istmo_macros::{message, plugin};

#[cfg(not(any(target_os = "android", target_os = "ios", target_arch = "wasm32")))]
mod desktop;
mod validate;

#[cfg(not(any(target_os = "android", target_os = "ios", target_arch = "wasm32")))]
pub use desktop::DesktopLauncherActions;

/// Wire identifier of the plugin.
pub const LAUNCHER_ACTIONS_PLUGIN_ID: &str = "istmo.launcher_actions";

/// Prefix of `DeepLink::source` for links that come from an action.
pub const ACTION_SOURCE_PREFIX: &str = "launcher-action:";

/// Longest action id, in bytes.
pub const MAX_ID_LEN: usize = 64;

/// Longest title, in characters.
pub const MAX_TITLE_CHARS: usize = 128;

/// Longest subtitle, in characters.
pub const MAX_SUBTITLE_CHARS: usize = 256;

/// Longest action URL, in bytes.
pub const MAX_URL_LEN: usize = 2048;

/// Largest PNG icon, in bytes.
pub const MAX_ICON_BYTES: usize = 1 << 20;

/// Most dynamic actions the Windows backend lists (Jump List tasks).
pub const WINDOWS_MAX_ACTIONS: u32 = 10;

/// Most dynamic actions the macOS backend lists (Dock menu items).
pub const MACOS_MAX_ACTIONS: u32 = 16;

/// `DeepLink::source` of a link published by the action `id`.
#[must_use]
pub fn action_source(id: &str) -> String {
    format!("{ACTION_SOURCE_PREFIX}{id}")
}

/// The action id of a `DeepLink::source`, when the link came from an
/// action whose platform reports the id.
#[must_use]
pub fn action_id_from_source(source: &str) -> Option<&str> {
    source
        .strip_prefix(ACTION_SOURCE_PREFIX)
        .filter(|id| !id.is_empty())
}

/// One entry of the icon's menu.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LauncherAction {
    /// `1..=64` bytes of `[A-Za-z0-9._-]`, unique in a set.
    pub id: String,
    /// Short label (Android `shortLabel`, the Jump List task name).
    pub title: String,
    /// Android `longLabel`, iOS subtitle, Windows tooltip.
    pub subtitle: Option<String>,
    /// What activating the action opens: a URL with an RFC 3986 scheme,
    /// normally the app's own (`[[app.url_scheme]]`). `http(s)` URLs are
    /// refused: opening the browser is the opener's job.
    pub url: String,
    /// Icon of the entry, when the platform shows one.
    pub icon: Option<ActionIcon>,
}

/// Icon of a [`LauncherAction`].
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ActionIcon {
    /// Platform symbol name: an SF Symbol on Apple platforms, ignored
    /// elsewhere.
    Symbol(String),
    /// PNG bytes: an Android bitmap icon, a Windows `.ico` written to
    /// the plugin's cache directory, a macOS `NSImage`. Ignored on iOS.
    Png(Vec<u8>),
}

/// How a platform shows a badge.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum BadgeSupport {
    /// No badge.
    #[default]
    Unsupported,
    /// Numbers only ([`Badge::Count`]).
    Count,
    /// Numbers and short labels ([`Badge::Count`], [`Badge::Label`]).
    Label,
}

/// What the running platform supports.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent feature flags of a wire type"
)]
pub struct Capabilities {
    /// [`LauncherActions::set_actions`] works.
    pub dynamic_actions: bool,
    /// Most dynamic actions shown; `0` when actions are unsupported.
    pub max_actions: u32,
    /// [`LauncherActions::request_pin`] works.
    pub pin: bool,
    /// How [`LauncherActions::set_badge`] shows.
    pub badge: BadgeSupport,
    /// [`LauncherActions::set_progress`] works.
    pub progress: bool,
    /// [`LauncherActions::request_attention`] works.
    pub attention: bool,
    /// [`LauncherActions::add_recent_document`] works.
    pub recent_documents: bool,
}

/// The badge on the icon.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Badge {
    /// No badge.
    Clear,
    /// A number (`0` clears on most platforms).
    Count(u32),
    /// A short text; only where [`BadgeSupport::Label`].
    Label(String),
}

/// The progress bar on the icon. Fractions are clamped to `0.0..=1.0`.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub enum Progress {
    /// No progress bar.
    Clear,
    /// Busy, amount unknown.
    Indeterminate,
    /// Running at the given fraction.
    Normal(f32),
    /// Paused at the given fraction.
    Paused(f32),
    /// Failed at the given fraction.
    Error(f32),
}

/// How insistently to ask for the user's attention.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Attention {
    /// Once: one bounce, a few flashes.
    Informational,
    /// Until the user switches to the app.
    Critical,
    /// Stop a request still running.
    Cancel,
}

/// Result of [`LauncherActions::request_pin`].
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PinOutcome {
    /// The launcher asked the user; the answer is not reported.
    Requested,
    /// A shortcut with this id is pinned already (it was updated).
    AlreadyPinned,
}

/// What is wrong with one action.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ActionProblem {
    /// Id of the offending action, as given.
    pub id: String,
    /// What is wrong with it.
    pub reason: String,
}

/// Why a launcher call failed.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum LauncherError {
    /// The platform (or this backend) does not do that.
    Unsupported,
    /// An action is malformed (or two share an id).
    InvalidAction(ActionProblem),
    /// More actions than the platform shows; carries the most accepted.
    TooMany(u32),
    /// Android `ShortcutManager` rate limiting is active.
    RateLimited,
    /// The user did not allow it (iOS badge without notification
    /// permission).
    NotPermitted,
    /// A taskbar call with no window registered with `istmo-window`.
    NoWindow,
    /// Any other platform failure, with its message.
    Backend(String),
}

impl std::fmt::Display for LauncherError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported => f.write_str("not supported on this platform"),
            Self::InvalidAction(problem) => {
                write!(
                    f,
                    "invalid launcher action {:?}: {}",
                    problem.id, problem.reason
                )
            }
            Self::TooMany(max) => write!(f, "too many launcher actions (at most {max})"),
            Self::RateLimited => f.write_str("launcher shortcut updates are rate limited"),
            Self::NotPermitted => f.write_str("not permitted by the user"),
            Self::NoWindow => f.write_str("no window registered with istmo-window"),
            Self::Backend(msg) => write!(f, "launcher backend error: {msg}"),
        }
    }
}

impl std::error::Error for LauncherError {}

/// Recover the typed error from a failed client call.
impl From<istmo_core::IstmoError> for LauncherError {
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

/// The plugin trait.
///
/// `remote = false`: these calls change the app's own launcher presence,
/// which a `:remote` process has no business doing.
#[plugin(
    name = "istmo.launcher_actions",
    crate = "::istmo_core",
    remote = false
)]
pub trait LauncherActions {
    /// What this platform supports.
    async fn capabilities(&self) -> Capabilities;

    /// Replace the dynamic actions (static ones from `istmo.toml` stay).
    async fn set_actions(&self, actions: Vec<LauncherAction>) -> Result<(), LauncherError>;

    /// Remove every dynamic action.
    async fn clear_actions(&self) -> Result<(), LauncherError>;

    /// The user did what the action `id` does from inside the app
    /// (Android ranks shortcuts by it). A no-op elsewhere.
    async fn report_used(&self, id: String) -> Result<(), LauncherError>;

    /// Ask the launcher to pin `action` to the home screen.
    async fn request_pin(&self, action: LauncherAction) -> Result<PinOutcome, LauncherError>;

    /// Set or clear the badge.
    async fn set_badge(&self, badge: Badge) -> Result<(), LauncherError>;

    /// Set or clear the progress bar.
    async fn set_progress(&self, progress: Progress) -> Result<(), LauncherError>;

    /// Ask for (or stop asking for) the user's attention.
    async fn request_attention(&self, attention: Attention) -> Result<(), LauncherError>;

    /// Add the file at `path` to the recent documents.
    async fn add_recent_document(&self, path: String) -> Result<(), LauncherError>;

    /// Empty the recent documents.
    async fn clear_recent_documents(&self) -> Result<(), LauncherError>;
}

impl LauncherActionsClient {
    /// [`Self::set_actions`] after [`LauncherAction::validate_set`], so a
    /// malformed set never leaves the process; errors come back typed.
    pub async fn set_actions_checked(
        &self,
        actions: Vec<LauncherAction>,
    ) -> Result<(), LauncherError> {
        LauncherAction::validate_set(&actions)?;
        self.set_actions(actions).await.map_err(LauncherError::from)
    }

    /// [`Self::request_pin`] after [`LauncherAction::validate`].
    pub async fn request_pin_checked(
        &self,
        action: LauncherAction,
    ) -> Result<PinOutcome, LauncherError> {
        action.validate()?;
        self.request_pin(action).await.map_err(LauncherError::from)
    }
}
