//! The platform-independent half of every tray host: item and icon
//! tables, validation, decoding, change coalescing, menu activation,
//! event delivery and the animation timer. A [`Backend`] only mirrors
//! [`State`] onto the platform and reports what the user did.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, Weak};
use std::time::{Duration, Instant};

use istmo_core::sync::lock;
use istmo_core::{HostCtx, NativeHandleId, Runtime};
use istmo_menu::{
    ActivationEffect, IconImage, IconRef, ItemChange, ItemId, ItemPatch, Menu, MenuDiff, MenuNode,
};

#[cfg(not(target_arch = "wasm32"))]
use crate::image::{DecodedIcon, MAX_FRAMES};
#[cfg(target_arch = "wasm32")]
use crate::web::{DecodedIcon, MAX_FRAMES};
use crate::{
    CheckChange, HostState, LimitKind, PrimaryAction, RadioChange, RegisteredIcon, Repeat, Support,
    Tooltip, TrayCapabilities, TrayError, TrayEvent, TrayIcon, TrayItem, TraySpec,
};

/// Longest title / tooltip text accepted, in bytes.
const MAX_TEXT_BYTES: usize = 4096;
/// Events kept for a tray nobody listens to yet.
const BACKLOG: usize = 64;
/// Events a subscriber may fall behind before newer ones are dropped.
const SUBSCRIBER_DEPTH: usize = 1024;

/// The platform half of a host.
pub(crate) trait Backend: Send + Sync + 'static {
    fn capabilities(&self) -> TrayCapabilities;

    /// [`State`] has pending changes ([`Shared::take_pending`]).
    fn wake(&self);
}

/// Starts a backend over the shared state.
pub(crate) type StartBackend = fn(&Arc<Shared>) -> Result<Arc<dyn Backend>, TrayError>;

/// An icon with its animation state.
#[derive(Debug, Clone)]
pub(crate) struct IconState {
    pub(crate) frames: Vec<(Arc<DecodedIcon>, Duration)>,
    pub(crate) repeat: Repeat,
    pub(crate) current: usize,
    /// Runs finished so far, for [`Repeat::Times`].
    runs: u32,
    next_at: Option<Instant>,
}

impl IconState {
    fn new(frames: Vec<(Arc<DecodedIcon>, Duration)>, repeat: Repeat) -> Self {
        let animated = frames.len() > 1 && repeat != Repeat::Times(0);
        // On the web the browser runs animations (`Instant::now` panics
        // on wasm32-unknown-unknown).
        let next_at =
            (animated && !cfg!(target_arch = "wasm32")).then(|| Instant::now() + frames[0].1);
        Self {
            frames,
            repeat,
            current: 0,
            runs: 0,
            next_at,
        }
    }

    /// The frame on show.
    pub(crate) fn frame(&self) -> &Arc<DecodedIcon> {
        &self.frames[self.current].0
    }

    /// Moves to the next frame when its time has come. Returns whether
    /// the frame changed.
    fn advance(&mut self, now: Instant) -> bool {
        let Some(due) = self.next_at else {
            return false;
        };
        if now < due {
            return false;
        }
        let next = self.current + 1;
        if next < self.frames.len() {
            self.current = next;
        } else {
            self.runs = self.runs.saturating_add(1);
            if let Repeat::Times(times) = self.repeat
                && self.runs >= times
            {
                self.next_at = None;
                return false;
            }
            self.current = 0;
        }
        // Catch up from the due time, not from `now`, so a late tick
        // does not stretch the animation; a very late one restarts it.
        let step = self.frames[self.current].1;
        let next_due = due + step;
        self.next_at = Some(if next_due < now { now + step } else { next_due });
        true
    }
}

/// One tray item as the host knows it.
#[derive(Debug)]
pub(crate) struct TrayState {
    /// Creation order, for backends that need a small number.
    #[cfg_attr(
        any(target_os = "macos", target_arch = "wasm32"),
        expect(dead_code, reason = "status items and the favicon need no number")
    )]
    pub(crate) serial: u32,
    pub(crate) id: String,
    pub(crate) icon: IconState,
    pub(crate) title: Option<String>,
    pub(crate) tooltip: Option<Tooltip>,
    pub(crate) visible: bool,
    pub(crate) primary: PrimaryAction,
    #[cfg_attr(
        not(target_os = "windows"),
        expect(
            dead_code,
            reason = "only the Windows backend identifies icons by GUID"
        )
    )]
    pub(crate) windows_guid: Option<String>,
    pub(crate) menu: Menu,
    pub(crate) item_icons: HashMap<ItemId, Arc<DecodedIcon>>,
    events: EventHub,
}

