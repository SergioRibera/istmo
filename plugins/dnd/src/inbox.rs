//! Receiving side: the drops the native backends publish.
//!
//! Each drop goes on the [`DROP_CHANNEL`] early-event queue as one
//! [`DroppedFiles`]. The queue keeps up to [`DROP_QUEUE_CAPACITY`] drops
//! until the app subscribes, so a drop that lands while Rust is still
//! starting is not lost.

use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use flume::Receiver;
use istmo_core::early_events::{LatestValueSlot, PreMainQueue};
use istmo_core::{IstmoError, Plugin, Runtime, codec};

use crate::{DragAndDropClient, DragState, DroppedFiles};

/// Early-event channel drops are published on. Native code publishes
/// with `publishEarlyQueue(DROP_CHANNEL, DROP_QUEUE_CAPACITY, bytes)`.
pub const DROP_CHANNEL: &str = "istmo.dnd.drops";

/// Drops kept before the app subscribes.
pub const DROP_QUEUE_CAPACITY: usize = 8;

/// Latest-value channel of the [`DragState`]. Native code publishes with
/// `publishEarlyLatest(DRAG_CHANNEL, bytes)`.
pub const DRAG_CHANNEL: &str = "istmo.dnd.incoming";

/// Directory (as path components) the backends copy dropped files into:
/// `<cache>/istmo-dnd/incoming/<uuid>/<name>` on Android and iOS.
pub const DROP_DIR: &str = "istmo-dnd/incoming";

/// Largest drop the queue decodes; the backends publish paths, not file
/// contents, so a real drop is a few kilobytes.
const MAX_DROP_BYTES: usize = 1 << 20;

/// Handle to the queue of drops.
#[derive(Debug, Clone)]
pub struct DropInbox {
    queue: Arc<PreMainQueue>,
    drag: Arc<LatestValueSlot>,
}

impl DropInbox {
    /// Attach to the process-global runtime.
    pub fn acquire() -> Result<Self, IstmoError> {
        Self::from_runtime(&Runtime::global()?)
    }

    /// Attach to `runtime`. Fails when the runtime enforces plugin
    /// declarations and `istmo.dnd` was not declared.
    pub fn from_runtime(runtime: &Arc<Runtime>) -> Result<Self, IstmoError> {
        runtime.check_declared(DragAndDropClient::PLUGIN_ID)?;
        let events = runtime.early_events();
        Ok(Self {
            queue: events.queue(DROP_CHANNEL, DROP_QUEUE_CAPACITY),
            drag: events.latest_slot(DRAG_CHANNEL),
        })
    }

    /// Subscribe. The first subscriber gets every drop kept so far, in
    /// order, then the new ones.
    #[must_use]
    pub fn stream(&self) -> DropStream {
        DropStream {
            rx: self.queue.subscribe(),
        }
    }

    /// What is being dragged over the window now.
    #[must_use]
    pub fn drag_state(&self) -> DragState {
        self.drag
            .peek()
            .and_then(|bytes| codec::decode_exact_capped(&bytes, MAX_DROP_BYTES).ok())
            .unwrap_or_default()
    }

    /// Every change of the [`DragState`], starting with the current one:
    /// an overlay while files hover, a note while they are copied.
    #[must_use]
    pub fn drag_changes(&self) -> DragStream {
        DragStream {
            rx: self.drag.subscribe(),
        }
    }

    /// Publish a drop from Rust: for desktop apps forwarding what their
    /// UI toolkit received, and for tests. `received_at_ms` is filled in
    /// when absent.
    pub fn publish(&self, mut dropped: DroppedFiles) -> Result<(), IstmoError> {
        if dropped.received_at_ms.is_none() {
            dropped.received_at_ms = now_ms();
        }
        self.queue.publish(codec::encode(&dropped)?);
        Ok(())
    }

