//! Declarative driver for one `AdMob` banner.
//!
//! UI code calls [`BannerSlot::sync`] every frame with where the banner
//! should be ([`SlotTarget::Show`]) or that it should be gone
//! ([`SlotTarget::Hide`]). The slot remembers that target and issues the
//! `show_banner` / `update_banner` / `hide_banner` calls needed to reach
//! it, one at a time. A target that changes while a call is in flight is
//! picked up when that call completes, so no transition is lost and
//! rapid rect changes coalesce into a single update carrying the latest
//! rect.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};

use istmo_core::{IstmoError, NativeHandle, NativeHandleId};

use crate::admob::{AdError, AdMobClient, Banner, BannerRect, BannerRequest};

/// Future a [`BannerSlot`] hands to its executor.
pub type BoxFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// Executor hook: runs one [`BoxFuture`] to completion.
pub type SpawnFn = Arc<dyn Fn(BoxFuture) + Send + Sync + 'static>;

/// Where the banner should be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlotTarget {
    /// Banner visible at this rect, in physical pixels.
    Show(BannerRect),
    /// No banner on screen.
    Hide,
}

/// Observable phase of a [`BannerSlot`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotStatus {
    /// No banner and no call in flight.
    Idle,
    /// `show_banner` in flight.
    Loading,
    /// Banner on screen.
    Live,
    /// `hide_banner` in flight.
    Hiding,
}

/// Drives one banner towards the last [`SlotTarget`] it was given.
///
/// Cloning shares the slot. Dropping the last clone while the banner is
/// live releases the native banner through [`NativeHandle`]'s drop.
#[derive(Clone)]
pub struct BannerSlot {
    shared: Arc<Shared>,
    spawn: SpawnFn,
}

impl std::fmt::Debug for BannerSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BannerSlot")
            .field("ad_unit", &&*self.shared.ad_unit)
            .field("status", &self.status())
            .finish_non_exhaustive()
    }
}

struct Shared {
    ad_unit: Arc<str>,
    client: Arc<AdMobClient>,
    inner: Mutex<SlotInner>,
}

struct SlotInner {
    state: SlotState,
    desired: SlotTarget,
    last_error: Option<AdError>,
    updater_running: bool,
}

enum SlotState {
    Idle,
    Loading,
    Live {
        handle: NativeHandle<Banner>,
        /// Rect most recently sent to the native side.
        rect: BannerRect,
    },
    Hiding,
}

/// Work decided under the lock and run after it is released, so an
/// executor that polls inline never re-enters the (non-reentrant) mutex.
enum Action {
    None,
    Show(BannerRect),
    Update(NativeHandleId),
    Hide(NativeHandle<Banner>),
}

impl SlotInner {
    /// Next step towards `desired` from the current state.
    fn reconcile(&mut self) -> Action {
        match (&mut self.state, &self.desired) {
            (SlotState::Idle, SlotTarget::Show(rect)) => {
                let rect = *rect;
                self.state = SlotState::Loading;
                Action::Show(rect)
            }
            (SlotState::Idle, SlotTarget::Hide) | (SlotState::Loading | SlotState::Hiding, _) => {
                Action::None
            }
            (SlotState::Live { rect: current, .. }, SlotTarget::Show(rect)) if current == rect => {
                Action::None
            }
            (SlotState::Live { handle, .. }, SlotTarget::Show(_)) => {
                if self.updater_running {
                    Action::None
                } else {
                    let id = handle.id();
                    self.updater_running = true;
                    Action::Update(id)
                }
            }
            (SlotState::Live { .. }, SlotTarget::Hide) => {
                match std::mem::replace(&mut self.state, SlotState::Hiding) {
                    SlotState::Live { handle, .. } => Action::Hide(handle),
                    other => {
                        self.state = other;
                        Action::None
                    }
                }
            }
        }
    }

    /// Rect the updater should send next for the banner `id`, marking it
    /// as sent. `None` stops the updater: the banner was hidden or
    /// replaced, or it already sits at the desired rect.
    fn next_update(&mut self, id: NativeHandleId) -> Option<BannerRect> {
        let next = match (&mut self.state, &self.desired) {
            (SlotState::Live { handle, rect }, SlotTarget::Show(want))
                if handle.id() == id && rect != want =>
            {
                *rect = *want;
                Some(*want)
            }
            _ => None,
        };
        if next.is_none() {
            self.updater_running = false;
        }
        next
    }

    /// Record an update's outcome; `false` stops the updater. A failed
    /// rect stays recorded as sent so a UI that keeps syncing it does not
    /// retry every frame.
    fn update_done(&mut self, outcome: Result<(), IstmoError>) -> bool {
        match outcome {
            Ok(()) => {
                self.last_error = None;
                true
            }
            Err(err) => {
                self.last_error = Some(classify(&err));
                self.updater_running = false;
                false
            }
        }
    }
}

impl BannerSlot {
    /// Slot for `ad_unit`, running its calls on a fresh thread each (see
    /// [`with_spawn`](Self::with_spawn) to use an executor instead).
    #[must_use]
    pub fn new(ad_unit: impl Into<String>, client: Arc<AdMobClient>) -> Self {
        Self {
            shared: Arc::new(Shared {
                ad_unit: Arc::from(ad_unit.into()),
                client,
                inner: Mutex::new(SlotInner {
                    state: SlotState::Idle,
                    desired: SlotTarget::Hide,
                    last_error: None,
                    updater_running: false,
                }),
            }),
            spawn: default_spawn(),
        }
    }