/// What changed in one tray since the backend last looked.
#[derive(Debug, Default, Clone, PartialEq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "one independent dirty flag per property"
)]
pub(crate) struct Dirty {
    pub(crate) added: bool,
    pub(crate) removed: bool,
    pub(crate) icon: bool,
    pub(crate) title: bool,
    pub(crate) tooltip: bool,
    pub(crate) visible: bool,
    pub(crate) menu: MenuDirty,
}

/// How the menu changed.
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) enum MenuDirty {
    #[default]
    Clean,
    /// These properties, in order.
    Patches(Vec<ItemPatch>),
    /// Anything else: rebuild from [`TrayState::menu`].
    Rebuild,
}

impl MenuDirty {
    fn push(&mut self, patches: Vec<ItemPatch>) {
        if patches.is_empty() {
            return;
        }
        match self {
            Self::Clean => *self = Self::Patches(patches),
            Self::Patches(pending) => pending.extend(patches),
            Self::Rebuild => {}
        }
    }
}

/// Everything a host owns, behind one lock.
#[derive(Debug, Default)]
pub(crate) struct State {
    pub(crate) trays: BTreeMap<NativeHandleId, TrayState>,
    icons: HashMap<NativeHandleId, Arc<DecodedIcon>>,
    pending: BTreeMap<NativeHandleId, Dirty>,
    pub(crate) host: Option<HostState>,
    next_serial: u32,
}

impl State {
    fn dirty(&mut self, tray: NativeHandleId) -> &mut Dirty {
        self.pending.entry(tray).or_default()
    }
}

/// State shared by the host, its backend and the animation timer.
#[derive(Default)]
pub(crate) struct Shared {
    state: Mutex<State>,
    backend: OnceLock<Weak<dyn Backend>>,
    timer: OnceLock<flume::Sender<()>>,
}

impl std::fmt::Debug for Shared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shared").finish_non_exhaustive()
    }
}

impl Shared {
    pub(crate) fn lock(&self) -> MutexGuard<'_, State> {
        lock(&self.state)
    }

    /// The changes since the last call, per tray in creation order. A
    /// tray added and removed in between is left out.
    pub(crate) fn take_pending(&self) -> Vec<(NativeHandleId, Dirty)> {
        std::mem::take(&mut self.lock().pending)
            .into_iter()
            .filter(|(_, dirty)| !(dirty.added && dirty.removed))
            .collect()
    }

    fn wake_backend(&self) {
        if let Some(backend) = self.backend.get().and_then(Weak::upgrade) {
            backend.wake();
        }
    }

    /// Reports a click on an item: runs its automatic state change and
    /// emits the events.
    pub(crate) fn activate_item(&self, tray: NativeHandleId, item: ItemId) {
        let mut state = self.lock();
        let Some(entry) = state.trays.get_mut(&tray) else {
            return;
        };
        let Some(activation) = entry.menu.activate(item) else {
            return;
        };
        entry.events.emit(TrayEvent::ItemActivated(item));
        match activation.effect {
            ActivationEffect::Checked(state) => {
                entry
                    .events
                    .emit(TrayEvent::CheckChanged(CheckChange { item, state }));
            }
            ActivationEffect::Selected(group) => {
                entry
                    .events
                    .emit(TrayEvent::RadioSelected(RadioChange { group, item }));
            }
            // `None`, and effects added to the model later.
            _ => {}
        }
        if activation.patches.is_empty() {
            return;
        }
        state.dirty(tray).menu.push(activation.patches);
        drop(state);
        self.wake_backend();
    }

    pub(crate) fn emit(&self, tray: NativeHandleId, event: TrayEvent) {
        if let Some(entry) = self.lock().trays.get_mut(&tray) {
            entry.events.emit(event);
        }
    }

    /// Records the tray host's state and tells every tray when it
    /// changed.
    pub(crate) fn set_host(&self, host: HostState) {
        let mut state = self.lock();
        if state.host == Some(host) {
            return;
        }
        let first = state.host.is_none();
        state.host = Some(host);
        if first && host == HostState::Available {
            return;
        }
        for entry in state.trays.values_mut() {
            entry.events.emit(TrayEvent::Host(host));
        }
    }

    /// Marks every tray for a full re-add (the tray host restarted).
    #[cfg_attr(
        not(target_os = "windows"),
        expect(dead_code, reason = "the other backends re-register without a re-add")
    )]
    pub(crate) fn readd_all(&self) {
        let mut state = self.lock();
        let ids: Vec<_> = state.trays.keys().copied().collect();
        for id in ids {
            let dirty = state.dirty(id);
            dirty.added = true;
            dirty.removed = false;
        }
        drop(state);
        self.wake_backend();
    }

    /// Advances every animation that is due; returns when the next one
    /// is due.
    fn tick(&self, now: Instant) -> Option<Instant> {
        let mut state = self.lock();
        let mut next: Option<Instant> = None;
        let mut changed = Vec::new();
        for (&id, entry) in &mut state.trays {
            if entry.icon.advance(now) {
                changed.push(id);
            }
            if let Some(due) = entry.icon.next_at {
                next = Some(next.map_or(due, |n| n.min(due)));
            }
        }
        let any = !changed.is_empty();
        for id in changed {
            state.dirty(id).icon = true;
        }
        drop(state);
        if any {
            self.wake_backend();
        }
        next
    }

    fn start_timer(self: &Arc<Self>) {
        let sender = self.timer.get_or_init(|| {
            let (tx, rx) = flume::unbounded::<()>();
            let shared = Arc::downgrade(self);
            let spawned = std::thread::Builder::new()
                .name("istmo-tray-anim".to_owned())
                .spawn(move || animate(&shared, &rx));
            if let Err(err) = spawned {
                tracing::warn!(%err, "tray: the animation timer could not start");
            }
            tx
        });
        let _woken = sender.send(());
    }
}