    /// Delete the copies of `dropped`'s files.
    ///
    /// Only regular files that, once canonicalised, sit inside a
    /// [`DROP_DIR`] directory are deleted. Any other path is left alone
    /// without an error: a desktop drop forwarded with the person's own
    /// files, `..` tricks, symlinks out of the directory. The per-drop
    /// folder goes once empty. Missing files are ignored; the first other
    /// I/O error is returned after trying every file.
    pub fn cleanup(dropped: &DroppedFiles) -> std::io::Result<()> {
        let mut first_error = None;
        for file in &dropped.files {
            match remove_copy(Path::new(&file.path)) {
                Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
                    first_error.get_or_insert(err);
                }
                Ok(()) | Err(_) => {}
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}

fn remove_copy(path: &Path) -> std::io::Result<()> {
    let canonical = std::fs::canonicalize(path)?;
    // Not a copy this plugin made: never ours to delete.
    if !is_in_drop_dir(&canonical) || !std::fs::symlink_metadata(&canonical)?.is_file() {
        return Ok(());
    }
    std::fs::remove_file(&canonical)?;
    // The `<uuid>` folder of the drop, when that was its last file.
    if let Some(folder) = canonical.parent()
        && is_in_drop_dir(folder)
    {
        let _ = std::fs::remove_dir(folder);
    }
    Ok(())
}

/// Whether `path` (already canonical) lies strictly below a directory
/// whose last components are [`DROP_DIR`].
fn is_in_drop_dir(path: &Path) -> bool {
    let marker: Vec<_> = Path::new(DROP_DIR).components().collect();
    let components: Vec<_> = path.components().collect();
    components
        .windows(marker.len())
        .enumerate()
        .any(|(at, window)| window == marker.as_slice() && at + marker.len() < components.len())
}

/// Stream of [`DroppedFiles`]. Mirrors the receive shapes of
/// [`istmo_core::TypedStream`].
#[derive(Debug)]
pub struct DropStream {
    rx: Receiver<Vec<u8>>,
}

impl DropStream {
    /// Block for the next drop.
    pub fn recv(&self) -> Result<DroppedFiles, IstmoError> {
        let bytes = self.rx.recv().map_err(|_| IstmoError::ChannelClosed)?;
        decode(&bytes)
    }

    /// Await the next drop.
    pub async fn recv_async(&self) -> Result<DroppedFiles, IstmoError> {
        let bytes = self
            .rx
            .recv_async()
            .await
            .map_err(|_| IstmoError::ChannelClosed)?;
        decode(&bytes)
    }

    /// The next drop if one is waiting.
    #[must_use]
    pub fn try_recv(&self) -> Option<Result<DroppedFiles, IstmoError>> {
        self.rx.try_recv().ok().map(|bytes| decode(&bytes))
    }
}

/// Stream of [`DragState`] changes.
#[derive(Debug)]
pub struct DragStream {
    rx: Receiver<Vec<u8>>,
}

impl DragStream {
    /// Block for the next change.
    pub fn recv(&self) -> Result<DragState, IstmoError> {
        let bytes = self.rx.recv().map_err(|_| IstmoError::ChannelClosed)?;
        Ok(codec::decode_exact_capped(&bytes, MAX_DROP_BYTES)?)
    }

    /// Await the next change.
    pub async fn recv_async(&self) -> Result<DragState, IstmoError> {
        let bytes = self
            .rx
            .recv_async()
            .await
            .map_err(|_| IstmoError::ChannelClosed)?;
        Ok(codec::decode_exact_capped(&bytes, MAX_DROP_BYTES)?)
    }
}

fn decode(bytes: &[u8]) -> Result<DroppedFiles, IstmoError> {
    Ok(codec::decode_exact_capped::<DroppedFiles>(
        bytes,
        MAX_DROP_BYTES,
    )?)
}

fn now_ms() -> Option<u64> {
    let elapsed = SystemTime::now().duration_since(UNIX_EPOCH).ok()?;
    u64::try_from(elapsed.as_millis()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_paths_below_the_drop_dir_count() {
        assert!(is_in_drop_dir(Path::new(
            "/cache/istmo-dnd/incoming/abc/a.pdf"
        )));
        assert!(!is_in_drop_dir(Path::new("/cache/istmo-dnd/incoming")));
        assert!(!is_in_drop_dir(Path::new("/home/me/Documents/a.pdf")));
        assert!(!is_in_drop_dir(Path::new("/cache/istmo-dnd/a.pdf")));
    }
}
