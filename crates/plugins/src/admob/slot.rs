// `significant_drop_tightening` fires on `let mut guard = mutex.lock();`
// scopes that already release the guard at their block end, which is
// exactly the RAII pattern we want. Not a real footgun here.
#![allow(clippy::significant_drop_tightening)]

//! UI-agnostic banner slot.
//!
//! A `BannerSlot` glues an `AdMobClient` to a rectangle that some UI
//! framework decides where to place. Every frame the UI code:
//!
//! 1. Computes the rectangle it wants the banner to occupy, in
//!    **physical pixels** relative to the app window.
//! 2. Decides visibility (is the rectangle currently inside the
//!    scroll viewport / not covered by a modal / not off-screen).
//! 3. Calls [`BannerSlot::sync`] with the resulting [`SlotTarget`].
//!
//! The slot owns the ad's lifecycle — load, update, hide — and
//! deduplicates calls: sending the same rect twice in a row is a
//! no-op, so the per-frame cost is one mutex lock and a rect compare.
//!
//! The slot is UI-framework agnostic on purpose. `BannerRect` is the
//! only currency it speaks. Any framework can provide a two-line
//! adapter — see [`banner_rect_from_logical`] for the standard
//! conversion from logical-unit rectangles.
//!
//! # Example (framework-agnostic)
//!
//! ```ignore
//! let slot = BannerSlot::new(BANNER_UNIT);
//!
//! // In your per-frame loop:
//! let rect_px = banner_rect_from_logical(50.0, 800.0, 320.0, 50.0, scale);
//! slot.sync(&client, if visible {
//!     SlotTarget::Show(rect_px)
//! } else {
//!     SlotTarget::Hide
//! });
//! ```
//!
//! # Async execution
//!
//! The show / update / hide calls are async. `BannerSlot` runs them on
//! a caller-provided [`SpawnFn`]. The default spawns a dedicated OS
//! thread per action that blocks on [`pollster::block_on`] — no
//! executor assumption. Users on tokio / async-std can inject their
//! own [`Self::with_spawn`].

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use istmo_core::{IstmoError, NativeHandle};

use crate::admob::{AdError, AdMobClient, Banner, BannerRect, BannerRequest};

/// Owned, boxed, `Send`-able future the slot hands to [`SpawnFn`].
pub type BoxFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// Executor abstraction: run this future to completion on some worker.
/// Default is a fresh OS thread per invocation blocking on `pollster`.
pub type SpawnFn = Arc<dyn Fn(BoxFuture) + Send + Sync + 'static>;

/// What the UI wants the slot to do this frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlotTarget {
    /// Show a banner at exactly this rectangle. If none is currently
    /// shown, this loads one. If one is shown at a different rectangle,
    /// it is moved. Idempotent when the rectangle matches the current
    /// one.
    Show(BannerRect),
    /// Hide any currently-shown banner. No-op when idle.
    Hide,
}

/// Public snapshot of the slot's lifecycle state. Framework adapters
/// use this to render a spinner / error overlay while the ad is
/// loading, or to hide the reserved rectangle when the ad failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotStatus {
    /// No banner requested. UI's rectangle is dark.
    Idle,
    /// Show requested; waiting for `AdMob` to return a handle.
    Loading,
    /// Banner live at the last-known rectangle.
    Live,
    /// Hide in flight. Slot returns to `Idle` when it completes.
    Hiding,
}

/// UI-agnostic banner slot. Cheap to `Clone` — internally
/// `Arc`-backed, safe to share across threads and frames.
#[derive(Clone)]
pub struct BannerSlot {
    ad_unit: Arc<str>,
    client: Arc<AdMobClient>,
    inner: Arc<Mutex<SlotInner>>,
    spawn: SpawnFn,
}

impl std::fmt::Debug for BannerSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BannerSlot")
            .field("ad_unit", &&*self.ad_unit)
            .field("status", &self.status())
            .finish_non_exhaustive()
    }
}

