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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

use istmo_core::sync::lock;
use istmo_core::{IstmoError, Runtime, codec};
use istmo_window::{NativeWindow, WindowId, WindowListener, WindowRegistry};

use crate::{
    DRAG_CHANNEL, DROP_CHANNEL, DROP_QUEUE_CAPACITY, DndError, DragAndDropHost, DragRequest,
    DragResult, DragState, DropOptions, DropPoint,
};

mod files;
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod macos;
mod outgoing;
#[cfg(target_os = "linux")]
mod portal;
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
mod wayland;
#[cfg(target_os = "windows")]
#[allow(unsafe_code)]
mod win32;
#[cfg(target_os = "linux")]
mod x11;

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

/// How the host reaches the publisher to start drags out of the app.
#[derive(Clone, Default)]
pub(crate) struct Outgoing(Arc<Mutex<Option<Weak<DndPublisher>>>>);

impl std::fmt::Debug for Outgoing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Outgoing").finish_non_exhaustive()
    }
}

impl Outgoing {
    pub(crate) async fn start(&self, request: DragRequest) -> Result<DragResult, DndError> {
        let publisher = lock(&self.0)
            .as_ref()
            .and_then(Weak::upgrade)
            .ok_or(DndError::Unsupported)?;
        let ended = publisher.start_drag(request);
        ended
            .recv_async()
            .await
            .unwrap_or_else(|_| Err(DndError::Backend("the drag backend went away".into())))
    }
}

/// Answers one drag out of the app: once, and clears the dragging flag.
pub(crate) struct DragDone {
    tx: flume::Sender<Result<DragResult, DndError>>,
    shared: Arc<Shared>,
}

impl std::fmt::Debug for DragDone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DragDone").finish_non_exhaustive()
    }
}

impl DragDone {
    pub(crate) fn finish(self, result: Result<DragResult, DndError>) {
        *lock(&self.shared.ended_at) = Some(std::time::Instant::now());
        self.shared.dragging_out.store(false, Ordering::Release);
        let _ = self.tx.send(result);
    }
}

/// What the native backends need from the publisher, kept apart so the
/// Wayland pumps hold it without the window table.
#[derive(Debug)]
struct Shared {
    runtime: Weak<Runtime>,
    listening: Listening,
    /// A drag out of the app is in progress: drops of it on the app
    /// itself are not taken as incoming files.
    dragging_out: AtomicBool,
    /// When the last drag out ended: the toolkit may deliver its own drop
    /// of it a moment later.
    ended_at: Mutex<Option<std::time::Instant>>,
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
            Err(err) => tracing::warn!(target: "istmo.dnd", %err, "encode drag state"),
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

/// Desktop side of `istmo.dnd`, one per app.
///
/// [`DndPublisher::install`] registers the host that answers `listen`.
/// Windows reach it through [`DndPublisher::follow_registry`] (or
/// [`DndPublisher::attach`]): on Wayland that is what makes drops work.
/// On other desktops the app forwards its toolkit's drops with
/// [`DndPublisher::publish_paths`], and gets the same filtering and the
/// same [`crate::DroppedFiles`] the Wayland path and the phones give.
///
/// Dropped files are not copied on desktop: the paths are the person's
/// own files, and [`crate::DropInbox::cleanup`] leaves them alone.
#[derive(Debug)]
pub struct DndPublisher {
    shared: Arc<Shared>,
    #[cfg(target_os = "linux")]
    wayland: Mutex<HashMap<usize, wayland::Attachment>>,
    /// Every window attached, for dragging out of it.
    windows: Mutex<HashMap<u64, NativeWindow>>,
}

impl DndPublisher {
    /// Register the desktop host on `runtime` and return the publisher.
    /// Keep the `Arc`: dropping it detaches every window.
    #[must_use]
    pub fn install(runtime: &Arc<Runtime>) -> Arc<Self> {
        let host = crate::DesktopDragAndDrop::default();
        let shared = Arc::new(Shared {
            runtime: Arc::downgrade(runtime),
            listening: host.listening.clone(),
            dragging_out: AtomicBool::new(false),
            ended_at: Mutex::new(None),
        });
        let outgoing = host.outgoing.clone();
        runtime.register_host(DragAndDropHost::new(host));
        let publisher = Arc::new(Self {
            shared,
            #[cfg(target_os = "linux")]
            wayland: Mutex::new(HashMap::new()),
            windows: Mutex::new(HashMap::new()),
        });
        *lock(&outgoing.0) = Some(Arc::downgrade(&publisher));
        publisher
    }

