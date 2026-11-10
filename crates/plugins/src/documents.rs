//! Documents the operating system asks the app to open.
//!
//! Every platform funnels into the same [`PreMainQueue`] so a file that
//! arrives before the UI subscribes is not lost:
//!
//! - **Desktop** — command-line arguments of the first launch, plus the
//!   arguments a second launch forwards to the running instance
//!   ([`crate::documents_publisher`]).
//! - **Android** — `ACTION_VIEW` / `ACTION_SEND` intents, published by
//!   the app's activity.
//! - **iOS** — `scene(_:openURLContexts:)` / `application(_:open:)`,
//!   published by the app delegate.
//!
//! Which types reach the app is declared once in `istmo.toml`
//! (`[[app.document_type]]`); istmo-build turns that into intent
//! filters and `CFBundleDocumentTypes`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use flume::Receiver;
use istmo_core::early_events::PreMainQueue;
use istmo_core::{IstmoError, Plugin, Runtime, codec};
use istmo_macros::message;

pub const DOCUMENTS_PLUGIN_ID: &str = "istmo.documents";

/// Early-event channel documents are published on.
pub const DOCUMENTS_CHANNEL: &str = "istmo.documents";

/// Default number of pre-subscriber documents retained by the queue.
pub const DEFAULT_DOCUMENTS_CAPACITY: usize = 32;

/// Early-event channel for launches that forwarded no document (the
/// user started the app again while it runs).
pub const DOCUMENTS_ACTIVATION_CHANNEL: &str = "istmo.documents.activation";

const ACTIVATION_CAPACITY: usize = 4;

/// A later launch asked the running app to come forward.
#[message(bincode = "::bincode")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceActivation {
    /// Same as [`OpenedDocument::activation_token`].
    pub activation_token: Option<String>,
}

/// Where an opened document lives.
#[message(bincode = "::bincode")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocumentLocation {
    /// Absolute path on the local file system.
    Path(String),
    /// Platform URI only the native side can resolve: an Android
    /// `content://` URI or an iOS security-scoped `file://` URL.
    Uri(String),
}

/// How the document reached the app.
#[message(bincode = "::bincode")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentOrigin {
    /// It started the app (cold start).
    Launch,
    /// The app was already running.
    Running,
    /// Shared into the app (`ACTION_SEND`, share sheet).
    Share,
}

#[message(bincode = "::bincode")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenedDocument {
    pub location: DocumentLocation,
    /// File name to show, when the location does not carry one
    /// (`content://` URIs).
    pub display_name: Option<String>,
    pub mime_type: Option<String>,
    pub origin: DocumentOrigin,
    /// Desktop focus hand-off: the `XDG_ACTIVATION_TOKEN` (Wayland) or
    /// `DESKTOP_STARTUP_ID` (X11) the launcher gave the process that
    /// asked to open the document. The window showing it presents the
    /// token to get focus.
    pub activation_token: Option<String>,
}

impl OpenedDocument {
    /// Local path, when the document is a plain file.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        match &self.location {
            DocumentLocation::Path(path) => Some(Path::new(path)),
            DocumentLocation::Uri(_) => None,
        }
    }

    /// File name to show: `display_name`, else the last path segment.
    #[must_use]
    pub fn name(&self) -> Option<String> {
        self.display_name.clone().or_else(|| {
            self.path()
                .and_then(Path::file_name)
                .map(|name| name.to_string_lossy().into_owned())
        })
    }

    /// Document for a local file, `None` when the path is not UTF-8
    /// (it would not survive the wire).
    #[must_use]
    pub fn from_path(path: PathBuf, origin: DocumentOrigin) -> Option<Self> {
        let path = path.into_os_string().into_string().ok()?;
        Some(Self {
            location: DocumentLocation::Path(path),
            display_name: None,
            mime_type: None,
            origin,
            activation_token: None,
        })
    }
}

#[derive(Debug, Clone)]
pub struct Documents {
    queue: Arc<PreMainQueue>,
    activations: Arc<PreMainQueue>,
}

impl Plugin for Documents {
    const PLUGIN_ID: &'static str = DOCUMENTS_PLUGIN_ID;
}