/// The animation loop: sleeps until the next frame is due or a new
/// animation starts; ends when the host is gone.
fn animate(shared: &Weak<Shared>, wake: &flume::Receiver<()>) {
    loop {
        let Some(shared) = shared.upgrade() else {
            return;
        };
        let next = shared.tick(Instant::now());
        drop(shared);
        let woken = next.map_or_else(
            || {
                wake.recv()
                    .map_err(|_| flume::RecvTimeoutError::Disconnected)
            },
            |due| wake.recv_deadline(due),
        );
        if woken == Err(flume::RecvTimeoutError::Disconnected) {
            return;
        }
    }
}

/// Subscribers of one tray's events, with a backlog until the first
/// one arrives.
#[derive(Debug, Default)]
struct EventHub {
    backlog: VecDeque<TrayEvent>,
    subscribers: Vec<flume::Sender<Result<TrayEvent, TrayError>>>,
    subscribed: bool,
}

impl EventHub {
    fn emit(&mut self, event: TrayEvent) {
        if !self.subscribed {
            if self.backlog.len() == BACKLOG {
                self.backlog.pop_front();
            }
            self.backlog.push_back(event);
            return;
        }
        self.subscribers
            .retain(|tx| match tx.try_send(Ok(event.clone())) {
                Ok(()) => true,
                Err(flume::TrySendError::Full(_)) => {
                    tracing::warn!("tray: an event subscriber is not reading; event dropped");
                    true
                }
                Err(flume::TrySendError::Disconnected(_)) => false,
            });
    }

    fn subscribe(&mut self) -> flume::Receiver<Result<TrayEvent, TrayError>> {
        let (tx, rx) = flume::bounded(SUBSCRIBER_DEPTH);
        if !self.subscribed {
            self.subscribed = true;
            for event in self.backlog.drain(..) {
                let _queued = tx.try_send(Ok(event));
            }
        }
        self.subscribers.push(tx);
        rx
    }
}

/// The generic host every platform host wraps.
pub(crate) struct Host {
    shared: Arc<Shared>,
    start: StartBackend,
    backend: OnceLock<Result<Arc<dyn Backend>, TrayError>>,
    hook: OnceLock<()>,
}

impl std::fmt::Debug for Host {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Host").finish_non_exhaustive()
    }
}

impl Host {
    pub(crate) fn new(start: StartBackend) -> Self {
        Self {
            shared: Arc::default(),
            start,
            backend: OnceLock::new(),
            hook: OnceLock::new(),
        }
    }

