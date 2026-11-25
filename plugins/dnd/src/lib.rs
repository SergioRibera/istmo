//! Drag and drop between the app and other apps: files dragged in, and
//! text, links and files dragged out ([`DragAndDropClient::start_drag`]).
//!
//! ## Dropping into the app
//!
//! On Android (split screen, freeform windows, Chromebooks) and on
//! iPadOS, people drag files from another app onto this one. This plugin
//! listens for those drops on the app's window, copies each file into a
//! directory the app owns and hands the batch to Rust as one
//! [`DroppedFiles`], with where on the window it landed.
//!
//! ```no_run
//! use istmo_dnd::{DropInbox, DropOptions, DragAndDropClient};
//!
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! // Nothing listens until the app asks: dropping on the window does
//! // nothing before this.
//! DragAndDropClient::acquire()?
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
//! On desktop, [`register_desktop`] installs a [`DndPublisher`]. On
//! Wayland, where winit delivers no drops, it takes them from the
//! compositor for every window registered with `istmo-window` (through
//! the document portal inside Flatpak). Elsewhere the UI toolkit gets
//! them and the app forwards them with [`DndPublisher::publish_paths`],
//! so every platform ends in the same [`DroppedFiles`].
//!
//! ## Dragging out of the app
//!
//! When a press on something the app lets people drag turns into a drag,
//! the app calls [`DragAndDropClient::start_drag`] with what it carries
//! ([`DragItem`]: text, a link, files the app wrote). The system takes the
//! pointer from there and the call returns once the drag ends, saying
//! whether another app took it ([`DragResult`]). Android, iOS, Wayland
//! (Flatpak included), X11, macOS and Windows.
//!
//! iOS starts drags itself on its own long press
//! ([`DRAG_STARTS_ON_LONG_PRESS`]): there the app calls `start_drag` as
//! soon as a press lands on something draggable, and the drag begins if
//! the press becomes the system's lift.
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
pub const DND_PLUGIN_ID: &str = "istmo.dnd";

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
    /// Take dropped folders as entries of their own (MIME type
    /// `inode/directory`, size 0) instead of skipping them. Desktop only:
    /// phones and tablets drag files, never folders.
    pub folders: bool,
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
            folders: false,
        }
    }

    /// Leave out files larger than `bytes`. `0` keeps the default,
    /// [`DEFAULT_MAX_BYTES`].
    #[must_use]
    pub const fn max_bytes(mut self, bytes: u64) -> Self {
        self.max_bytes = bytes;
        self
    }

    /// Take dropped folders too ([`DropOptions::folders`]).
    #[must_use]
    pub const fn with_folders(mut self) -> Self {
        self.folders = true;
        self
    }
}

/// Why the window does not take drops. Sent by position: new variants
/// go at the end.
///
/// Generated clients return it inside
/// [`istmo_core::IstmoError::PluginError`]; get it back with
/// [`DndError::try_from_istmo`] (or `DndError::from`).
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DndError {
    /// Not on this platform or window: drops that come through the UI
    /// toolkit instead, or a window kind no backend drags from.
    Unsupported,
    /// The native side failed; the text says why, for logs.
    Backend(String),
    /// A drag was asked for with no press in progress to drag from.
    NotPressed,
}

impl fmt::Display for DndError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported => f.write_str("drag and drop is not available here"),
            Self::Backend(message) => write!(f, "drag and drop backend: {message}"),
            Self::NotPressed => f.write_str("no press in progress to drag from"),
        }
    }
}

impl std::error::Error for DndError {}

impl DndError {
    /// The `DndError` a client call failed with, or the transport error
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

impl From<istmo_core::IstmoError> for DndError {
    /// Lossy: transport failures (and undecodable plugin errors) become
    /// [`DndError::Backend`]. Use [`DndError::try_from_istmo`] to tell
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
    /// The window it landed on, as registered with `istmo-window`, when
    /// the platform says (Wayland on desktop).
    pub window_id: Option<u64>,
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
    /// Items in the drag, files or not; 0 when the platform does not say
    /// before the drop (Wayland).
    pub count: u32,
    /// Where the pointer or finger is, when the platform says; updated a
    /// few dozen times a second at most.
    pub position: Option<DropPoint>,
    /// The window under it, as registered with `istmo-window`, when the
    /// platform says (Wayland on desktop).
    pub window_id: Option<u64>,
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

/// Whether the system starts drags itself on a long press (iOS).
///
/// The app then calls [`DragAndDropClient::start_drag`] when a press lands on
/// something draggable, and the drag begins if the press becomes the
/// system's lift. Elsewhere the app starts the drag when the press moves.
pub const DRAG_STARTS_ON_LONG_PRESS: bool = cfg!(target_os = "ios");

/// Something dragged out of the app. Sent by position: new kinds go at
/// the end.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DragItem {
    /// Plain text.
    Text(String),
    /// A link (`https://…`).
    Url(String),
    /// A file the app wrote: it must exist until the drag ends.
    File(DragFile),
}

/// A file dragged out ([`DragItem::File`]).
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DragFile {
    /// Absolute path of the file.
    pub path: String,
    /// MIME type, when known; guessed from the name otherwise.
    pub mime_type: Option<String>,
}

/// An image under the pointer while dragging.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DragImage {
    /// PNG bytes.
    pub png: Vec<u8>,
    /// Width to show it at, in logical pixels.
    pub width: u32,
    /// Height to show it at, in logical pixels.
    pub height: u32,
}

