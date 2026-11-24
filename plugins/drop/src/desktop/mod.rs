//! Desktop side: the options `listen` set, drops the app forwards from
//! its UI toolkit, and on Wayland the compositor's own drag and drop.
//!
//! winit delivers dropped files on X11, Windows and macOS, never on
//! Wayland. There the publisher opens a second protocol connection on
//! the app's `wl_display` (the window registered with `istmo-window`
//! carries it) and binds a `wl_data_device` of its own, the same way
//! `istmo-pen` reads tablets. Inside Flatpak it takes the files through
//! the document portal, so the paths are ones the sandbox can open.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};

use istmo_core::sync::lock;
use istmo_core::{IstmoError, Runtime, codec};
use istmo_window::{NativeWindow, WindowId, WindowListener, WindowRegistry};

use crate::{
    DRAG_CHANNEL, DROP_CHANNEL, DROP_QUEUE_CAPACITY, DragState, DropError, DropOptions, DropPoint,
    FileDropHost,
};

mod files;
#[cfg(target_os = "linux")]
mod portal;
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
mod wayland;

/// What `listen` asked for, shared by the host and the publisher.
#[derive(Debug, Clone, Default)]
pub(crate) struct Listening(Arc<Mutex<Option<DropOptions>>>);

impl Listening {
    pub(crate) fn set(&self, options: Option<DropOptions>) {
        *lock(&self.0) = options;
    }

    fn get(&self) -> Option<DropOptions> {
        lock(&self.0).clone()
    }
}

/// What the native backends need from the publisher, kept apart so the
/// Wayland pumps hold it without the window table.
#[derive(Debug)]
struct Shared {
    runtime: Weak<Runtime>,
    listening: Listening,
}

impl Shared {
    fn options(&self) -> Option<DropOptions> {
        self.listening.get()
    }

    fn set_state(&self, state: DragState) {
        let Some(runtime) = self.runtime.upgrade() else {
            return;
        };
        match codec::encode(&state) {
            Ok(bytes) => runtime.publish_early_latest(DRAG_CHANNEL, bytes),
            Err(err) => tracing::warn!(target: "istmo.drop", %err, "encode drag state"),
        }
    }

    /// Publish `paths` as one drop, filtered by the current options.
    fn deliver(
        &self,
        paths: Vec<PathBuf>,
        not_files: usize,
        position: Option<DropPoint>,
        window_id: Option<u64>,
    ) -> Result<(), IstmoError> {
        let options = self.options().unwrap_or_default();
        let mut dropped = files::collect(paths, &options);
        dropped.skipped.extend(files::not_files(not_files));
        if dropped.files.is_empty() && dropped.skipped.is_empty() {
            return Ok(());
        }
        dropped.position = position;
        dropped.window_id = window_id;
        dropped.received_at_ms = files::now_ms();
        let runtime = self.runtime.upgrade().ok_or(IstmoError::RuntimeShutDown)?;
        runtime.publish_early_queue(DROP_CHANNEL, DROP_QUEUE_CAPACITY, codec::encode(&dropped)?);
        Ok(())
    }
}

/// Desktop side of `istmo.drop`, one per app.
///
/// [`DropPublisher::install`] registers the host that answers `listen`.
/// Windows reach it through [`DropPublisher::follow_registry`] (or
/// [`DropPublisher::attach`]): on Wayland that is what makes drops work.
/// On other desktops the app forwards its toolkit's drops with
/// [`DropPublisher::publish_paths`], and gets the same filtering and the
/// same [`crate::DroppedFiles`] the Wayland path and the phones give.
///
/// Dropped files are not copied on desktop: the paths are the person's
/// own files, and [`crate::DropInbox::cleanup`] leaves them alone.
#[derive(Debug)]
pub struct DropPublisher {
    shared: Arc<Shared>,
    #[cfg(target_os = "linux")]
    wayland: Mutex<HashMap<usize, wayland::Attachment>>,
    #[cfg(not(target_os = "linux"))]
    windows: Mutex<HashMap<u64, NativeWindow>>,
}

