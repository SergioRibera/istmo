//! Files dragged into the app from other apps.
//!
//! On Android (split screen, freeform windows, Chromebooks) and on
//! iPadOS, people drag files from another app onto this one. This plugin
//! listens for those drops on the app's window, copies each file into a
//! directory the app owns and hands the batch to Rust as one
//! [`DroppedFiles`], with where on the window it landed.
//!
//! ```no_run
//! use istmo_drop::{DropInbox, DropOptions, FileDropClient};
//!
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! // Nothing listens until the app asks: dropping on the window does
//! // nothing before this.
//! FileDropClient::acquire()?
//!     .listen(DropOptions::accepting(["application/pdf", "text/*"]))
//!     .await?;
//! let drops = DropInbox::acquire()?.stream();
//! while let Ok(dropped) = drops.recv_async().await {
//!     for file in &dropped.files {
//!         println!("{} at {}", file.display_name, file.path);
//!     }
//!     // The copies are the app's: delete them once read.
//!     DropInbox::cleanup(&dropped)?;
//! }
//! # Ok(()) }
//! ```
//!
//! Desktop windows get drops from their UI toolkit (winit, Freya, …),
//! not from this plugin. [`DesktopFileDrop`] answers `listen` there, and
//! an app that wants one code path forwards its toolkit's drops with
//! [`DropInbox::publish`].
//!
//! Declare the plugin in `istmo::runtime!` and add the crate: the
//! Android and iOS backends register themselves through the generated
//! plugin registry.

use std::fmt;

use istmo_macros::{message, plugin};

mod inbox;

pub use inbox::{
    DRAG_CHANNEL, DROP_CHANNEL, DROP_DIR, DROP_QUEUE_CAPACITY, DragStream, DropInbox, DropStream,
};

/// Wire identifier of the plugin.
pub const DROP_PLUGIN_ID: &str = "istmo.drop";

/// Largest file copied when [`DropOptions::max_bytes`] is 0.
pub const DEFAULT_MAX_BYTES: u64 = 512 * 1024 * 1024;

/// What the window takes.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DropOptions {
    /// MIME types the window takes (`application/pdf`, `image/*`).
    /// Empty takes any file. A drag carrying none of them is refused
    /// before it is dropped: the system shows it cannot go here.
    pub mime_types: Vec<String>,
    /// Largest file copied, in bytes; 0 means [`DEFAULT_MAX_BYTES`].
    /// Larger ones are left out and listed in
    /// [`DroppedFiles::skipped`].
    pub max_bytes: u64,
}

impl DropOptions {
    /// Take any file, up to [`DEFAULT_MAX_BYTES`].
    #[must_use]
    pub fn any() -> Self {
        Self::default()
    }

    /// Take files of `mime_types` only (wildcards like `image/*` work).
    #[must_use]
    pub fn accepting<I, S>(mime_types: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            mime_types: mime_types.into_iter().map(Into::into).collect(),
            max_bytes: 0,
        }
    }

    /// Leave out files larger than `bytes`. `0` keeps the default,
    /// [`DEFAULT_MAX_BYTES`].
    #[must_use]
    pub const fn max_bytes(mut self, bytes: u64) -> Self {
        self.max_bytes = bytes;
        self
    }
}

/// Why the window does not take drops. Sent by position: new variants
/// go at the end.
///
/// Generated clients return it inside
/// [`istmo_core::IstmoError::PluginError`]; get it back with
/// [`DropError::try_from_istmo`] (or `DropError::from`).
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DropError {
    /// This platform delivers drops some other way (desktop: the UI
    /// toolkit) or not at all.
    Unsupported,
    /// The native side failed; the text says why, for logs.
    Backend(String),
}

impl fmt::Display for DropError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported => f.write_str("drops are not delivered by this plugin here"),
            Self::Backend(message) => write!(f, "drop backend: {message}"),
        }
    }
}

impl std::error::Error for DropError {}

impl DropError {
    /// The `DropError` a client call failed with, or the transport error
    /// itself when the failure was not the plugin's.
    pub fn try_from_istmo(err: istmo_core::IstmoError) -> Result<Self, istmo_core::IstmoError> {
        match err {
            istmo_core::IstmoError::PluginError { bytes } => {
                match istmo_core::codec::decode::<Self>(&bytes) {
                    Ok((domain, _)) => Ok(domain),
                    Err(_) => Err(istmo_core::IstmoError::PluginError { bytes }),
                }
            }
            other => Err(other),
        }
    }
}

