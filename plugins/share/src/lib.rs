//! Cross-platform share plugin for [`istmo`](https://docs.rs/istmo):
//! send text, links and files through the OS share sheet, and receive
//! content other apps share into yours.
//!
//! | Platform | Send | Receive |
//! |---|---|---|
//! | Android | `ACTION_SEND(_MULTIPLE)` chooser + `FileProvider` | `ACTION_SEND` intent filters |
//! | iOS | `UIActivityViewController` | Share Extension + App Group |
//! | macOS | `NSSharingServicePicker` | Share Extension + App Group |
//! | Windows | `DataTransferManager` | Share Target (MSIX packages only) |
//! | Linux | not available — see [`ShareCapabilities`] | app-driven ([`ShareInbox::publish`]) |
//!
//! # Sending
//!
//! Build a [`ShareRequest`] (any mix of text, link, subject and files)
//! and call [`ShareClient::share`]. The future resolves with a
//! [`ShareOutcome`] once the platform reports what the user did — each
//! platform reports a different amount, see the variant docs.
//!
//! Linux has no standard share sheet (there is no XDG share portal),
//! so the desktop backend reports [`ShareError::Unsupported`] and every
//! [`ShareCapabilities`] flag as `false`; apps render their own targets
//! there (copy to clipboard, open mail client, …).
//!
//! # Receiving
//!
//! Content shared into the app is delivered through [`ShareInbox`] —
//! a buffered stream that also retains shares received before the app
//! subscribed (cold start from the share sheet). Files are always
//! copied into app-owned storage first, so every [`IncomingFile::path`]
//! is a readable filesystem path; call [`ShareInbox::cleanup`] when
//! done with them.
//!
//! # Deployment
//!
//! The native backends need an `Activity` (Android), a presenting view
//! controller (iOS) or a registered window (desktop), so the plugin is
//! pinned to `default_deployment = "local"`.
//!
//! On mobile the generated `IstmoPluginRegistry` registers it: Android
//! hands the backend the host `ComponentActivity` (`IstmoGameActivity`
//! is one), iOS presents from the key window. Desktop apps register
//! [`DesktopShare`] through `ShareHost` themselves.

#![doc(html_root_url = "https://docs.rs/istmo-share")]

#[cfg(feature = "codegen")]
pub mod codegen;

#[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
pub mod desktop;

#[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
pub use desktop::DesktopShare;

#[cfg(feature = "file-picker")]
mod picked;

mod inbox;

pub use inbox::{INCOMING_CHANNEL, INCOMING_QUEUE_CAPACITY, IncomingStream, ShareInbox};

use istmo_core::CancelToken;
use istmo_macros::{message, plugin};

/// Wire identifier for the share plugin.
pub const SHARE_PLUGIN_ID: &str = "istmo.share";

/// Rectangle in the anchor window's coordinate space, in points
/// (logical pixels), origin top-left.
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ShareRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Where the share sheet should appear.
///
/// Required on iPad (the sheet is a popover and crashes without an
/// anchor — the iOS backend falls back to the centre of the key window)
/// and on macOS (the picker is a popover). Windows uses only the
/// window. Android ignores it.
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ShareAnchor {
    /// Desktop: id the window was registered under in
    /// [`istmo_window::WindowRegistry`]. `None` picks the lowest
    /// registered id.
    pub window_id: Option<u64>,
    /// Element the sheet points at (e.g. the share button). `None`
    /// centres the sheet in the window.
    pub rect: Option<ShareRect>,
}

/// Where a [`ShareFile`]'s bytes come from.
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ShareFileSource {
    /// Absolute path to an existing file readable by the app. On
    /// Android the file must live under the app's internal files or
    /// cache directory (the `FileProvider` roots); other paths are
    /// copied into the cache first.
    Path(String),
    /// In-memory contents. The backend writes them to a temporary file
    /// named after [`ShareFile::name`] before sharing. Large payloads
    /// cross the wire once — prefer [`Self::Path`] for big files.
    Bytes(Vec<u8>),
}

/// One file attached to a [`ShareRequest`].
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ShareFile {
    pub source: ShareFileSource,
    /// File name shown to the receiving app, e.g. `"report.pdf"`.
    /// Required for [`ShareFileSource::Bytes`]; defaults to the path's
    /// file name otherwise.
    pub name: Option<String>,
    /// MIME type, e.g. `"application/pdf"`. Guessed from the name when
    /// absent.
    pub mime_type: Option<String>,
}

impl ShareFile {
    /// Share the file at `path`.
    #[must_use]
    pub fn path(path: impl Into<String>) -> Self {
        Self {
            source: ShareFileSource::Path(path.into()),
            name: None,
            mime_type: None,
        }
    }