    /// Follow `registry`: every window registered there, now or later,
    /// is attached, and unregistering it detaches it.
    pub fn follow_registry(self: &Arc<Self>, registry: &WindowRegistry) -> Result<(), DndError> {
        let listener: Arc<dyn WindowListener> = Arc::clone(self) as Arc<dyn WindowListener>;
        registry
            .add_listener(&listener)
            .map_err(|err| DndError::Backend(err.to_string()))
    }

    /// Attach one window. Only Wayland windows need it; others are
    /// remembered and their drops come through [`Self::publish_paths`].
    pub fn attach(&self, id: WindowId, window: NativeWindow) -> Result<(), DndError> {
        lock(&self.windows).insert(id.0, window);
        #[cfg(windows)]
        if let NativeWindow::Win32 { hwnd } = window {
            win32::attach(hwnd);
        }
        #[cfg(target_os = "linux")]
        if let NativeWindow::Wayland { surface, display } = window {
            let mut attachments = lock(&self.wayland);
            // One connection per display: a drag entering any window of
            // the app reaches every data device the app has bound.
            if let Some(attachment) = attachments.get(&display.get()) {
                attachment.add_window(id.0, surface.get());
            } else {
                let attachment = wayland::attach(display, Arc::clone(&self.shared))
                    .map_err(|err| DndError::Backend(err.to_string()))?;
                attachment.add_window(id.0, surface.get());
                attachments.insert(display.get(), attachment);
            }
            drop(attachments);
            return Ok(());
        }
        Ok(())
    }

    /// Detach one window; the last window of a display closes its
    /// connection.
    pub fn detach(&self, id: WindowId) {
        #[cfg(target_os = "linux")]
        lock(&self.wayland).retain(|_, attachment| attachment.remove_window(id.0));
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

    /// Whether a drag out of the app is in progress, or ended less than a
    /// second ago. Apps that forward their toolkit's drops ignore the ones
    /// that arrive meanwhile: they are the app's own drag, let go on its
    /// own window (the toolkit may deliver it just after the drag ends).
    #[must_use]
    pub fn dragging_out(&self) -> bool {
        self.shared.dragging_out.load(Ordering::Acquire)
            || lock(&self.shared.ended_at)
                .is_some_and(|ended| ended.elapsed() < std::time::Duration::from_secs(1))
    }

    /// Drag `request` out of one of the windows; the receiver gets how it
    /// ended. What [`crate::DragAndDropClient::start_drag`] does on desktop.
    pub fn start_drag(
        &self,
        request: DragRequest,
    ) -> flume::Receiver<Result<DragResult, DndError>> {
        let (tx, rx) = flume::bounded(1);
        let done = DragDone {
            tx,
            shared: Arc::clone(&self.shared),
        };
        if request.items.is_empty() {
            done.finish(Err(DndError::Backend("nothing to drag".into())));
            return rx;
        }
        let window = {
            let windows = lock(&self.windows);
            request
                .window_id
                .and_then(|id| windows.get(&id).copied())
                .or_else(|| windows.iter().min_by_key(|(id, _)| **id).map(|(_, w)| *w))
        };
        let Some(window) = window else {
            done.finish(Err(DndError::Unsupported));
            return rx;
        };
        if self.shared.dragging_out.swap(true, Ordering::AcqRel) {
            let _ = done
                .tx
                .send(Err(DndError::Backend("a drag is in progress".into())));
            return rx;
        }
        self.start_on(window, request, done);
        rx
    }

    #[allow(
        clippy::needless_pass_by_value,
        reason = "moved into the backend on some targets"
    )]
    #[cfg_attr(not(target_os = "linux"), allow(clippy::unused_self))]
    fn start_on(&self, window: NativeWindow, request: DragRequest, done: DragDone) {
        match window {
            #[cfg(target_os = "linux")]
            NativeWindow::Wayland { display, .. } => {
                let attachments = lock(&self.wayland);
                match attachments.get(&display.get()) {
                    Some(attachment) => attachment.start_drag(&request, done),
                    None => done.finish(Err(DndError::Unsupported)),
                }
            }
            #[cfg(target_os = "linux")]
            NativeWindow::Xlib { .. } | NativeWindow::Xcb { .. } => x11::start_drag(request, done),
            #[cfg(target_os = "macos")]
            NativeWindow::AppKit { ns_view } => macos::start_drag(ns_view, request, done),
            #[cfg(target_os = "windows")]
            NativeWindow::Win32 { hwnd } => win32::start_drag(hwnd, request, done),
            _ => done.finish(Err(DndError::Unsupported)),
        }
    }
}

impl WindowListener for DndPublisher {
    fn window_registered(&self, id: WindowId, window: NativeWindow) -> Result<(), String> {
        self.attach(id, window).map_err(|err| err.to_string())
    }

    fn window_unregistered(&self, id: WindowId) {
        self.detach(id);
    }
}