impl From<istmo_core::IstmoError> for DropError {
    /// Lossy: transport failures (and undecodable plugin errors) become
    /// [`DropError::Backend`]. Use [`DropError::try_from_istmo`] to tell
    /// them apart.
    fn from(err: istmo_core::IstmoError) -> Self {
        Self::try_from_istmo(err).unwrap_or_else(|err| Self::Backend(err.to_string()))
    }
}

/// One drop: every file that came with it.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DroppedFiles {
    /// The files, copied into [`DROP_DIR`], in the order they were
    /// dragged.
    pub files: Vec<DroppedFile>,
    /// Files of the drop that were left out, and why.
    pub skipped: Vec<SkippedFile>,
    /// Where on the window the drop landed.
    pub position: Option<DropPoint>,
    /// Unix milliseconds when the drop happened.
    pub received_at_ms: Option<u64>,
}

impl DroppedFiles {
    /// Delete the copies of these files: [`DropInbox::cleanup`].
    pub fn cleanup(&self) -> std::io::Result<()> {
        DropInbox::cleanup(self)
    }
}

/// One dropped file, as a copy the app owns.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DroppedFile {
    /// Absolute path of the copy. Delete it with
    /// [`DropInbox::cleanup`] once read.
    pub path: String,
    /// Name the other app gave the file, for showing.
    pub display_name: String,
    /// MIME type the other app gave, when it gave one.
    pub mime_type: Option<String>,
    /// Size of the copy in bytes.
    pub size: u64,
}

/// A file of the drop that did not come through.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedFile {
    /// Name the other app gave the file.
    pub display_name: String,
    /// Why it was left out.
    pub reason: SkipReason,
}

/// Why a dropped file was left out. Sent by position: new reasons go
/// at the end.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// Larger than [`DropOptions::max_bytes`].
    TooLarge,
    /// The other app did not let it be read (no permission, gone, or a
    /// file of this app's own the other side named).
    Unreadable,
    /// Not a file: text, a link, or a folder.
    NotAFile,
    /// A file of a type [`DropOptions::mime_types`] does not take,
    /// dragged along with ones it does.
    WrongType,
}

/// What is being dragged over the window right now ([`DropInbox::drag_state`]).
///
/// For showing it: an overlay while files hover, a "receiving" note while
/// large ones are copied. Sent by position: new states go at the end.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum DragState {
    /// Nothing is being dragged over the window.
    #[default]
    Idle,
    /// Files the window takes are over it.
    Over(DragOver),
    /// They were dropped and this many items are being copied; a
    /// [`DroppedFiles`] follows.
    Receiving(u32),
}

/// A drag over the window ([`DragState::Over`]).
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct DragOver {
    /// Items in the drag, files or not.
    pub count: u32,
    /// Where the pointer or finger is, when the platform says; updated a
    /// few dozen times a second at most.
    pub position: Option<DropPoint>,
}

/// A point on the app window in logical pixels (Android dp, iOS points).
///
/// From the window's top-left corner. An Android app that is not
/// drawn edge to edge has its content below the status bar: subtract
/// that inset before hit-testing its views.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct DropPoint {
    /// From the left edge.
    pub x: f32,
    /// From the top edge.
    pub y: f32,
}

/// The plugin trait, implemented by the native backends.
#[plugin(name = "istmo.drop", crate = "::istmo_core")]
pub trait FileDrop {
    /// Start taking drops on the app's window. Calling it again replaces
    /// the options. Drops arrive on [`DropInbox::stream`].
    async fn listen(&self, options: DropOptions) -> Result<(), DropError>;

    /// Stop taking drops: drags over the window are refused again.
    async fn stop(&self) -> Result<(), DropError>;
}

/// Desktop backend, which takes the options and does nothing.
///
/// Desktop windows get drops from their UI toolkit, which the app
/// forwards with [`DropInbox::publish`] if it wants; this keeps the same
/// `listen` call working on every platform.
#[derive(Debug, Clone, Copy, Default)]
pub struct DesktopFileDrop;

impl FileDrop for DesktopFileDrop {
    async fn listen(&self, _options: DropOptions) -> Result<(), DropError> {
        Ok(())
    }

    async fn stop(&self) -> Result<(), DropError> {
        Ok(())
    }
}

/// Register [`DesktopFileDrop`] on `runtime`, so `listen` answers on desktop.
///
/// There, nothing else would, and the call would wait forever.
/// Mobile builds get their native backend from the plugin registry
/// instead, so this exists only off Android and iOS.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn register_desktop(runtime: &std::sync::Arc<istmo_core::Runtime>) {
    runtime.register_host(FileDropHost::new(DesktopFileDrop));
}