    /// Share in-memory `bytes` under `name`.
    #[must_use]
    pub fn bytes(name: impl Into<String>, bytes: Vec<u8>) -> Self {
        Self {
            source: ShareFileSource::Bytes(bytes),
            name: Some(name.into()),
            mime_type: None,
        }
    }

    /// Set the MIME type.
    #[must_use]
    pub fn with_mime_type(mut self, mime_type: impl Into<String>) -> Self {
        self.mime_type = Some(mime_type.into());
        self
    }

    /// Effective display name: [`Self::name`], else the path's file
    /// name.
    #[must_use]
    pub fn display_name(&self) -> Option<&str> {
        self.name.as_deref().or_else(|| match &self.source {
            ShareFileSource::Path(path) => std::path::Path::new(path)
                .file_name()
                .and_then(std::ffi::OsStr::to_str),
            ShareFileSource::Bytes(_) => None,
        })
    }

    /// Effective MIME type: [`Self::mime_type`], else a guess from the
    /// display name's extension, else `application/octet-stream`.
    #[must_use]
    pub fn effective_mime_type(&self) -> &str {
        self.mime_type
            .as_deref()
            .unwrap_or_else(|| mime_from_name(self.display_name().unwrap_or_default()))
    }
}

/// Rich preview shown at the top of the share sheet. Opt-in: leave
/// [`ShareRequest::preview`] as `None` for the platform default.
///
/// Honoured by iOS (`LPLinkMetadata`), Android 10+ (`EXTRA_TITLE` and
/// a `ClipData` thumbnail) and Windows (`DataPackage` title and
/// thumbnail). macOS ignores it — `NSSharingServicePicker` has no
/// preview header. Check [`ShareCapabilities::rich_preview`].
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct SharePreview {
    /// Title shown in the preview header.
    pub title: Option<String>,
    /// Image shown in the preview header (PNG/JPEG).
    pub thumbnail: Option<ShareFile>,
    /// iOS only: fetch title and icon from [`ShareRequest::url`]
    /// with `LPMetadataProvider` before presenting. Adds a network
    /// round-trip; ignored when `url` is `None`.
    pub fetch_link_metadata: bool,
}

/// What to share. Any combination of fields may be set; at least one
/// of `text`, `url` or `files` must be.
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ShareRequest {
    /// Plain text body.
    pub text: Option<String>,
    /// Link. Kept separate from `text` so targets that understand links
    /// (messaging apps, browsers) receive a real URL; platforms without
    /// a dedicated link slot append it to the text.
    pub url: Option<String>,
    /// Subject line for targets that have one (mail). Also the default
    /// preview / chooser title.
    pub subject: Option<String>,
    /// Files to attach. Several files — and files together with text —
    /// are supported everywhere sending is.
    pub files: Vec<ShareFile>,
    /// Optional rich preview; `None` keeps the platform default.
    pub preview: Option<SharePreview>,
    /// Where to present the sheet (iPad / macOS / desktop window).
    pub anchor: Option<ShareAnchor>,
}

impl ShareRequest {
    /// Share `text`.
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: Some(text.into()),
            ..Self::default()
        }
    }

    /// Share the link `url`.
    #[must_use]
    pub fn url(url: impl Into<String>) -> Self {
        Self {
            url: Some(url.into()),
            ..Self::default()
        }
    }

    /// Share `files`.
    #[must_use]
    pub fn files(files: impl IntoIterator<Item = ShareFile>) -> Self {
        Self {
            files: files.into_iter().collect(),
            ..Self::default()
        }
    }

    #[must_use]
    pub fn with_text(mut self, text: impl Into<String>) -> Self {
        self.text = Some(text.into());
        self
    }

    #[must_use]
    pub fn with_url(mut self, url: impl Into<String>) -> Self {
        self.url = Some(url.into());
        self
    }

    #[must_use]
    pub fn with_subject(mut self, subject: impl Into<String>) -> Self {
        self.subject = Some(subject.into());
        self
    }

    #[must_use]
    pub fn with_file(mut self, file: ShareFile) -> Self {
        self.files.push(file);
        self
    }

    #[must_use]
    pub fn with_preview(mut self, preview: SharePreview) -> Self {
        self.preview = Some(preview);
        self
    }

    #[must_use]
    pub const fn with_anchor(mut self, anchor: ShareAnchor) -> Self {
        self.anchor = Some(anchor);
        self
    }

    /// `true` when there is nothing to share.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.text.is_none() && self.url.is_none() && self.files.is_empty()
    }

    /// Reject requests no backend can serve. Backends call this first;
    /// callers may too.
    pub fn validate(&self) -> Result<(), ShareError> {
        if self.is_empty() {
            return Err(ShareError::InvalidRequest(
                "share request has no text, url or files".to_owned(),
            ));
        }
        if let Some(file) = self
            .files
            .iter()
            .find(|f| matches!(f.source, ShareFileSource::Bytes(_)) && f.name.is_none())
        {
            return Err(ShareError::InvalidRequest(format!(
                "in-memory file of {} bytes needs a name",
                match &file.source {
                    ShareFileSource::Bytes(b) => b.len(),
                    ShareFileSource::Path(_) => 0,
                }
            )));
        }
        Ok(())
    }

    /// `text` and `url` joined for platforms with a single text slot.
    #[must_use]
    pub fn text_with_url(&self) -> Option<String> {
        match (self.text.as_deref(), self.url.as_deref()) {
            (Some(text), Some(url)) if text.contains(url) => Some(text.to_owned()),
            (Some(text), Some(url)) => Some(format!("{text}\n{url}")),
            (Some(text), None) => Some(text.to_owned()),
            (None, Some(url)) => Some(url.to_owned()),
            (None, None) => None,
        }
    }
}