    /// The backend, started on first use.
    fn backend(&self) -> Result<&Arc<dyn Backend>, TrayError> {
        self.backend
            .get_or_init(|| {
                let backend = (self.start)(&self.shared)?;
                let weak = Arc::downgrade(&backend);
                let _set = self.shared.backend.set(weak);
                Ok(backend)
            })
            .as_ref()
            .map_err(Clone::clone)
    }

    pub(crate) fn capabilities(&self) -> TrayCapabilities {
        match self.backend() {
            Ok(backend) => {
                let mut caps = backend.capabilities();
                let host = self.shared.lock().host;
                if let Some(host) = host {
                    caps.host = host;
                }
                caps
            }
            Err(err) => {
                tracing::warn!(%err, "tray: backend unavailable");
                TrayCapabilities::none(crate::TrayBackend::Unavailable)
            }
        }
    }

    fn install_release_hook(&self, ctx: &HostCtx) -> Result<Arc<Runtime>, TrayError> {
        let runtime = ctx.runtime().map_err(|_| TrayError::ShutDown)?;
        self.hook.get_or_init(|| {
            let shared = Arc::downgrade(&self.shared);
            runtime.install_native_handle_release_hook(Arc::new(move |id| {
                // Every release in the runtime reaches every hook; ids
                // this host never handed out are in neither table.
                if let Some(shared) = shared.upgrade() {
                    remove_tray(&shared, id);
                    shared.lock().icons.remove(&id);
                }
            }));
        });
        Ok(runtime)
    }

    pub(crate) fn register_icon(
        &self,
        ctx: &HostCtx,
        image: &IconImage,
    ) -> Result<RegisteredIcon, TrayError> {
        let caps = self.backend()?.capabilities();
        let decoded = Arc::new(DecodedIcon::decode(image, 0, names(&caps))?);
        let runtime = self.install_release_hook(ctx)?;
        let id = runtime.alloc_native_handle_id();
        self.shared.lock().icons.insert(id, decoded);
        Ok(RegisteredIcon { icon: id })
    }

    pub(crate) fn create(&self, ctx: &HostCtx, spec: TraySpec) -> Result<TrayItem, TrayError> {
        let caps = self.backend()?.capabilities();
        check_text(spec.title.as_deref())?;
        check_tooltip(spec.tooltip.as_ref())?;
        let icon = self.icon_state(&spec.icon, &caps)?;
        let menu = Menu::try_from(spec.menu)?;
        let item_icons = self.item_icons(menu.nodes(), &caps)?;
        let runtime = self.install_release_hook(ctx)?;
        let mut state = self.shared.lock();
        if let Some(max) = caps.max_items
            && state.trays.len() >= max as usize
        {
            return Err(TrayError::Limit(LimitKind::Items));
        }
        let id = runtime.alloc_native_handle_id();
        let serial = state.next_serial;
        state.next_serial = serial.wrapping_add(1);
        let animated = icon.next_at.is_some();
        state.trays.insert(
            id,
            TrayState {
                serial,
                id: spec.id,
                icon,
                title: spec.title,
                tooltip: spec.tooltip,
                visible: true,
                primary: spec.primary,
                windows_guid: spec.windows_guid,
                menu,
                item_icons,
                events: EventHub::default(),
            },
        );
        state.dirty(id).added = true;
        drop(state);
        if animated {
            self.shared.start_timer();
        }
        self.shared.wake_backend();
        Ok(TrayItem { tray: id })
    }

    pub(crate) fn set_icon(&self, tray: NativeHandleId, icon: &TrayIcon) -> Result<(), TrayError> {
        let caps = self.backend()?.capabilities();
        let icon = self.icon_state(icon, &caps)?;
        let animated = icon.next_at.is_some();
        self.update(tray, |entry, dirty| {
            entry.icon = icon;
            dirty.icon = true;
        })?;
        if animated {
            self.shared.start_timer();
        }
        Ok(())
    }

    pub(crate) fn set_title(
        &self,
        tray: NativeHandleId,
        title: Option<String>,
    ) -> Result<(), TrayError> {
        check_text(title.as_deref())?;
        self.update(tray, |entry, dirty| {
            dirty.title |= entry.title != title;
            entry.title = title;
        })
    }