    /// Run the calls this handle starts, and their follow-ups, on
    /// `spawn` instead of a thread per call.
    #[must_use]
    pub fn with_spawn<F>(mut self, spawn: F) -> Self
    where
        F: Fn(BoxFuture) + Send + Sync + 'static,
    {
        self.spawn = Arc::new(spawn);
        self
    }

    /// Record `target` as where the banner should be and start the next
    /// call towards it if none is in flight. Cheap when nothing changed,
    /// so calling it every frame is fine.
    pub fn sync(&self, target: SlotTarget) {
        let action = {
            let mut guard = self.shared.lock();
            guard.desired = target;
            guard.reconcile()
        };
        self.run(action);
    }

    /// Whether the banner is on screen.
    #[must_use]
    pub fn is_shown(&self) -> bool {
        matches!(self.shared.lock().state, SlotState::Live { .. })
    }

    /// Current phase.
    #[must_use]
    pub fn status(&self) -> SlotStatus {
        match self.shared.lock().state {
            SlotState::Idle => SlotStatus::Idle,
            SlotState::Loading => SlotStatus::Loading,
            SlotState::Live { .. } => SlotStatus::Live,
            SlotState::Hiding => SlotStatus::Hiding,
        }
    }

    /// Error of the most recent call, cleared by the next success.
    #[must_use]
    pub fn last_error(&self) -> Option<AdError> {
        self.shared.lock().last_error.clone()
    }

    fn run(&self, action: Action) {
        let fut: BoxFuture = match action {
            Action::None => return,
            Action::Show(rect) => Box::pin(self.clone().show(rect)),
            Action::Update(id) => Box::pin(self.clone().update(id)),
            Action::Hide(handle) => Box::pin(self.clone().hide(handle)),
        };
        (self.spawn)(fut);
    }

    async fn show(self, rect: BannerRect) {
        let result = self
            .shared
            .client
            .show_banner_owned(BannerRequest {
                ad_unit_id: self.shared.ad_unit.to_string(),
                rect,
            })
            .await;
        let action = {
            let mut guard = self.shared.lock();
            match result {
                Ok(handle) => {
                    guard.last_error = None;
                    guard.state = SlotState::Live { handle, rect };
                }
                Err(err) => {
                    guard.last_error = Some(classify(&err));
                    guard.state = SlotState::Idle;
                    // Retrying here would loop on a persistent failure
                    // (no fill, bad ad unit); the next `sync` retries.
                    guard.desired = SlotTarget::Hide;
                }
            }
            guard.reconcile()
        };
        self.run(action);
    }

    /// Send rect updates for banner `id` until it sits at the desired
    /// rect. Carries the raw id rather than a [`NativeHandle`]: a handle
    /// built here would release the live banner if this future were
    /// dropped mid-call.
    ///
    /// While this runs, `sync` starts no other updater, so on exit the
    /// slot reconciles once more: the banner may have been replaced by
    /// one that is not at the desired rect yet, or the target may have
    /// moved during a failed call.
    async fn update(self, id: NativeHandleId) {
        loop {
            let rect = {
                let mut guard = self.shared.lock();
                match guard.next_update(id) {
                    Some(rect) => rect,
                    None => break,
                }
            };
            let outcome = self.shared.client.update_banner(id, rect).await;
            if !self.shared.lock().update_done(outcome) {
                break;
            }
        }
        let action = self.shared.lock().reconcile();
        self.run(action);
    }

    async fn hide(self, handle: NativeHandle<Banner>) {
        let outcome = self.shared.client.hide_banner_owned(handle).await;
        let action = {
            let mut guard = self.shared.lock();
            guard.last_error = outcome.err().as_ref().map(classify);
            guard.state = SlotState::Idle;
            guard.reconcile()
        };
        self.run(action);
    }
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, SlotInner> {
        istmo_core::sync::lock(&self.inner)
    }
}

/// Convert a rect in logical points to the physical pixels
/// [`BannerRect`] expects, clamping negative coordinates to zero.
#[must_use]
pub fn banner_rect_from_logical(x: f32, y: f32, w: f32, h: f32, scale: f32) -> BannerRect {
    #[expect(
        clippy::cast_sign_loss,
        clippy::cast_possible_truncation,
        reason = "clamped to >= 0 first; `as` saturates past u32::MAX"
    )]
    let px = |v: f32| (v.max(0.0) * scale) as u32;
    BannerRect {
        x: px(x),
        y: px(y),
        width: px(w),
        height: px(h),
    }
}

/// One named thread per call, driven by `pollster`. Calls are rare (a
/// show, a hide, a burst of coalesced updates) and mostly wait on the
/// native side, so a pool would buy nothing.
fn default_spawn() -> SpawnFn {
    Arc::new(|fut: BoxFuture| {
        let spawned = std::thread::Builder::new()
            .name("istmo-admob-slot".to_owned())
            .spawn(move || pollster::block_on(fut));
        if let Err(err) = spawned {
            tracing::error!(target: "istmo.admob", %err, "failed to spawn banner slot thread");
        }
    })
}

fn classify(err: &IstmoError) -> AdError {
    match err {
        IstmoError::PluginError { bytes } => istmo_core::codec::decode::<AdError>(bytes)
            .map_or_else(
                |_| AdError::Internal(format!("undecodable domain error ({} bytes)", bytes.len())),
                |(decoded, _)| decoded,
            ),
        other => AdError::Internal(other.to_string()),
    }
}
