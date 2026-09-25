//! Cross-platform file-picker plugin for
//! [`istmo`](https://docs.rs/istmo).
//!
//! Rust code sees a single async [`FilePicker`] trait; the native side
//! backs it with the Storage Access Framework on Android,
//! `UIDocumentPickerViewController` on iOS, `NSOpenPanel` / `NSSavePanel`
//! on macOS, `IFileOpenDialog` / `IFileSaveDialog` on Windows, and the
//! XDG Desktop Portal (`org.freedesktop.portal.FileChooser`) on Linux.
//!
//! # Handle-based ownership
//!
//! Native URIs, security-scoped URLs and Win32 paths never cross the
//! wire as strings. Every picked location is returned as a
//! `NativeHandle<FileRef>` (see [`istmo_core::NativeHandle`]) — the
//! native side keeps the underlying resource alive (retaining SAF
//! permissions, holding iOS security scope, etc.) until the handle
//! drops.
//!
//! # File descriptor bridging
//!
//! [`FilePicker::open_read`] and [`FilePicker::open_write`] return a
//! [`RawFileHandle`] carrying a POSIX fd (unix) or `HANDLE` (windows)
//! produced by the native side. The [`PickedFileReader`] and
//! [`PickedFileWriter`] wrappers adopt the raw handle into a
//! [`std::fs::File`] and implement [`std::io::Read`] / [`std::io::Write`]
//! / [`std::io::Seek`] so any consumer of the standard I/O traits works
//! transparently — including consumers that only accept `AsFd` /
//! `AsHandle`.
//!
//! # Path availability
//!
//! [`FilePicker::path`] returns `Ok(Some(path))` on desktop and on
//! iOS/macOS while security scope is active, and `Ok(None)` on Android
//! (SAF `content://` URIs are not filesystem paths since API 29). Callers
//! that need a real path must either fall back to
//! [`PickedFileReader`] + copy-to-cache or accept the missing path.
//!
//! # Deployment
//!
//! File descriptors are only meaningful inside the process that opened
//! them, so this plugin is pinned to `default_deployment = "local"` in
//! `istmo.toml`. Do not override to `remote`.
//!
//! # Auto-registration
//!
//! Backends need an `Activity` on Android and a top view controller on
//! iOS to present the picker, so bespoke construction is required.
//! Auto-registration is disabled — wire the backend manually in your
//! app's plugin registry.

#![doc(html_root_url = "https://docs.rs/istmo-file-picker")]

#[cfg(feature = "codegen")]
pub mod codegen;

mod fd_bridge;

pub use fd_bridge::{PickedFileReader, PickedFileWriter};

use istmo_core::NativeHandleId;
use istmo_macros::{message, owned, plugin};

/// Wire identifier for the file-picker plugin.
pub const FILE_PICKER_PLUGIN_ID: &str = "istmo.file_picker";

/// Uninhabited marker type for `NativeHandle<FileRef>` (see
/// [`istmo_core::NativeHandle`]).
///
/// Distinct from other handle markers so the type system rejects passing
/// a `NativeHandle<Credential>` where a `NativeHandle<FileRef>` is
/// expected.
#[non_exhaustive]
#[derive(Debug)]
pub enum FileRef {}

/// Filter describing what kinds of files the picker should offer.
///
/// Every backend understands a different subset — provide the widest set
/// you can and the native layer will pick whichever it supports:
///
/// * Android SAF → `mime_types`
/// * iOS / macOS `UIDocumentPickerViewController` → `uti_types`
/// * Windows `IFileOpenDialog`, Linux XDG portal, macOS `NSOpenPanel` →
///   `extensions`
///
/// An empty [`FileFilter`] means "any file".
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct FileFilter {
    /// MIME types such as `"image/*"` or `"application/pdf"`. Used by
    /// Android SAF and by the Linux XDG portal.
    pub mime_types: Vec<String>,
    /// Bare extensions without a leading dot, e.g. `"png"`, `"pdf"`.
    /// Used by Windows, Linux portal fallback, and non-UTI macOS panels.
    pub extensions: Vec<String>,
    /// Apple Uniform Type Identifiers such as `"public.image"`. Used by
    /// iOS and macOS.
    pub uti_types: Vec<String>,
}

/// Configuration for [`FilePicker::pick_file`] and
/// [`FilePicker::pick_files`].
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct PickConfig {
    /// Optional title shown in the picker chrome. Ignored by backends
    /// whose native picker does not expose a title (Android SAF).
    pub dialog_title: Option<String>,
    /// File-type filter. Empty = any file.
    pub filter: FileFilter,
    /// Best-effort starting location, either a `file://` URL or an
    /// absolute path. Backends may ignore this if they do not honour a
    /// starting-directory hint (Android SAF ignores it entirely).
    pub start_directory_hint: Option<String>,
    /// Only honoured by [`FilePicker::pick_files`] — allows selecting
    /// more than one file in a single invocation.
    pub allow_multiple: bool,
}

/// Configuration for [`FilePicker::save_file`].
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct SaveConfig {
    pub dialog_title: Option<String>,
    /// Filename pre-filled in the save dialog.
    pub suggested_name: Option<String>,
    /// MIME type used by Android SAF to create the destination. Ignored
    /// by desktop backends, which infer the type from the extension.
    pub mime_type: Option<String>,
    /// File-type filter. Empty = any file.
    pub filter: FileFilter,
    pub start_directory_hint: Option<String>,
    /// If `true`, the native dialog is asked to prompt before
    /// overwriting an existing file. Backends may ignore this and always
    /// prompt (Android/iOS) or never prompt.
    pub overwrite_prompt: bool,
}

