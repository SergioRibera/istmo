//! Per-frame placement of embedded views.
//!
//! UI code calls [`Presenter::sync`] every frame with where the view
//! should be. Nothing is sent when the frame did not change. A host in
//! this process (the desktop backends) registers a [`FrameSink`] and
//! receives the frame directly — a store into a latest-value slot its
//! engine thread reads once per tick, no encoding, no thread hop. Hosts
//! across the bus (Kotlin / Swift) get a fire-and-forget `set_frame`
//! notification, which they apply on their next display frame.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use istmo_core::sync::lock;
use istmo_core::{InstanceId, NativeHandleId, Runtime, codec};

use crate::{ViewFrame, ViewRef, WEBVIEW_PLUGIN_ID};

/// In-process receiver of a view's frames.
pub trait FrameSink: Send + Sync + 'static {
    /// Latest frame of the view. Called on the app's UI thread; must not
    /// block.
    fn set_frame(&self, frame: ViewFrame);
}

/// Sinks keyed by runtime (address) and view: ids are only unique within
/// one runtime.
type SinkKey = (usize, NativeHandleId);

fn key(runtime: &Arc<Runtime>, view: NativeHandleId) -> SinkKey {
    (Arc::as_ptr(runtime).addr(), view)
}

fn sinks() -> &'static Mutex<HashMap<SinkKey, Arc<dyn FrameSink>>> {
    static SINKS: OnceLock<Mutex<HashMap<SinkKey, Arc<dyn FrameSink>>>> = OnceLock::new();
    SINKS.get_or_init(Mutex::default)
}

/// Route frames of `view` of `runtime` straight to `sink` (Rust hosts in
/// this process). Replaces a previous sink of the same view.
pub fn register_frame_sink(runtime: &Arc<Runtime>, view: NativeHandleId, sink: Arc<dyn FrameSink>) {
    lock(sinks()).insert(key(runtime, view), sink);
}

/// Stop routing frames of `view` to an in-process sink.
pub fn unregister_frame_sink(runtime: &Arc<Runtime>, view: NativeHandleId) {
    lock(sinks()).remove(&key(runtime, view));
}

fn local_sink(runtime: &Arc<Runtime>, view: NativeHandleId) -> Option<Arc<dyn FrameSink>> {
    lock(sinks()).get(&key(runtime, view)).cloned()
}

/// Sends a view's placement when it changes.
pub struct Presenter {
    view: ViewRef,
    runtime: Arc<Runtime>,
    instance: Option<InstanceId>,
    /// Resolved on first use; `None` inside = remote host.
    sink: OnceLock<Option<Arc<dyn FrameSink>>>,
    last: Mutex<Option<ViewFrame>>,
}

impl std::fmt::Debug for Presenter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Presenter")
            .field("view", &self.view)
            .field("last", &*lock(&self.last))
            .finish_non_exhaustive()
    }
}

impl Presenter {
    pub(crate) fn new(view: ViewRef, runtime: Arc<Runtime>, instance: Option<InstanceId>) -> Self {
        Self {
            view,
            runtime,
            instance,
            sink: OnceLock::new(),
            last: Mutex::new(None),
        }
    }

    /// Place the view at `frame` (physical pixels of its parent).
    /// Cheap when nothing changed; call it every frame.
    pub fn sync(&self, frame: ViewFrame) {
        {
            let mut last = lock(&self.last);
            if *last == Some(frame) {
                return;
            }
            *last = Some(frame);
        }
        let sink = self
            .sink
            .get_or_init(|| local_sink(&self.runtime, self.view.view));
        if let Some(sink) = sink {
            sink.set_frame(frame);
            return;
        }
        let payload = match codec::encode(&(self.view, frame)) {
            Ok(payload) => payload,
            Err(err) => {
                tracing::error!(%err, "encode web view frame");
                return;
            }
        };
        if let Err(err) =
            self.runtime
                .notify(WEBVIEW_PLUGIN_ID, self.instance, "set_frame", payload)
        {
            tracing::debug!(%err, "web view frame not delivered");
            // Let the next call retry.
            *lock(&self.last) = None;
        }
    }

    /// Hide the view, keeping it alive.
    pub fn hide(&self) {
        let frame = lock(&self.last).map_or_else(ViewFrame::default, |f| ViewFrame {
            visible: false,
            ..f
        });
        self.sync(frame);
    }

    /// Last frame sent.
    #[must_use]
    pub fn last(&self) -> Option<ViewFrame> {
        *lock(&self.last)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[derive(Default)]
    struct Counting(AtomicUsize);

    impl FrameSink for Counting {
        fn set_frame(&self, _frame: ViewFrame) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn unchanged_frames_are_not_resent() {
        let rt = Runtime::mock().runtime;
        let view = ViewRef {
            view: NativeHandleId(4242),
            token_hi: 1,
            token_lo: 2,
        };
        let sink = Arc::new(Counting::default());
        register_frame_sink(&rt, view.view, sink.clone());
        let presenter = Presenter::new(view, Arc::clone(&rt), None);
        let frame = ViewFrame {
            x: 1,
            y: 2,
            width: 3,
            height: 4,
            visible: true,
        };
        for _ in 0..10 {
            presenter.sync(frame);
        }
        presenter.hide();
        presenter.hide();
        assert_eq!(sink.0.load(Ordering::Relaxed), 2);
        assert!(!presenter.last().unwrap().visible);
        unregister_frame_sink(&rt, view.view);
    }
}