impl Documents {
    pub fn acquire() -> Result<Self, IstmoError> {
        let rt = Runtime::global()?;
        Self::from_runtime(&rt)
    }

    pub fn from_runtime(rt: &Arc<Runtime>) -> Result<Self, IstmoError> {
        rt.check_declared(Self::PLUGIN_ID)?;
        Ok(Self {
            queue: queue(rt),
            activations: rt
                .early_events()
                .queue(DOCUMENTS_ACTIVATION_CHANNEL, ACTIVATION_CAPACITY),
        })
    }

    #[must_use]
    pub fn stream(&self) -> DocumentStream {
        DocumentStream {
            rx: self.queue.subscribe(),
        }
    }

    /// Launches that forwarded no document; raise or focus a window.
    #[must_use]
    pub fn activations(&self) -> ActivationStream {
        ActivationStream {
            rx: self.activations.subscribe(),
        }
    }

    /// Publish a document from Rust (desktop publisher, tests). Native
    /// shells publish through `Frame::EarlyEvent` on the same channel.
    pub fn publish(rt: &Runtime, document: &OpenedDocument) -> Result<(), IstmoError> {
        let bytes = codec::encode(document)?;
        rt.publish_early_queue(DOCUMENTS_CHANNEL, DEFAULT_DOCUMENTS_CAPACITY, bytes);
        Ok(())
    }

    /// Publish an [`InstanceActivation`].
    pub fn publish_activation(
        rt: &Runtime,
        activation_token: Option<&str>,
    ) -> Result<(), IstmoError> {
        let bytes = codec::encode(&InstanceActivation {
            activation_token: activation_token.map(str::to_owned),
        })?;
        rt.publish_early_queue(DOCUMENTS_ACTIVATION_CHANNEL, ACTIVATION_CAPACITY, bytes);
        Ok(())
    }
}

fn queue(rt: &Runtime) -> Arc<PreMainQueue> {
    rt.early_events()
        .queue(DOCUMENTS_CHANNEL, DEFAULT_DOCUMENTS_CAPACITY)
}

#[derive(Debug)]
pub struct DocumentStream {
    rx: Receiver<Vec<u8>>,
}

impl DocumentStream {
    pub fn recv(&self) -> Result<OpenedDocument, IstmoError> {
        let bytes = self.rx.recv().map_err(|_| IstmoError::ChannelClosed)?;
        decode(&bytes)
    }

    pub async fn recv_async(&self) -> Result<OpenedDocument, IstmoError> {
        let bytes = self
            .rx
            .recv_async()
            .await
            .map_err(|_| IstmoError::ChannelClosed)?;
        decode(&bytes)
    }

    #[must_use]
    pub fn try_recv(&self) -> Option<Result<OpenedDocument, IstmoError>> {
        self.rx.try_recv().ok().map(|b| decode(&b))
    }
}

fn decode(bytes: &[u8]) -> Result<OpenedDocument, IstmoError> {
    let (document, _) = codec::decode::<OpenedDocument>(bytes)?;
    Ok(document)
}

#[derive(Debug)]
pub struct ActivationStream {
    rx: Receiver<Vec<u8>>,
}

impl ActivationStream {
    pub fn recv(&self) -> Result<InstanceActivation, IstmoError> {
        let bytes = self.rx.recv().map_err(|_| IstmoError::ChannelClosed)?;
        decode_activation(&bytes)
    }

    pub async fn recv_async(&self) -> Result<InstanceActivation, IstmoError> {
        let bytes = self
            .rx
            .recv_async()
            .await
            .map_err(|_| IstmoError::ChannelClosed)?;
        decode_activation(&bytes)
    }

    #[must_use]
    pub fn try_recv(&self) -> Option<Result<InstanceActivation, IstmoError>> {
        self.rx.try_recv().ok().map(|b| decode_activation(&b))
    }
}

fn decode_activation(bytes: &[u8]) -> Result<InstanceActivation, IstmoError> {
    let (activation, _) = codec::decode::<InstanceActivation>(bytes)?;
    Ok(activation)
}