/// A file picked (or newly created) by the user.
///
/// The wire form carries only a raw [`NativeHandleId`]; use
/// [`PickedFile::into_owned`] or annotate the client method with
/// `#[owned]` to receive an [`OwnedPickedFile`] whose handle drops
/// deterministically.
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PickedFile {
    /// Human-visible filename as reported by the native backend.
    pub display_name: String,
    /// MIME type when the backend can determine one. Desktop backends
    /// may leave this `None`.
    pub mime_type: Option<String>,
    /// File size in bytes when the backend can determine one without a
    /// full read. Streaming backends may leave this `None`.
    pub size: Option<u64>,
    /// Opaque handle to the underlying resource. Native side releases it
    /// when `Frame::ReleaseNativeHandle` arrives.
    #[handle(FileRef)]
    pub handle: NativeHandleId,
}

/// Cross-platform wrapper for a native file descriptor produced by
/// [`FilePicker::open_read`] / [`FilePicker::open_write`].
///
/// On unix, [`raw`] is a POSIX file descriptor. On windows, [`raw`] is a
/// `HANDLE` cast to `i64`. Ownership is transferred — the receiver is
/// responsible for closing it (usually by wrapping it in a
/// [`std::fs::File`] via [`PickedFileReader`] / [`PickedFileWriter`],
/// which close on drop).
///
/// [`raw`]: RawFileHandle::raw
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RawFileHandle {
    /// The raw fd (unix) or `HANDLE`-as-`i64` (windows).
    pub raw: i64,
}

/// Domain-level errors surfaced by every [`FilePicker`] method.
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum FilePickerError {
    /// User dismissed the picker without selecting anything. Returned
    /// only from methods whose signature is not already `Option<T>` —
    /// pick/save methods surface cancellation as `Ok(None)`.
    UserCancelled,
    /// Native side refused access to the requested resource — usually
    /// because SAF permissions were not persisted, or macOS App Sandbox
    /// blocked a non-user-selected path.
    PermissionDenied(String),
    /// Handle referred to a resource that no longer exists.
    NotFound(String),
    /// Backend does not implement the requested operation on this
    /// platform — for example [`FilePicker::path`] on Android when the
    /// underlying URI has no real filesystem path.
    UnsupportedOperation(String),
    /// A lower-level I/O failure while opening or metadata-fetching.
    Io(String),
    /// Any other backend failure. Message is the raw platform error.
    Backend(String),
}

impl std::fmt::Display for FilePickerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UserCancelled => f.write_str("file picker cancelled by user"),
            Self::PermissionDenied(msg) => write!(f, "file picker permission denied: {msg}"),
            Self::NotFound(msg) => write!(f, "file not found: {msg}"),
            Self::UnsupportedOperation(msg) => write!(f, "unsupported file operation: {msg}"),
            Self::Io(msg) => write!(f, "file picker I/O error: {msg}"),
            Self::Backend(msg) => write!(f, "file picker backend error: {msg}"),
        }
    }
}

impl std::error::Error for FilePickerError {}

/// The plugin trait.
///
/// Native backends implement this to serve picker requests; Rust code
/// calls it via the macro-generated [`FilePickerClient`].
///
/// The `_owned` variants generated by [`istmo_macros::owned`]
/// (`pick_file_owned`, `pick_files_owned`, `save_file_owned`) return the
/// ergonomic [`OwnedPickedFile`] whose [`handle`](OwnedPickedFile) drops
/// deterministically. Prefer them unless you have a specific reason to
/// re-ship the raw [`NativeHandleId`].
#[plugin(name = "istmo.file_picker", crate = "::istmo_core")]
pub trait FilePicker {
    /// Present a single-file picker. `Ok(None)` when the user cancels.
    #[owned]
    async fn pick_file(&self, config: PickConfig)
        -> Result<Option<PickedFile>, FilePickerError>;

    /// Present a multi-file picker. Empty `Vec` when the user cancels.
    ///
    /// Backends without native multi-select semantics may fall back to
    /// single-select and return a one-element vec.
    #[owned]
    async fn pick_files(&self, config: PickConfig)
        -> Result<Vec<PickedFile>, FilePickerError>;

    /// Present a save-file picker. `Ok(None)` when the user cancels.
    ///
    /// On Android SAF the returned handle points at an empty file the
    /// backend created via `ACTION_CREATE_DOCUMENT`; write to it via
    /// [`FilePicker::open_write`].
    #[owned]
    async fn save_file(&self, config: SaveConfig)
        -> Result<Option<PickedFile>, FilePickerError>;

    /// Open the picked resource for reading, returning a native fd /
    /// `HANDLE`. Prefer wrapping the result with [`PickedFileReader`].
    async fn open_read(&self, file: NativeHandleId)
        -> Result<RawFileHandle, FilePickerError>;

    /// Open the picked resource for writing, returning a native fd /
    /// `HANDLE`. Prefer wrapping the result with [`PickedFileWriter`].
    async fn open_write(&self, file: NativeHandleId)
        -> Result<RawFileHandle, FilePickerError>;

    /// Real filesystem path when the backend can produce one, otherwise
    /// `Ok(None)`.
    ///
    /// * Desktop → always `Some`.
    /// * iOS / sandboxed macOS → `Some` while security scope is active
    ///   (i.e. while the [`NativeHandle`](istmo_core::NativeHandle) is
    ///   alive).
    /// * Android → always `None`; SAF `content://` URIs are not paths.
    async fn path(&self, file: NativeHandleId)
        -> Result<Option<String>, FilePickerError>;
}