/// What the user did with the share sheet, as far as the platform
/// reports it.
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ShareOutcome {
    /// The target reported success. Carries the target id when known
    /// (iOS/macOS activity type, Windows app user model id).
    Shared(Option<String>),
    /// The user picked a target but the platform does not report
    /// whether the share completed (Android). Carries the target's
    /// component / package name.
    TargetChosen(String),
    /// The user closed the sheet without sharing.
    Dismissed,
    /// The platform gives no feedback (Android when the chooser is
    /// dismissed, older Windows builds).
    Unknown,
}

/// Which parts of the API the current platform supports.
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[allow(clippy::struct_excessive_bools)]
pub struct ShareCapabilities {
    /// Any sending at all. `false` on Linux.
    pub send: bool,
    /// Files in [`ShareRequest::files`].
    pub files: bool,
    /// Text and files in the same request.
    pub mixed_content: bool,
    /// [`SharePreview`] is honoured.
    pub rich_preview: bool,
    /// [`ShareOutcome::Shared`] / [`ShareOutcome::Dismissed`] can be
    /// reported.
    pub reports_completion: bool,
    /// The chosen target is reported.
    pub reports_target: bool,
    /// The app can be a share destination right now: on Windows this
    /// requires build 19041+ and package identity, on Apple a Share
    /// Extension, on Android the opt-in `activity-alias`.
    pub receive: bool,
    /// [`Share::set_share_targets`] feeds the system's direct-share
    /// row (Android sharing shortcuts, iOS share-sheet suggestions).
    pub direct_share: bool,
    /// Dropping a pending [`ShareClient::share`] future closes the
    /// sheet. `false` where the platform offers no way to dismiss it
    /// (Android chooser, Windows share UI) — the call still resolves.
    pub dismiss_on_cancel: bool,
}

/// Errors surfaced by [`Share`] methods.
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ShareError {
    /// The platform (or this build of it) cannot share at all, or not
    /// this kind of content.
    Unsupported(String),
    /// The request itself is invalid (empty, unnamed bytes, bad URL).
    InvalidRequest(String),
    /// A [`ShareFileSource::Path`] does not exist.
    NotFound(String),
    /// No window / view controller / activity to present from.
    NoPresenter(String),
    /// Another share sheet is already open.
    Busy,
    /// Reading or staging a file failed.
    Io(String),
    /// Any other platform failure; message is the raw platform error.
    Backend(String),
}

impl std::fmt::Display for ShareError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported(msg) => write!(f, "sharing unsupported: {msg}"),
            Self::InvalidRequest(msg) => write!(f, "invalid share request: {msg}"),
            Self::NotFound(msg) => write!(f, "shared file not found: {msg}"),
            Self::NoPresenter(msg) => write!(f, "nothing to present the share sheet from: {msg}"),
            Self::Busy => f.write_str("a share sheet is already open"),
            Self::Io(msg) => write!(f, "share I/O error: {msg}"),
            Self::Backend(msg) => write!(f, "share backend error: {msg}"),
        }
    }
}

impl std::error::Error for ShareError {}

impl From<istmo_core::IstmoError> for ShareError {
    /// Decode the plugin's own error from a failed [`ShareClient`] call;
    /// transport failures become [`ShareError::Backend`].
    fn from(err: istmo_core::IstmoError) -> Self {
        match err {
            istmo_core::IstmoError::PluginError { bytes } => {
                istmo_core::codec::decode::<Self>(&bytes)
                    .map_or_else(|e| Self::Backend(e.to_string()), |(err, _)| err)
            }
            other => Self::Backend(other.to_string()),
        }
    }
}

/// A file shared into the app, already copied into app-owned storage.
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct IncomingFile {
    /// Absolute path of the app-owned copy.
    pub path: String,
    /// Original display name.
    pub name: String,
    pub mime_type: Option<String>,
    pub size: Option<u64>,
}