struct SlotInner {
    state: SlotState,
    last_error: Option<AdError>,
    /// Latest rect the UI wants — set by `sync()` whenever a new rect
    /// arrives during `Live`. The updater loop consumes it after its
    /// current native update finishes. `None` means "no work pending".
    pending_rect: Option<BannerRect>,
    /// `true` while an `update_banner` task is spawned; prevents
    /// spawning parallel updaters on fast scroll. The updater clears
    /// this itself when it exits because `pending_rect` is empty.
    updater_running: bool,
}

enum SlotState {
    Idle,
    Loading,
    Live {
        handle: NativeHandle<Banner>,
        rect: BannerRect,
    },
    Hiding,
}

impl BannerSlot {
    /// Create a new slot for the given ad unit id.
    ///
    /// `client` is stored as an `Arc<AdMobClient>` so multiple slots
    /// can share the same underlying `AdMob` instance (typical when
    /// the app has several banner placements). Uses the default
    /// `std::thread::spawn` + `pollster::block_on` executor.
    #[must_use]
    pub fn new(ad_unit: impl Into<String>, client: Arc<AdMobClient>) -> Self {
        Self {
            ad_unit: Arc::from(ad_unit.into()),
            client,
            inner: Arc::new(Mutex::new(SlotInner {
                state: SlotState::Idle,
                last_error: None,
                pending_rect: None,
                updater_running: false,
            })),
            spawn: default_spawn(),
        }
    }

    /// Override the executor. Useful when the host process runs a
    /// single-threaded tokio runtime, an actor system, or a bespoke
    /// scheduler.
    #[must_use]
    pub fn with_spawn<F>(mut self, spawn: F) -> Self
    where
        F: Fn(BoxFuture) + Send + Sync + 'static,
    {
        self.spawn = Arc::new(spawn);
        self
    }

    /// Reconcile the current lifecycle state with `target`. Cheap to
    /// call every frame — a no-op when the current state already
    /// matches.
    pub fn sync(&self, target: SlotTarget) {
        // Compute the transition under the lock, then release before
        // invoking `spawn`. The injected `SpawnFn` may run the future
        // inline (e.g. tests using a foreground executor); holding the
        // guard across spawn would deadlock when the future re-locks
        // `inner` — `std::sync::Mutex` is not reentrant.
        enum Action {
            None,
            Show(BannerRect),
            Updater(istmo_core::NativeHandleId),
            Hide(NativeHandle<Banner>),
        }
        let action = {
            let mut guard = self.inner.lock().expect("slot mutex");
            match (&guard.state, target) {
                (SlotState::Idle, SlotTarget::Show(rect)) => {
                    guard.state = SlotState::Loading;
                    Action::Show(rect)
                }
                (SlotState::Live { rect: current, .. }, SlotTarget::Show(rect))
                    if *current == rect =>
                {
                    // Rect matches — nothing to do. Common per-frame path.
                    Action::None
                }
                (SlotState::Live { handle, .. }, SlotTarget::Show(rect)) => {
                    // Move only — mutate the rect in place. Never replace
                    // the handle: dropping the old one would fire
                    // `ReleaseNativeHandle` even though the native banner
                    // is still alive.
                    let handle_id = handle.id();
                    if let SlotState::Live { rect: current, .. } = &mut guard.state {
                        *current = rect;
                    }
                    // Coalesce: park the latest rect and only spawn an
                    // updater if none is running. On fast scroll (many
                    // rects per second), intermediate values are dropped
                    // — the updater always converges to the newest
                    // position instead of queueing every step behind
                    // slow JNI hops.
                    guard.pending_rect = Some(rect);
                    if guard.updater_running {
                        Action::None
                    } else {
                        guard.updater_running = true;
                        Action::Updater(handle_id)
                    }
                }
                (SlotState::Live { .. }, SlotTarget::Hide) => {
                    if let SlotState::Live { handle, .. } =
                        std::mem::replace(&mut guard.state, SlotState::Hiding)
                    {
                        Action::Hide(handle)
                    } else {
                        Action::None
                    }
                }
                (SlotState::Loading, SlotTarget::Show(rect)) => {
                    // Show already in flight — park the latest rect so
                    // the load-completion handler can apply it
                    // immediately without a visible jump on first paint.
                    guard.pending_rect = Some(rect);
                    Action::None
                }
                // Hiding / (Idle+Hide) → no-op. Hide completes
                // asynchronously; a later `sync(Show)` will re-enter
                // Idle and load a fresh banner.
                _ => Action::None,
            }
        };
        match action {
            Action::None => {}
            Action::Show(rect) => self.spawn_show(rect),
            Action::Updater(handle_id) => self.spawn_updater_loop(handle_id),
            Action::Hide(handle) => self.spawn_hide(handle),
        }
    }