impl DropPublisher {
    /// Register the desktop host on `runtime` and return the publisher.
    /// Keep the `Arc`: dropping it detaches every window.
    #[must_use]
    pub fn install(runtime: &Arc<Runtime>) -> Arc<Self> {
        let host = crate::DesktopFileDrop::default();
        let shared = Arc::new(Shared {
            runtime: Arc::downgrade(runtime),
            listening: host.listening.clone(),
        });
        runtime.register_host(FileDropHost::new(host));
        Arc::new(Self {
            shared,
            #[cfg(target_os = "linux")]
            wayland: Mutex::new(HashMap::new()),
            #[cfg(not(target_os = "linux"))]
            windows: Mutex::new(HashMap::new()),
        })
    }

    /// Follow `registry`: every window registered there, now or later,
    /// is attached, and unregistering it detaches it.
    pub fn follow_registry(self: &Arc<Self>, registry: &WindowRegistry) -> Result<(), DropError> {
        let listener: Arc<dyn WindowListener> = Arc::clone(self) as Arc<dyn WindowListener>;
        registry
            .add_listener(&listener)
            .map_err(|err| DropError::Backend(err.to_string()))
    }

    /// Attach one window. Only Wayland windows need it; others are
    /// remembered and their drops come through [`Self::publish_paths`].
    pub fn attach(&self, id: WindowId, window: NativeWindow) -> Result<(), DropError> {
        #[cfg(target_os = "linux")]
        if let NativeWindow::Wayland { surface, display } = window {
            let mut attachments = lock(&self.wayland);
            // One connection per display: a drag entering any window of
            // the app reaches every data device the app has bound.
            if let Some(attachment) = attachments.get(&display.get()) {
                attachment.add_window(id.0, surface.get());
            } else {
                let attachment = wayland::attach(display, Arc::clone(&self.shared))
                    .map_err(|err| DropError::Backend(err.to_string()))?;
                attachment.add_window(id.0, surface.get());
                attachments.insert(display.get(), attachment);
            }
            drop(attachments);
            return Ok(());
        }
        #[cfg(not(target_os = "linux"))]
        lock(&self.windows).insert(id.0, window);
        #[cfg(target_os = "linux")]
        let _ = (id, window);
        Ok(())
    }

    /// Detach one window; the last window of a display closes its
    /// connection.
    pub fn detach(&self, id: WindowId) {
        #[cfg(target_os = "linux")]
        lock(&self.wayland).retain(|_, attachment| attachment.remove_window(id.0));
        #[cfg(not(target_os = "linux"))]
        lock(&self.windows).remove(&id.0);
    }

    /// Forward a drop the UI toolkit delivered (winit
    /// `WindowEvent::DroppedFile`, Freya `on_file_drop`): checked against
    /// the options `listen` set and published like any other drop.
    /// Nothing is published while not listening.
    ///
    /// `position` is in logical pixels when the toolkit gives one (winit
    /// does not).
    pub fn publish_paths(
        &self,
        paths: Vec<PathBuf>,
        position: Option<DropPoint>,
        window_id: Option<u64>,
    ) -> Result<(), IstmoError> {
        if self.shared.options().is_none() {
            return Ok(());
        }
        self.shared.deliver(paths, 0, position, window_id)
    }

    /// Tell the app a drag is over a window: for toolkits that report
    /// hover (winit `HoveredFile` / `HoveredFileCancelled`). Ignored
    /// while not listening.
    pub fn publish_state(&self, state: DragState) {
        if self.shared.options().is_some() || state == DragState::Idle {
            self.shared.set_state(state);
        }
    }
}

impl WindowListener for DropPublisher {
    fn window_registered(&self, id: WindowId, window: NativeWindow) -> Result<(), String> {
        self.attach(id, window).map_err(|err| err.to_string())
    }

    fn window_unregistered(&self, id: WindowId) {
        self.detach(id);
    }
}
