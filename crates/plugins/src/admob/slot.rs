#![allow(clippy::significant_drop_tightening)]

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use istmo_core::{IstmoError, NativeHandle};

use crate::admob::{AdError, AdMobClient, Banner, BannerRect, BannerRequest};

pub type BoxFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

pub type SpawnFn = Arc<dyn Fn(BoxFuture) + Send + Sync + 'static>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlotTarget {
    Show(BannerRect),

    Hide,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotStatus {
    Idle,

    Loading,

    Live,

    Hiding,
}

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

    pending_rect: Option<BannerRect>,

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

    #[must_use]
    pub fn with_spawn<F>(mut self, spawn: F) -> Self
    where
        F: Fn(BoxFuture) + Send + Sync + 'static,
    {
        self.spawn = Arc::new(spawn);
        self
    }

    pub fn sync(&self, target: SlotTarget) {
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
                    Action::None
                }
                (SlotState::Live { handle, .. }, SlotTarget::Show(rect)) => {
                    let handle_id = handle.id();
                    if let SlotState::Live { rect: current, .. } = &mut guard.state {
                        *current = rect;
                    }

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
                    guard.pending_rect = Some(rect);
                    Action::None
                }

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

    #[must_use]
    pub fn is_shown(&self) -> bool {
        matches!(
            self.inner.lock().expect("slot mutex").state,
            SlotState::Live { .. }
        )
    }

    #[must_use]
    pub fn status(&self) -> SlotStatus {
        match self.inner.lock().expect("slot mutex").state {
            SlotState::Idle => SlotStatus::Idle,
            SlotState::Loading => SlotStatus::Loading,
            SlotState::Live { .. } => SlotStatus::Live,
            SlotState::Hiding => SlotStatus::Hiding,
        }
    }

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
    fn drop(&mut self) {}
}

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

        let borrowed = NativeHandle::<Banner>::adopt(&runtime, handle_id);
        let outcome = client.update_banner_owned(&borrowed, rect).await;
        let _ = borrowed.into_id();
        let mut guard = inner.lock().expect("slot mutex");
        if let Err(err) = outcome {
            guard.last_error = Some(classify(&err));

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

fn classify(err: &IstmoError) -> AdError {
    match err {
        IstmoError::PluginError { bytes } => match istmo_core::codec::decode::<AdError>(bytes) {
            Ok((decoded, _)) => decoded,
            Err(_) => {
                AdError::Internal(format!("undecodable domain error ({} bytes)", bytes.len()))
            }
        },
        other => AdError::Internal(other.to_string()),
    }
}