/// Content another app shared into this one.
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct IncomingShare {
    pub text: Option<String>,
    pub url: Option<String>,
    pub subject: Option<String>,
    pub files: Vec<IncomingFile>,
    /// Sending app when the platform reveals it (Android referrer,
    /// Windows package family name).
    pub source_app: Option<String>,
    /// Receive time, milliseconds since the Unix epoch.
    pub received_at_ms: Option<u64>,
    /// [`ShareTarget::id`] when the user picked one of the app's
    /// direct-share targets.
    pub target_id: Option<String>,
}

/// A direct-share destination inside the app (a chat, a folder, a
/// contact) offered in the system share sheet's suggestion row.
///
/// Android publishes them as long-lived sharing shortcuts; iOS donates
/// `INSendMessageIntent` interactions. Shares sent to a target arrive
/// with [`IncomingShare::target_id`] set.
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ShareTarget {
    /// Stable id, returned in [`IncomingShare::target_id`].
    pub id: String,
    /// Name shown under the icon.
    pub label: String,
    /// Square PNG/JPEG icon.
    pub icon: Option<ShareFile>,
}

impl ShareTarget {
    #[must_use]
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            icon: None,
        }
    }

    #[must_use]
    pub fn with_icon(mut self, icon: ShareFile) -> Self {
        self.icon = Some(icon);
        self
    }
}

/// The plugin trait.
///
/// Native backends implement it; Rust code calls it through the
/// macro-generated [`ShareClient`]. Receiving is not a trait method —
/// see [`ShareInbox`].
#[plugin(name = "istmo.share", crate = "::istmo_core")]
pub trait Share {
    /// Present the share sheet for `request` and resolve with what the
    /// user did.
    ///
    /// Dropping the client future cancels the call and closes the sheet
    /// where [`ShareCapabilities::dismiss_on_cancel`] says the platform
    /// allows it.
    async fn share(
        &self,
        cancel: CancelToken,
        request: ShareRequest,
    ) -> Result<ShareOutcome, ShareError>;

    /// Report what this platform supports, so apps can hide the share
    /// button or render their own UI (Linux).
    async fn capabilities(&self) -> Result<ShareCapabilities, ShareError>;

    /// Directory where Rust may write files to share, inside the
    /// backend's shareable roots (the `FileProvider` cache on Android).
    /// The backend prunes it: after a day on mobile, when the backend is
    /// dropped on desktop.
    async fn staging_dir(&self) -> Result<String, ShareError>;

    /// Replace the app's direct-share targets. An empty list clears
    /// them. [`ShareError::Unsupported`] where
    /// [`ShareCapabilities::direct_share`] is `false`.
    async fn set_share_targets(&self, targets: Vec<ShareTarget>) -> Result<(), ShareError>;
}

/// How long received files stay in app-owned storage when the app
/// never calls [`ShareInbox::cleanup`]. Every backend prunes entries
/// older than this whenever a new share arrives.
pub const INCOMING_RETENTION: std::time::Duration =
    std::time::Duration::from_secs(7 * 24 * 60 * 60);

/// Keep only the final path component and replace characters Windows
/// rejects, so a hostile or sloppy name cannot escape a staging dir.
#[must_use]
pub(crate) fn sanitize_file_name(name: &str) -> String {
    let base = std::path::Path::new(name)
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or("shared");
    let cleaned: String = base
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    if cleaned.trim_matches('.').is_empty() {
        "shared".to_owned()
    } else {
        cleaned
    }
}

/// Minimal extension → MIME table for the types people share most.
/// Backends with a platform lookup (Android `MimeTypeMap`, Apple `UTType`)
/// use it only as a fallback.
#[must_use]
pub fn mime_from_name(name: &str) -> &'static str {
    let ext = std::path::Path::new(name)
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .map(str::to_ascii_lowercase);
    match ext.as_deref() {
        Some("txt") => "text/plain",
        Some("html" | "htm") => "text/html",
        Some("csv") => "text/csv",
        Some("json") => "application/json",
        Some("pdf") => "application/pdf",
        Some("zip") => "application/zip",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("heic") => "image/heic",
        Some("svg") => "image/svg+xml",
        Some("mp4") => "video/mp4",
        Some("mov") => "video/quicktime",
        Some("mp3") => "audio/mpeg",
        Some("m4a") => "audio/mp4",
        Some("doc") => "application/msword",
        Some("docx") => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        Some("xls") => "application/vnd.ms-excel",
        Some("xlsx") => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        Some("ppt") => "application/vnd.ms-powerpoint",
        Some("pptx") => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        _ => "application/octet-stream",
    }
}