    /// True when the banner is currently visible on screen.
    #[must_use]
    pub fn is_shown(&self) -> bool {
        matches!(
            self.inner.lock().expect("slot mutex").state,
            SlotState::Live { .. }
        )
    }

    /// Lifecycle snapshot for the UI (e.g. render a placeholder while
    /// `Loading`).
    #[must_use]
    pub fn status(&self) -> SlotStatus {
        match self.inner.lock().expect("slot mutex").state {
            SlotState::Idle => SlotStatus::Idle,
            SlotState::Loading => SlotStatus::Loading,
            SlotState::Live { .. } => SlotStatus::Live,
            SlotState::Hiding => SlotStatus::Hiding,
        }
    }

    /// Last surfaced load / update / hide error, if any. Cleared on
    /// the next successful transition.
    #[must_use]
    pub fn last_error(&self) -> Option<AdError> {
        self.inner.lock().expect("slot mutex").last_error.clone()
    }

    fn spawn_show(&self, rect: BannerRect) {
        let inner = self.inner.clone();
        let client = self.client.clone();
        let ad_unit = self.ad_unit.to_string();
        let after_show_spawn = self.spawn.clone();
        let after_show_client = self.client.clone();
        let fut: BoxFuture = Box::pin(async move {
            let result = client
                .show_banner_owned(BannerRequest {
                    ad_unit_id: ad_unit,
                    rect,
                })
                .await;
            let (needs_updater, handle_id) = {
                let mut guard = inner.lock().expect("slot mutex");
                match result {
                    Ok(handle) => {
                        let handle_id = handle.id();
                        guard.last_error = None;
                        guard.state = SlotState::Live { handle, rect };
                        // Was the UI already scrolling while we loaded?
                        // Stale rect sits in `pending_rect`. Kick the
                        // updater so the first paint lands at the
                        // correct position — no post-load jump.
                        let spawn_it = guard.pending_rect.is_some() && !guard.updater_running;
                        if spawn_it {
                            guard.updater_running = true;
                        }
                        (spawn_it, handle_id)
                    }
                    Err(err) => {
                        guard.last_error = Some(classify(&err));
                        guard.state = SlotState::Idle;
                        guard.pending_rect = None;
                        (false, istmo_core::NativeHandleId(0))
                    }
                }
            };
            if needs_updater {
                let fut: BoxFuture =
                    Box::pin(run_updater_loop(inner, after_show_client, handle_id));
                (after_show_spawn)(fut);
            }
        });
        (self.spawn)(fut);
    }

    /// Kick off the updater coroutine. Caller must have already set
    /// `updater_running = true` under the mutex to claim the slot;
    /// [`run_updater_loop`] clears it on exit.
    fn spawn_updater_loop(&self, handle_id: istmo_core::NativeHandleId) {
        let fut: BoxFuture = Box::pin(run_updater_loop(
            self.inner.clone(),
            self.client.clone(),
            handle_id,
        ));
        (self.spawn)(fut);
    }

