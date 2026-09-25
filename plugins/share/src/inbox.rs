//! Receiving side: content other apps share into this one.
//!
//! Native layers (Android intent handler, iOS/macOS Share Extension
//! hand-off, Windows share-target activation) publish each
//! [`IncomingShare`] on the [`INCOMING_CHANNEL`] early-event queue.
//! The queue buffers up to [`INCOMING_QUEUE_CAPACITY`] shares until the
//! first subscriber attaches, so a share that cold-starts the app is not
//! lost while Rust boots.

use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use flume::Receiver;
use istmo_core::early_events::PreMainQueue;
use istmo_core::{IstmoError, Plugin, Runtime, codec};

use crate::{IncomingShare, ShareClient};

/// Early-event channel incoming shares are published on. Native code
/// publishes with `publishEarlyQueue(INCOMING_CHANNEL, capacity, bytes)`.
pub const INCOMING_CHANNEL: &str = "istmo.share.incoming";

/// Shares retained before the first subscriber attaches.
pub const INCOMING_QUEUE_CAPACITY: usize = 8;

/// Handle to the incoming-share queue.
#[derive(Debug, Clone)]
pub struct ShareInbox {
    queue: Arc<PreMainQueue>,
}

impl ShareInbox {
    /// Attach to the process-global runtime.
    pub fn acquire() -> Result<Self, IstmoError> {
        Self::from_runtime(&Runtime::global()?)
    }

    /// Attach to `runtime`. Fails when the runtime enforces plugin
    /// declarations and `istmo.share` was not declared.
    pub fn from_runtime(runtime: &Arc<Runtime>) -> Result<Self, IstmoError> {
        runtime.check_declared(ShareClient::PLUGIN_ID)?;
        Ok(Self {
            queue: runtime
                .early_events()
                .queue(INCOMING_CHANNEL, INCOMING_QUEUE_CAPACITY),
        })
    }

    /// Subscribe. The first subscriber receives every buffered share in
    /// arrival order, then live ones.
    #[must_use]
    pub fn stream(&self) -> IncomingStream {
        IncomingStream {
            rx: self.queue.subscribe(),
        }
    }

    /// Publish a share from Rust — for desktop integrations the plugin
    /// cannot observe itself (Linux `.desktop` `MimeType` handlers,
    /// command-line arguments, D-Bus activation) and for tests.
    /// `received_at_ms` is filled in when absent.
    pub fn publish(&self, mut share: IncomingShare) -> Result<(), IstmoError> {
        if share.received_at_ms.is_none() {
            share.received_at_ms = now_ms();
        }
        self.queue.publish(codec::encode(&share)?);
        Ok(())
    }

    /// Delete the app-owned copies of `share`'s files. Missing files are
    /// ignored; the first other I/O error is returned after attempting
    /// every file.
    pub fn cleanup(share: &IncomingShare) -> std::io::Result<()> {
        let mut first_error = None;
        for file in &share.files {
            match std::fs::remove_file(Path::new(&file.path)) {
                Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
                    first_error.get_or_insert(err);
                }
                Ok(()) | Err(_) => {}
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}

/// Stream of [`IncomingShare`]s. Mirrors the receive shapes of
/// [`istmo_core::TypedStream`].
#[derive(Debug)]
pub struct IncomingStream {
    rx: Receiver<Vec<u8>>,
}

impl IncomingStream {
    /// Block for the next share.
    pub fn recv(&self) -> Result<IncomingShare, IstmoError> {
        let bytes = self.rx.recv().map_err(|_| IstmoError::ChannelClosed)?;
        decode(&bytes)
    }

    /// Await the next share.
    pub async fn recv_async(&self) -> Result<IncomingShare, IstmoError> {
        let bytes = self
            .rx
            .recv_async()
            .await
            .map_err(|_| IstmoError::ChannelClosed)?;
        decode(&bytes)
    }

    /// Next share if one is buffered.
    #[must_use]
    pub fn try_recv(&self) -> Option<Result<IncomingShare, IstmoError>> {
        self.rx.try_recv().ok().map(|bytes| decode(&bytes))
    }
}

fn decode(bytes: &[u8]) -> Result<IncomingShare, IstmoError> {
    let (share, _) = codec::decode::<IncomingShare>(bytes)?;
    Ok(share)
}

fn now_ms() -> Option<u64> {
    let elapsed = SystemTime::now().duration_since(UNIX_EPOCH).ok()?;
    u64::try_from(elapsed.as_millis()).ok()
}