/// A drag out of the app ([`DragAndDropClient::start_drag`]).
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DragRequest {
    /// What the drag carries; another app takes whichever it reads.
    pub items: Vec<DragItem>,
    /// Shown under the pointer; the platform's own (a file icon) when
    /// absent.
    pub preview: Option<DragImage>,
    /// The window the press is on, as registered with `istmo-window`;
    /// desktop only, the only or first window when absent.
    pub window_id: Option<u64>,
}

impl DragRequest {
    /// A drag of `items`.
    #[must_use]
    pub fn new(items: impl IntoIterator<Item = DragItem>) -> Self {
        Self {
            items: items.into_iter().collect(),
            ..Self::default()
        }
    }
}

/// Directory (as path components, under the app's cache) for files the
/// app writes to drag them out: on Android only files there go out as
/// they are, any other is copied there first. See [`drag_out_dir`].
pub const DRAG_OUT_DIR: &str = "istmo-dnd/outgoing";

/// A new empty folder under [`DRAG_OUT_DIR`] to write one drag's files
/// in. Folders older than a day are removed on the way.
pub fn drag_out_dir() -> std::io::Result<std::path::PathBuf> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let root = istmo_core::path::cache_dir().join(DRAG_OUT_DIR);
    if let Ok(entries) = std::fs::read_dir(&root) {
        let day = std::time::Duration::from_secs(24 * 60 * 60);
        for entry in entries.flatten() {
            let old = entry
                .metadata()
                .and_then(|meta| meta.modified())
                .ok()
                .and_then(|modified| modified.elapsed().ok())
                .is_some_and(|age| age > day);
            if old {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_millis());
    let dir = root.join(format!("{stamp}-{}", NEXT.fetch_add(1, Ordering::Relaxed)));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// How a drag out of the app ended. Sent by position: new outcomes go at
/// the end.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DragResult {
    /// Another app (or another place of this one) took it.
    Dropped,
    /// Let go where nothing takes it, or cancelled.
    Cancelled,
}

/// The plugin trait, implemented by the native backends.
#[plugin(name = "istmo.dnd", crate = "::istmo_core")]
pub trait DragAndDrop {
    /// Start taking drops on the app's window. Calling it again replaces
    /// the options. Drops arrive on [`DropInbox::stream`].
    async fn listen(&self, options: DropOptions) -> Result<(), DndError>;

    /// Stop taking drops: drags over the window are refused again.
    async fn stop(&self) -> Result<(), DndError>;

    // macOS starts the drag with a synchronous hop to the main thread.
    #[istmo_macros::blocking]
    /// Drag `request` out of the window, from the press in progress.
    /// Returns when the drag ends: dropped on another app or cancelled.
    /// Call it when the press turns into a drag (moved past a threshold),
    /// or, where [`DRAG_STARTS_ON_LONG_PRESS`], when the press lands.
    async fn start_drag(&self, request: DragRequest) -> Result<DragResult, DndError>;
}

/// Desktop host of the plugin: keeps what `listen` asked for.
///
/// On X11, Windows and macOS the UI toolkit delivers drops; the app
/// hands them to [`DndPublisher::publish_paths`], which applies these
/// options. On Wayland, where winit has no drops, the publisher takes
/// them from the compositor itself. Built by [`DndPublisher::install`];
/// registering it alone only makes `listen` answer.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
#[derive(Debug, Clone, Default)]
pub struct DesktopDragAndDrop {
    listening: desktop::Listening,
    outgoing: desktop::Outgoing,
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
impl DragAndDrop for DesktopDragAndDrop {
    async fn listen(&self, options: DropOptions) -> Result<(), DndError> {
        self.listening.set(Some(options));
        Ok(())
    }

    async fn stop(&self) -> Result<(), DndError> {
        self.listening.set(None);
        Ok(())
    }

    async fn start_drag(&self, request: DragRequest) -> Result<DragResult, DndError> {
        self.outgoing.start(request).await
    }
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
mod desktop;

#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub use desktop::DndPublisher;

/// Install the desktop side on `runtime`: [`DndPublisher::install`].
///
/// Needed on desktop, where nothing else answers `listen` and the call
/// would wait forever. Mobile builds get their native backend from the
/// plugin registry instead, so this exists only off Android and iOS.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn register_desktop(
    runtime: &std::sync::Arc<istmo_core::Runtime>,
) -> std::sync::Arc<DndPublisher> {
    DndPublisher::install(runtime)
}