    fn spawn_hide(&self, handle: NativeHandle<Banner>) {
        let inner = self.inner.clone();
        let client = self.client.clone();
        let fut: BoxFuture = Box::pin(async move {
            let outcome = client.hide_banner_owned(handle).await;
            let mut guard = inner.lock().expect("slot mutex");
            if let Err(err) = outcome {
                guard.last_error = Some(classify(&err));
            } else {
                guard.last_error = None;
            }
            guard.state = SlotState::Idle;
        });
        (self.spawn)(fut);
    }
}

impl Drop for BannerSlot {
    fn drop(&mut self) {
        // If the outer Arc reaches 1 while a Live handle is still in the
        // state, the handle's own Drop fires ReleaseNativeHandle
        // automatically. No explicit hide needed here — the native side
        // treats an unknown release as a no-op if the popup was already
        // torn down.
    }
}

/// Convert a logical-unit rectangle into physical pixels.
///
/// Physical pixels are the coordinate space every UI framework's banner
/// adapter has to end up in. `scale` is the framework's density factor
/// (egui `pixels_per_point`, iced `viewport.scale_factor()`, slint
/// `Window::scale_factor()`, …).
#[must_use]
pub fn banner_rect_from_logical(x: f32, y: f32, w: f32, h: f32, scale: f32) -> BannerRect {
    let clamp = |v: f32| v.max(0.0);
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
    BannerRect {
        x: (clamp(x) * scale) as u32,
        y: (clamp(y) * scale) as u32,
        width: (clamp(w) * scale) as u32,
        height: (clamp(h) * scale) as u32,
    }
}

/// The coalescing update loop. Drains `pending_rect` one call at a
/// time; on fast scroll, intermediate rects set by `sync()` while a
/// call is in flight are silently overwritten. Latest rect always
/// wins. Exits when `pending_rect` is None on entry, releasing the
/// `updater_running` claim so a later scroll can spawn a fresh loop.
async fn run_updater_loop(
    inner: Arc<Mutex<SlotInner>>,
    client: Arc<AdMobClient>,
    handle_id: istmo_core::NativeHandleId,
) {
    let runtime = client.runtime().clone();
    loop {
        let rect = {
            let mut guard = inner.lock().expect("slot mutex");
            let Some(r) = guard.pending_rect.take() else {
                guard.updater_running = false;
                return;
            };
            r
        };
        // Borrow-adopt for this call only. `into_id` suppresses the
        // drop-cascade release — the real handle stays alive inside
        // `SlotState::Live { handle, .. }`.
        let borrowed = NativeHandle::<Banner>::adopt(&runtime, handle_id);
        let outcome = client.update_banner_owned(&borrowed, rect).await;
        let _ = borrowed.into_id();
        let mut guard = inner.lock().expect("slot mutex");
        if let Err(err) = outcome {
            guard.last_error = Some(classify(&err));
            // Persistent error → stop looping to avoid busy-spin against
            // a broken banner. UI can reset by dropping the slot.
            guard.pending_rect = None;
            guard.updater_running = false;
            return;
        }
        guard.last_error = None;
    }
}

fn default_spawn() -> SpawnFn {
    Arc::new(|fut: BoxFuture| {
        std::thread::spawn(move || pollster::block_on(fut));
    })
}

/// Decode an `IstmoError::PluginError { bytes }` into the typed
/// `AdError`. Non-plugin errors surface as `AdError::Internal` with the
/// wire error's Display representation.
fn classify(err: &IstmoError) -> AdError {
    match err {
        IstmoError::PluginError { bytes } => {
            match istmo_core::codec::decode::<AdError>(bytes) {
                Ok((decoded, _)) => decoded,
                Err(_) => AdError::Internal(format!(
                    "undecodable domain error ({} bytes)",
                    bytes.len(),
                )),
            }
        }
        other => AdError::Internal(other.to_string()),
    }
}