    pub(crate) fn set_tooltip(
        &self,
        tray: NativeHandleId,
        tooltip: Option<Tooltip>,
    ) -> Result<(), TrayError> {
        check_tooltip(tooltip.as_ref())?;
        self.update(tray, |entry, dirty| {
            dirty.tooltip |= entry.tooltip != tooltip;
            entry.tooltip = tooltip;
        })
    }

    pub(crate) fn set_visible(&self, tray: NativeHandleId, visible: bool) -> Result<(), TrayError> {
        self.update(tray, |entry, dirty| {
            dirty.visible |= entry.visible != visible;
            entry.visible = visible;
        })
    }

    pub(crate) fn set_menu(
        &self,
        tray: NativeHandleId,
        nodes: Vec<MenuNode>,
    ) -> Result<(), TrayError> {
        let caps = self.backend()?.capabilities();
        let menu = Menu::try_from(nodes)?;
        let item_icons = self.item_icons(menu.nodes(), &caps)?;
        self.update(tray, |entry, dirty| {
            match entry.menu.diff(&menu) {
                MenuDiff::Same => {}
                MenuDiff::Patches(patches) => dirty.menu.push(patches),
                // A future diff kind rebuilds, which is always correct.
                _ => dirty.menu = MenuDirty::Rebuild,
            }
            entry.menu = menu;
            entry.item_icons = item_icons;
        })
    }

    pub(crate) fn update_items(
        &self,
        tray: NativeHandleId,
        patches: &[ItemPatch],
    ) -> Result<(), TrayError> {
        let caps = self.backend()?.capabilities();
        // Decode new item icons before touching the menu, so a bad icon
        // leaves it as it was.
        let mut icons = Vec::new();
        for patch in patches {
            if let ItemChange::Icon(Some(icon)) = &patch.change {
                icons.push((patch.id, self.resolve(icon, 0, &caps)?));
            }
        }
        let mut result = Ok(());
        self.update(tray, |entry, dirty| match entry.menu.apply(patches) {
            Ok(applied) => {
                for patch in patches {
                    if patch.change == ItemChange::Icon(None) {
                        entry.item_icons.remove(&patch.id);
                    }
                }
                entry.item_icons.extend(icons);
                dirty.menu.push(applied);
            }
            Err(err) => result = Err(TrayError::from(err)),
        })?;
        result
    }

    pub(crate) fn destroy(&self, tray: NativeHandleId) -> Result<(), TrayError> {
        if remove_tray(&self.shared, tray) {
            Ok(())
        } else {
            Err(TrayError::UnknownTray)
        }
    }

    pub(crate) fn events(
        &self,
        tray: NativeHandleId,
    ) -> flume::Receiver<Result<TrayEvent, TrayError>> {
        let subscribed = self
            .shared
            .lock()
            .trays
            .get_mut(&tray)
            .map(|entry| entry.events.subscribe());
        if let Some(rx) = subscribed {
            return rx;
        }
        let (tx, rx) = flume::bounded(1);
        let _queued = tx.try_send(Err(TrayError::UnknownTray));
        rx
    }

    /// Runs `change` on the tray and wakes the backend.
    fn update(
        &self,
        tray: NativeHandleId,
        change: impl FnOnce(&mut TrayState, &mut Dirty),
    ) -> Result<(), TrayError> {
        let mut state = self.shared.lock();
        let State { trays, pending, .. } = &mut *state;
        let entry = trays.get_mut(&tray).ok_or(TrayError::UnknownTray)?;
        let dirty = pending.entry(tray).or_default();
        change(entry, dirty);
        if *dirty == Dirty::default() {
            pending.remove(&tray);
            return Ok(());
        }
        drop(state);
        self.shared.wake_backend();
        Ok(())
    }

    fn icon_state(&self, icon: &TrayIcon, caps: &TrayCapabilities) -> Result<IconState, TrayError> {
        match icon {
            TrayIcon::Static(icon) => Ok(IconState::new(
                vec![(self.resolve(icon, 0, caps)?, Duration::ZERO)],
                Repeat::Times(0),
            )),
            TrayIcon::Animated(animation) => {
                if animation.frames.is_empty() {
                    return Err(TrayError::Image(crate::ImageError {
                        frame: 0,
                        rep: 0,
                        reason: crate::ImageFailure::Empty,
                    }));
                }
                if animation.frames.len() > MAX_FRAMES {
                    return Err(TrayError::Limit(LimitKind::Frames));
                }
                let min = Duration::from_millis(u64::from(caps.min_frame_ms.max(1)));
                let frames = animation
                    .frames
                    .iter()
                    .zip(0u32..)
                    .map(|(frame, at)| {
                        let icon = self.resolve(&frame.icon, at, caps)?;
                        let duration = Duration::from_millis(u64::from(frame.duration_ms)).max(min);
                        Ok((icon, duration))
                    })
                    .collect::<Result<Vec<_>, TrayError>>()?;
                Ok(IconState::new(frames, animation.repeat))
            }
        }
    }

    fn item_icons(
        &self,
        nodes: &[MenuNode],
        caps: &TrayCapabilities,
    ) -> Result<HashMap<ItemId, Arc<DecodedIcon>>, TrayError> {
        if caps.menu.item_icons == Support::Unsupported {
            return Ok(HashMap::new());
        }
        nodes
            .iter()
            .filter_map(|node| node.icon.as_ref().map(|icon| (node.id, icon)))
            .map(|(id, icon)| Ok((id, self.resolve(icon, 0, caps)?)))
            .collect()
    }

    fn resolve(
        &self,
        icon: &IconRef,
        frame: u32,
        caps: &TrayCapabilities,
    ) -> Result<Arc<DecodedIcon>, TrayError> {
        match icon {
            IconRef::Inline(image) => Ok(Arc::new(DecodedIcon::decode(image, frame, names(caps))?)),
            IconRef::Registered(id) => self
                .shared
                .lock()
                .icons
                .get(id)
                .cloned()
                .ok_or(TrayError::UnknownIcon),
            // A future reference kind this host does not know.
            _ => Err(TrayError::UnknownIcon),
        }
    }
}

/// Removes a tray, ending its event streams. Returns whether it existed.
fn remove_tray(shared: &Shared, tray: NativeHandleId) -> bool {
    let mut state = shared.lock();
    let Some(entry) = state.trays.remove(&tray) else {
        return false;
    };
    state.dirty(tray).removed = true;
    drop(state);
    // Dropping the senders ends the streams; outside the lock.
    drop(entry);
    shared.wake_backend();
    true
}

fn names(caps: &TrayCapabilities) -> bool {
    caps.image_names != Support::Unsupported
}

fn check_text(text: Option<&str>) -> Result<(), TrayError> {
    if text.is_some_and(|t| t.len() > MAX_TEXT_BYTES) {
        return Err(TrayError::Limit(LimitKind::Text));
    }
    Ok(())
}

fn check_tooltip(tooltip: Option<&Tooltip>) -> Result<(), TrayError> {
    let Some(tooltip) = tooltip else {
        return Ok(());
    };
    check_text(Some(&tooltip.title))?;
    check_text(tooltip.body.as_deref())
}

/// Implements [`Tray`](crate::Tray) for a wrapper with a `host: Host`
/// field.
macro_rules! delegate_tray {
    ($ty:ty) => {
        impl $crate::Tray for $ty {
            async fn capabilities(&self) -> $crate::TrayCapabilities {
                self.host.capabilities()
            }

            async fn register_icon(
                &self,
                ctx: ::istmo_core::HostCtx,
                image: ::istmo_menu::IconImage,
            ) -> Result<$crate::RegisteredIcon, $crate::TrayError> {
                self.host.register_icon(&ctx, &image)
            }

            async fn create(
                &self,
                ctx: ::istmo_core::HostCtx,
                spec: $crate::TraySpec,
            ) -> Result<$crate::TrayItem, $crate::TrayError> {
                self.host.create(&ctx, spec)
            }

            async fn set_icon(
                &self,
                tray: ::istmo_core::NativeHandleId,
                icon: $crate::TrayIcon,
            ) -> Result<(), $crate::TrayError> {
                self.host.set_icon(tray, &icon)
            }

            async fn set_title(
                &self,
                tray: ::istmo_core::NativeHandleId,
                title: Option<String>,
            ) -> Result<(), $crate::TrayError> {
                self.host.set_title(tray, title)
            }

            async fn set_tooltip(
                &self,
                tray: ::istmo_core::NativeHandleId,
                tooltip: Option<$crate::Tooltip>,
            ) -> Result<(), $crate::TrayError> {
                self.host.set_tooltip(tray, tooltip)
            }

            async fn set_visible(
                &self,
                tray: ::istmo_core::NativeHandleId,
                visible: bool,
            ) -> Result<(), $crate::TrayError> {
                self.host.set_visible(tray, visible)
            }

            async fn set_menu(
                &self,
                tray: ::istmo_core::NativeHandleId,
                menu: Vec<::istmo_menu::MenuNode>,
            ) -> Result<(), $crate::TrayError> {
                self.host.set_menu(tray, menu)
            }

            async fn update_items(
                &self,
                tray: ::istmo_core::NativeHandleId,
                patches: Vec<::istmo_menu::ItemPatch>,
            ) -> Result<(), $crate::TrayError> {
                self.host.update_items(tray, &patches)
            }

            async fn destroy(
                &self,
                tray: ::istmo_core::NativeHandleId,
            ) -> Result<(), $crate::TrayError> {
                self.host.destroy(tray)
            }

            fn events(
                &self,
                tray: ::istmo_core::NativeHandleId,
            ) -> ::flume::Receiver<Result<$crate::TrayEvent, $crate::TrayError>> {
                self.host.events(tray)
            }
        }
    };
}
pub(crate) use delegate_tray;

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use istmo_core::CallOrigin;
    use istmo_menu::{CheckState, ImageData, MenuBuilder};

    use super::*;
    use crate::{Animation, AnimationFrame, EventSupport, MenuSupport, TrayBackend};

    struct FakeBackend {
        wakes: AtomicUsize,
    }

    impl Backend for FakeBackend {
        fn capabilities(&self) -> TrayCapabilities {
            TrayCapabilities {
                backend: TrayBackend::Sni,
                host: HostState::Available,
                max_items: Some(2),
                min_frame_ms: 10,
                image_names: Support::Native,
                template_images: Support::Unsupported,
                title: Support::Native,
                tooltip_body: Support::Native,
                menu: MenuSupport {
                    item_icons: Support::Native,
                    ..MenuSupport::NONE
                },
                events: EventSupport::NONE,
                anchor: None,
            }
        }

        fn wake(&self) {
            self.wakes.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[expect(clippy::unnecessary_wraps, reason = "has the `StartBackend` signature")]
    fn fake(_shared: &Arc<Shared>) -> Result<Arc<dyn Backend>, TrayError> {
        Ok(Arc::new(FakeBackend {
            wakes: AtomicUsize::new(0),
        }))
    }

    fn png(value: u8) -> IconImage {
        let raster = crate::image::Raster {
            width: 2,
            height: 2,
            rgba: [value, value, value, 255].repeat(4),
        };
        IconImage::new(ImageData::Png(raster.to_png()))
    }

    fn setup() -> (Host, HostCtx, Arc<Runtime>) {
        let runtime = Runtime::mock().finish().runtime;
        let ctx = HostCtx::new(&runtime, CallOrigin::Local);
        (Host::new(fake), ctx, runtime)
    }

    fn spec() -> TraySpec {
        TraySpec::new("test", png(1)).with_menu(
            MenuBuilder::default()
                .action(1, "a")
                .check(2, "c", CheckState::Off)
                .into_nodes(),
        )
    }

    #[test]
    fn changes_coalesce_per_tray() {
        let (host, ctx, _rt) = setup();
        let tray = host.create(&ctx, spec()).unwrap().tray;
        for title in ["a", "b", "c"] {
            host.set_title(tray, Some(title.to_owned())).unwrap();
        }
        let pending = host.shared.take_pending();
        assert_eq!(pending.len(), 1);
        let dirty = &pending[0].1;
        assert!(dirty.added && dirty.title);
        assert_eq!(host.shared.lock().trays[&tray].title.as_deref(), Some("c"));

        // Setting the same value again is no change.
        host.set_title(tray, Some("c".to_owned())).unwrap();
        assert_eq!(host.shared.take_pending(), Vec::new());
    }

    #[test]
    fn a_tray_added_and_removed_unseen_is_skipped() {
        let (host, ctx, _rt) = setup();
        let tray = host.create(&ctx, spec()).unwrap().tray;
        host.destroy(tray).unwrap();
        assert_eq!(host.shared.take_pending(), Vec::new());
        assert_eq!(host.destroy(tray), Err(TrayError::UnknownTray));
    }

    #[test]
    fn item_limit_is_enforced() {
        let (host, ctx, _rt) = setup();
        host.create(&ctx, spec()).unwrap();
        host.create(&ctx, spec()).unwrap();
        assert_eq!(
            host.create(&ctx, spec()),
            Err(TrayError::Limit(LimitKind::Items))
        );
    }

    #[test]
    fn events_before_the_first_subscriber_are_replayed() {
        let (host, ctx, _rt) = setup();
        let tray = host.create(&ctx, spec()).unwrap().tray;
        host.shared.activate_item(tray, ItemId::from(2));
        let events = host.events(tray);
        let got: Vec<_> = events.try_iter().map(Result::unwrap).collect();
        assert_eq!(
            got,
            [
                TrayEvent::ItemActivated(ItemId::from(2)),
                TrayEvent::CheckChanged(CheckChange {
                    item: ItemId::from(2),
                    state: CheckState::On
                }),
            ]
        );
        let pending = host.shared.take_pending();
        assert!(matches!(pending[0].1.menu, MenuDirty::Patches(_)));
    }

    #[test]
    fn destroying_a_tray_ends_its_streams() {
        let (host, ctx, _rt) = setup();
        let tray = host.create(&ctx, spec()).unwrap().tray;
        let events = host.events(tray);
        host.destroy(tray).unwrap();
        assert!(events.recv().is_err());
    }

    #[test]
    fn bad_patches_leave_the_menu_alone() {
        let (host, ctx, _rt) = setup();
        let tray = host.create(&ctx, spec()).unwrap().tray;
        host.shared.take_pending();
        let patches = [
            ItemPatch {
                id: ItemId::from(1),
                change: ItemChange::Label("b".to_owned()),
            },
            ItemPatch {
                id: ItemId::from(9),
                change: ItemChange::Enabled(false),
            },
        ];
        assert!(host.update_items(tray, &patches).is_err());
        assert_eq!(
            host.shared.lock().trays[&tray]
                .menu
                .get(ItemId::from(1))
                .unwrap()
                .label,
            "a"
        );
        assert_eq!(host.shared.take_pending(), Vec::new());
    }

    #[test]
    fn registered_icons_resolve_and_unknown_ones_fail() {
        let (host, ctx, _rt) = setup();
        let icon = host.register_icon(&ctx, &png(7)).unwrap().icon;
        let tray = host.create(&ctx, spec()).unwrap().tray;
        host.set_icon(tray, &TrayIcon::Static(IconRef::Registered(icon)))
            .unwrap();
        let missing = NativeHandleId::from(12_345);
        assert_eq!(
            host.set_icon(tray, &TrayIcon::Static(IconRef::Registered(missing))),
            Err(TrayError::UnknownIcon)
        );
    }

    #[test]
    fn animations_loop_and_stop_after_their_runs() {
        let (host, _ctx, _rt) = setup();
        let caps = host.backend().unwrap().capabilities();
        let frame = |value| AnimationFrame {
            icon: IconRef::Inline(png(value)),
            duration_ms: 1,
        };
        let animation = TrayIcon::Animated(Animation {
            frames: vec![frame(1), frame(2)],
            repeat: Repeat::Times(1),
        });
        let mut state = host.icon_state(&animation, &caps).unwrap();
        // Durations are raised to the backend's minimum.
        assert_eq!(state.frames[0].1, Duration::from_millis(10));
        let start = Instant::now();
        assert!(!state.advance(start));
        assert!(state.advance(start + Duration::from_millis(10)));
        assert_eq!(state.current, 1);
        // One run done: the last frame stays.
        assert!(!state.advance(start + Duration::from_millis(30)));
        assert_eq!(state.current, 1);
        assert!(state.next_at.is_none());
    }

    #[test]
    fn host_changes_reach_every_tray() {
        let (host, ctx, _rt) = setup();
        let tray = host.create(&ctx, spec()).unwrap().tray;
        let events = host.events(tray);
        host.shared.set_host(HostState::Available);
        host.shared.set_host(HostState::Missing);
        host.shared.set_host(HostState::Missing);
        let got: Vec<_> = events.try_iter().map(Result::unwrap).collect();
        assert_eq!(got, [TrayEvent::Host(HostState::Missing)]);
        assert_eq!(host.capabilities().host, HostState::Missing);
    }
}
