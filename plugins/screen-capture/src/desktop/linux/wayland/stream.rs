//! The capture loop both direct protocols share: format negotiation
//! against what the compositor offers, the DMA-BUF / `wl_shm` slot
//! pools, exhaustion, in-band downgrades and frame delivery. The
//! protocols differ only in how a frame is requested ([`FrameSource`]).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use istmo_core::NativeBuffer;
use wayland_client::protocol::wl_buffer::WlBuffer;

use crate::select::{self, Chosen, Offer};
use crate::types::{
    BorderState, BufferMemory, CaptureError, CaptureEvent, Colour, ConsentModel, CopyStage,
    CursorMode, DamageSupport, DowngradeReason, Exhaustion, FormatRequest, FrameMeta, FrameRate,
    GpuIdentity, IndicatorState, NegotiatedStream, Orientation, OrientationSupport, PixelFormat,
    PlaneLayout, PtsClock, Rect, RegionCrop, Requirement, Size, Source, SyncKind,
};

use super::super::super::backend::BackendSession;
use super::super::super::clock;
use super::super::super::output::{Delivery, FrameOut};
use super::super::gbm::{Allocator, Bo, Pick, Slots};
use super::{Client, FrameEvents, FrameStatus, ShmBuffer, shm_format};

/// How long a capture waits between stop checks.
const STOP_POLL: Duration = Duration::from_millis(100);
/// Consecutive refused DMA-BUF copies before the session leaves the tier.
const REJECTED_LIMIT: u32 = 3;

/// Environment switch: request every frame with a full copy instead of
/// a damage-driven one (compositors with broken damage, measurements,
/// tests on an idle screen).
#[cfg(feature = "linux-wlr-screencopy")]
pub(crate) const FULL_COPY_ENV: &str = "ISTMO_SCREEN_CAPTURE_FULL_COPY";

/// What the next frame can be copied into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Offers {
    pub(crate) size: Size,
    /// `wl_shm` format code and the stride it needs.
    pub(crate) shm: Vec<(u32, u32)>,
    /// DRM fourcc and the modifiers the compositor imports, its order.
    pub(crate) dmabuf: Vec<(u32, Vec<u64>)>,
    /// The device to allocate DMA-BUFs on.
    pub(crate) device: Option<u64>,
}

impl Offers {
    /// The offers as negotiation sees them. DMA-BUF only when an
    /// allocator exists and `cpu_only` is off.
    fn as_select(&self, dmabuf: bool) -> Vec<Offer> {
        let gpu = self
            .dmabuf
            .iter()
            .filter(|_| dmabuf)
            .filter_map(|(fourcc, modifiers)| {
                Some(Offer {
                    format: PixelFormat::from_drm_fourcc(*fourcc)?,
                    memory: BufferMemory::DmaBuf,
                    modifiers: modifiers.clone(),
                    copies: Vec::new(),
                })
            });
        let cpu = self.shm.iter().filter_map(|(code, _)| {
            Some(Offer {
                format: shm_format(*code)?,
                memory: BufferMemory::Cpu,
                modifiers: Vec::new(),
                copies: vec![CopyStage::ShmTransport],
            })
        });
        gpu.chain(cpu).collect()
    }
}

/// How one protocol asks for frames.
pub(crate) trait FrameSource {
    /// What the next frame can be copied into (wlr creates the frame
    /// object here; ext reads the session constraints).
    fn offers(&mut self, client: &mut Client) -> Result<Offers, CaptureError>;

    /// Give up the frame `offers` prepared without copying.
    fn abandon(&mut self);

    /// Copy the next frame into `buffer`. `None` when `stop()` became
    /// true while waiting.
    fn copy(
        &mut self,
        client: &mut Client,
        buffer: &WlBuffer,
        size: Size,
        with_damage: bool,
        stop: &dyn Fn() -> bool,
    ) -> Result<Option<FrameEvents>, CaptureError>;

    /// The orientation a copied frame needs.
    fn orientation(&self, client: &Client, events: &FrameEvents) -> Orientation;

    /// The captured source went away.
    fn lost(&self, client: &Client) -> bool;
}

struct DmaSlot {
    bo: Bo,
    buffer: WlBuffer,
}

impl Drop for DmaSlot {
    fn drop(&mut self) {
        self.buffer.destroy();
    }
}

type DmaKey = (Size, u32, Vec<u64>);
type ShmKey = (Size, u32, u32);

/// Where the frames of the session go.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Transport {
    Dmabuf { fourcc: u32, modifiers: Vec<u64> },
    Shm { code: u32 },
}

/// One captured frame, or why there is none.
pub(crate) enum Step {
    Frame(Box<FrameMeta>, NativeBuffer),
    /// The consumer holds every buffer it may.
    Exhausted(crate::pool::Exhausted),
    /// The compositor refused this frame; try again.
    Skipped,
    /// Asked to stop while waiting.
    Stopped,
}

/// What a session or screenshot was opened with.
#[derive(Debug, Clone)]
pub(crate) struct Settings {
    pub(crate) formats: FormatRequest,
    pub(crate) exhaustion: Exhaustion,
    pub(crate) pool_depth: u8,
    pub(crate) cursor: CursorMode,
    pub(crate) damage: DamageSupport,
}

/// The shared engine: a source, its buffers and the negotiated format.
pub(crate) struct Engine<S> {
    pub(crate) client: Client,
    pub(crate) source: S,
    pub(crate) info: Source,
    allocator: Option<Allocator>,
    settings: Settings,
    transport: Transport,
    chosen: Chosen,
    dma: Slots<DmaKey, DmaSlot>,
    shm: Slots<ShmKey, ShmBuffer>,
    rejected: u32,
    /// A `CopyToCpu` exhaustion to report (frames still flowed).
    pub(crate) readback_episode: Option<crate::pool::Exhausted>,
    /// DMA-BUF tiers left after the compositor or GBM refused them.
    dmabuf_lost: bool,
    size: Size,
    sync: SyncKind,
}

fn chosen_transport(chosen: &Chosen, offers: &Offers) -> Option<Transport> {
    match chosen.memory {
        BufferMemory::DmaBuf => offers
            .dmabuf
            .iter()
            .find(|(fourcc, _)| PixelFormat::from_drm_fourcc(*fourcc) == Some(chosen.format))
            .map(|(fourcc, _)| Transport::Dmabuf {
                fourcc: *fourcc,
                modifiers: chosen.modifiers.clone(),
            }),
        BufferMemory::Cpu => offers
            .shm
            .iter()
            .find(|(code, _)| shm_format(*code) == Some(chosen.format))
            .map(|(code, _)| Transport::Shm { code: *code }),
        BufferMemory::D3d11Texture
        | BufferMemory::IoSurface
        | BufferMemory::CvPixelBuffer
        | BufferMemory::HardwareBuffer
        | BufferMemory::WebVideoFrame => None,
    }
}

impl<S: FrameSource> Engine<S> {
    /// Negotiate against the first offers.
    ///
    /// # Errors
    /// `Unsatisfiable(Formats)` when no listed tier fits what the
    /// compositor offers; `Unsatisfiable(ExhaustionReadback)` for
    /// `CopyToCpu` on DMA-BUF (no GPU readback yet).
    pub(crate) fn negotiate(
        mut client: Client,
        mut source: S,
        info: Source,
        settings: Settings,
        sync: SyncKind,
    ) -> Result<Self, CaptureError> {
        let backend = client.backend;
        let offers = source.offers(&mut client)?;
        source.abandon();
        let allocator = if client.has_dmabuf() && !offers.dmabuf.is_empty() {
            Allocator::open(offers.device)
                .map_err(|err| tracing::info!(%backend, %err, "screen-capture: DMA-BUF tiers unavailable"))
                .ok()
        } else {
            None
        };
        // An empty request with CopyToCpu means CPU memory: that is the
        // memory the readback works for.
        let cpu_only =
            settings.formats.tiers.is_empty() && settings.exhaustion == Exhaustion::CopyToCpu;
        let candidates = offers.as_select(allocator.is_some() && !cpu_only);
        let chosen = select::negotiate(&settings.formats, &candidates, 0)
            .ok_or_else(|| CaptureError::unsatisfiable(Some(backend), Requirement::Formats))?;
        if chosen.memory == BufferMemory::DmaBuf && settings.exhaustion == Exhaustion::CopyToCpu {
            return Err(CaptureError::unsatisfiable(
                Some(backend),
                Requirement::ExhaustionReadback,
            ));
        }
        let transport = chosen_transport(&chosen, &offers)
            .ok_or_else(|| CaptureError::unsatisfiable(Some(backend), Requirement::Formats))?;
        let depth = usize::from(settings.pool_depth.max(2));
        Ok(Self {
            client,
            source,
            info,
            allocator,
            dma: Slots::new(depth),
            shm: Slots::new(depth),
            settings,
            transport,
            chosen,
            rejected: 0,
            readback_episode: None,
            dmabuf_lost: false,
            size: offers.size,
            sync,
        })
    }

    const fn backend(&self) -> crate::types::BackendId {
        self.client.backend
    }

    fn slots_have_room(&self) -> bool {
        match self.transport {
            Transport::Dmabuf { .. } => self.dma.accounting().has_room(),
            Transport::Shm { .. } => self.shm.accounting().has_room(),
        }
    }

    /// The tier, format and copies as negotiated now.
    pub(crate) fn negotiated(&self) -> NegotiatedStream {
        let dmabuf = matches!(self.transport, Transport::Dmabuf { .. });
        let mut copies = self.chosen.copies.clone();
        if !dmabuf && !copies.contains(&CopyStage::ShmTransport) {
            copies.push(CopyStage::ShmTransport);
        }
        let consent = ConsentModel::NoneNeeded;
        NegotiatedStream {
            backend: self.backend(),
            source: self.info.clone(),
            size: self.size,
            content_rect: Rect::from_size(self.size),
            format: self.chosen.format,
            memory: self.chosen.memory,
            modifier: None,
            tier: self.chosen.tier,
            copies,
            fps: FrameRate::Native,
            source_rate_mhz: self.info.refresh_mhz,
            producer: if dmabuf {
                self.allocator.as_ref().map(|a| GpuIdentity::Drm(a.dev_t))
            } else {
                None
            },
            sync: if dmabuf { self.sync } else { SyncKind::Ready },
            pool_depth: self.settings.pool_depth,
            exhaustion: self.settings.exhaustion,
            orientation: OrientationSupport::InMeta,
            crop: RegionCrop::InMeta,
            damage: self.settings.damage,
            cursor: self.settings.cursor,
            colour: Colour::SRGB,
            hdr: false,
            composite: None,
            consent,
            indicator: IndicatorState::NotApplicable,
            border: BorderState::NotApplicable,
            restore_token: None,
            restored: false,
            audio: None,
        }
    }

    fn meta(
        &self,
        events: &FrameEvents,
        size: Size,
        planes: Vec<PlaneLayout>,
        with_damage: bool,
    ) -> FrameMeta {
        let dmabuf = matches!(self.transport, Transport::Dmabuf { .. });
        FrameMeta {
            seq: 0,
            pts_ns: events.pts_ns.unwrap_or_else(clock::monotonic_ns),
            pts_clock: PtsClock::Monotonic,
            wall_us: 0,
            size,
            content_rect: Rect::from_size(size),
            format: self.chosen.format,
            memory: self.chosen.memory,
            planes,
            modifier: None,
            buffer: crate::types::BufferIdentity::default(),
            tier: self.chosen.tier,
            copied: None,
            producer: if dmabuf {
                self.allocator.as_ref().map(|a| GpuIdentity::Drm(a.dev_t))
            } else {
                None
            },
            colour: Colour::SRGB,
            hdr: None,
            orientation: self.source.orientation(&self.client, events),
            damage: events.damage(!with_damage),
            cursor: None,
            sync: if dmabuf { self.sync } else { SyncKind::Ready },
            scale_permille: self.info.scale(),
            source_id: self.info.id.clone(),
            repeat: false,
            skipped_before: 0,
            protected_content: false,
            composite: None,
        }
    }

    /// Leave the current tier for the next listed one.
    fn downgrade(
        &mut self,
        offers: &Offers,
        reason: DowngradeReason,
    ) -> Result<CaptureEvent, CaptureError> {
        self.dmabuf_lost = true;
        let candidates = offers.as_select(false);
        let (chosen, downgrade) = select::downgrade(
            &self.settings.formats,
            &candidates,
            self.chosen.tier,
            reason,
        )?;
        let transport = chosen_transport(&chosen, offers).ok_or(CaptureError::FormatLost)?;
        tracing::warn!(backend = %self.backend(), ?downgrade, "screen-capture: format tier lowered");
        self.chosen = chosen;
        self.transport = transport;
        Ok(CaptureEvent::Downgraded(downgrade))
    }

    /// Capture one frame.
    ///
    /// `downgraded` receives the event of a tier change, if one happened.
    ///
    /// # Errors
    /// Protocol errors, a lost source, or `FormatLost`.
    pub(crate) fn frame(
        &mut self,
        with_damage: bool,
        stop: &dyn Fn() -> bool,
        downgraded: &mut Option<CaptureEvent>,
    ) -> Result<Step, CaptureError> {
        if self.source.lost(&self.client) {
            return Err(CaptureError::SourceLost);
        }
        let readback = matches!(self.transport, Transport::Shm { .. })
            && self.settings.exhaustion == Exhaustion::CopyToCpu;
        if !readback && !self.slots_have_room() {
            let accounting = match self.transport {
                Transport::Dmabuf { .. } => self.dma.accounting(),
                Transport::Shm { .. } => self.shm.accounting(),
            };
            if let Err(exhausted) = accounting.hold(u32::MAX) {
                return Ok(Step::Exhausted(exhausted));
            }
        }
        let offers = self.source.offers(&mut self.client)?;
        self.size = offers.size;
        match self.transport.clone() {
            Transport::Dmabuf { fourcc, modifiers } => {
                if !offers.dmabuf.iter().any(|(f, _)| *f == fourcc) || self.dmabuf_lost {
                    // The compositor stopped offering this format.
                    *downgraded = Some(self.downgrade(&offers, DowngradeReason::ProducerChanged)?);
                    self.source.abandon();
                    return Ok(Step::Skipped);
                }
                self.dmabuf_frame(&offers, fourcc, &modifiers, with_damage, stop, downgraded)
            }
            Transport::Shm { code } => self.shm_frame(&offers, code, with_damage, stop),
        }
    }

    fn dmabuf_frame(
        &mut self,
        offers: &Offers,
        fourcc: u32,
        modifiers: &[u64],
        with_damage: bool,
        stop: &dyn Fn() -> bool,
        downgraded: &mut Option<CaptureEvent>,
    ) -> Result<Step, CaptureError> {
        let size = offers.size;
        self.dma.reconfigure((size, fourcc, modifiers.to_vec()));
        let slot = match self.dma.pick() {
            Pick::Free(slot) => slot,
            Pick::Exhausted(exhausted) => {
                self.source.abandon();
                return Ok(Step::Exhausted(exhausted));
            }
            Pick::Grow => {
                let Some(allocator) = self.allocator.as_ref() else {
                    *downgraded = Some(self.downgrade(offers, DowngradeReason::AllocationFailed)?);
                    self.source.abandon();
                    return Ok(Step::Skipped);
                };
                let bo = match allocator.alloc(size.width, size.height, fourcc, modifiers) {
                    Ok(bo) => bo,
                    Err(err) => {
                        tracing::warn!(backend = %self.backend(), %err, "screen-capture: DMA-BUF allocation failed");
                        *downgraded =
                            Some(self.downgrade(offers, DowngradeReason::AllocationFailed)?);
                        self.source.abandon();
                        return Ok(Step::Skipped);
                    }
                };
                let Some(buffer) = self.client.dmabuf_buffer(&bo) else {
                    *downgraded = Some(self.downgrade(offers, DowngradeReason::AllocationFailed)?);
                    self.source.abandon();
                    return Ok(Step::Skipped);
                };
                self.dma.insert(DmaSlot { bo, buffer })
            }
        };
        let lease = match self.dma.accounting().hold(slot) {
            Ok(lease) => lease,
            Err(exhausted) => {
                self.source.abandon();
                return Ok(Step::Exhausted(exhausted));
            }
        };
        let Some(entry) = self.dma.get(slot) else {
            self.source.abandon();
            return Ok(Step::Skipped);
        };
        let buffer = entry.buffer.clone();
        let Some(events) = self
            .source
            .copy(&mut self.client, &buffer, size, with_damage, stop)?
        else {
            return Ok(Step::Stopped);
        };
        if events.status != FrameStatus::Ready {
            drop(lease);
            drop(self.dma.discard(slot));
            self.rejected += 1;
            if self.rejected >= REJECTED_LIMIT {
                *downgraded = Some(self.downgrade(offers, DowngradeReason::ImportRejected)?);
            }
            return Ok(Step::Skipped);
        }
        self.rejected = 0;
        let Some(entry) = self.dma.get(slot) else {
            return Ok(Step::Skipped);
        };
        let dmabuf = Arc::clone(&entry.bo.dmabuf);
        let planes = dmabuf
            .planes
            .iter()
            .map(|p| PlaneLayout {
                offset: p.offset,
                stride: p.stride,
                size: u64::from(p.stride) * u64::from(size.height),
            })
            .collect();
        let mut meta = self.meta(&events, size, planes, with_damage);
        meta.modifier = Some(dmabuf.modifier);
        meta.buffer = lease.identity();
        let identity = lease.identity().into();
        let buffer = NativeBuffer::from_dmabuf(dmabuf)
            .with_identity(identity)
            .with_release(move || drop(lease));
        Ok(Step::Frame(Box::new(meta), buffer))
    }

    fn shm_frame(
        &mut self,
        offers: &Offers,
        code: u32,
        with_damage: bool,
        stop: &dyn Fn() -> bool,
    ) -> Result<Step, CaptureError> {
        let backend = self.backend();
        let size = offers.size;
        let Some(&(_, stride)) = offers.shm.iter().find(|(c, _)| *c == code) else {
            self.source.abandon();
            return Err(CaptureError::FormatLost);
        };
        self.shm.reconfigure((size, code, stride));
        let (slot, readback) = match self.shm.pick() {
            Pick::Free(slot) => (Some(slot), false),
            Pick::Grow => {
                let buffer = self.client.shm_buffer(size, stride, code)?;
                (Some(self.shm.insert(buffer)), false)
            }
            Pick::Exhausted(exhausted) if self.settings.exhaustion != Exhaustion::CopyToCpu => {
                self.source.abandon();
                return Ok(Step::Exhausted(exhausted));
            }
            // CopyToCpu: a lease-free copy keeps frames flowing.
            Pick::Exhausted(exhausted) => {
                self.readback_episode = Some(exhausted);
                (None, true)
            }
        };
        let lease = match slot.map(|s| self.shm.accounting().hold(s)) {
            Some(Ok(lease)) => Some(lease),
            Some(Err(exhausted)) => {
                self.source.abandon();
                return Ok(Step::Exhausted(exhausted));
            }
            None => None,
        };
        let scratch = match slot {
            Some(_) => None,
            None => Some(self.client.shm_buffer(size, stride, code)?),
        };
        let Some(target) = slot.and_then(|s| self.shm.get(s)).or(scratch.as_ref()) else {
            self.source.abandon();
            return Ok(Step::Skipped);
        };
        let wl_buffer = target.buffer.clone();
        let Some(events) =
            self.source
                .copy(&mut self.client, &wl_buffer, size, with_damage, stop)?
        else {
            return Ok(Step::Stopped);
        };
        if events.status != FrameStatus::Ready {
            return Ok(Step::Skipped);
        }
        let Some(target) = slot.and_then(|s| self.shm.get(s)).or(scratch.as_ref()) else {
            return Ok(Step::Skipped);
        };
        let bytes = target.read(backend)?;
        let planes = vec![PlaneLayout {
            offset: 0,
            stride,
            size: u64::from(stride) * u64::from(size.height),
        }];
        let mut meta = self.meta(&events, size, planes, with_damage);
        let buffer = NativeBuffer::from_bytes(bytes);
        let buffer = if let Some(lease) = lease {
            meta.buffer = lease.identity();
            buffer.with_release(move || drop(lease))
        } else {
            meta.copied = Some(CopyStage::ExhaustionReadback);
            buffer
        };
        if readback {
            meta.copied = Some(CopyStage::ExhaustionReadback);
        }
        Ok(Step::Frame(Box::new(meta), buffer))
    }

    /// One frame, full copy, for a screenshot.
    ///
    /// # Errors
    /// The compositor refused it repeatedly, or a protocol error.
    pub(crate) fn screenshot(&mut self) -> Result<(FrameMeta, NativeBuffer), CaptureError> {
        let backend = self.backend();
        let never = || false;
        for _attempt in 0..REJECTED_LIMIT + 2 {
            let mut downgraded = None;
            match self.frame(false, &never, &mut downgraded)? {
                Step::Frame(meta, buffer) => return Ok((*meta, buffer)),
                Step::Skipped | Step::Exhausted(_) | Step::Stopped => {}
            }
        }
        Err(CaptureError::protocol(
            backend,
            "screenshot",
            "the compositor refused every copy",
        ))
    }
}

/// A running direct-protocol session: its capture thread.
pub(crate) struct ThreadSession {
    stop: Arc<AtomicBool>,
    done: flume::Receiver<()>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl BackendSession for ThreadSession {}

impl Drop for ThreadSession {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Bounded: a capture waits at most `STOP_POLL` between checks.
        if self.done.recv_timeout(Duration::from_secs(2)).is_ok()
            && let Some(thread) = self.thread.take()
        {
            let _joined = thread.join();
        }
    }
}

fn period(fps: FrameRate) -> Option<Duration> {
    match fps {
        FrameRate::Native => None,
        FrameRate::Max(n) | FrameRate::Exact(n) | FrameRate::OnDamage(n) => {
            Some(Duration::from_secs(1) / n.max(1))
        }
    }
}

/// Start a capture thread: `setup` runs on it (Wayland objects stay on
/// the thread that dispatches them) and returns the negotiated engine;
/// the loop then delivers frames into `out` until stopped.
///
/// # Errors
/// What `setup` failed with, or a thread that could not start.
pub(crate) fn spawn<S, F>(
    name: &str,
    out: FrameOut,
    with_damage: bool,
    setup: F,
) -> Result<(Box<dyn BackendSession>, NegotiatedStream), CaptureError>
where
    S: FrameSource + 'static,
    F: FnOnce() -> Result<Engine<S>, CaptureError> + Send + 'static,
{
    let backend = out.backend();
    let stop = Arc::new(AtomicBool::new(false));
    let (ready_tx, ready_rx) = flume::bounded(1);
    let (done_tx, done_rx) = flume::bounded(1);
    let thread_stop = Arc::clone(&stop);
    let thread = std::thread::Builder::new()
        .name(format!("istmo-screen-capture-{name}"))
        .spawn(move || {
            let mut engine = match setup() {
                Ok(engine) => engine,
                Err(err) => {
                    let _sent = ready_tx.send(Err(err));
                    let _done = done_tx.send(());
                    return;
                }
            };
            if ready_tx.send(Ok(engine.negotiated())).is_err() {
                let _done = done_tx.send(());
                return;
            }
            run(&mut engine, &out, &thread_stop, with_damage);
            let _done = done_tx.send(());
        })
        .map_err(|e| CaptureError::protocol(backend, "spawn capture thread", e))?;
    let negotiated = ready_rx
        .recv_timeout(Duration::from_secs(10))
        .map_err(|_| {
            CaptureError::protocol(backend, "open", "no answer from the compositor within 10 s")
        })??;
    Ok((
        Box::new(ThreadSession {
            stop,
            done: done_rx,
            thread: Some(thread),
        }),
        negotiated,
    ))
}

fn run<S: FrameSource>(
    engine: &mut Engine<S>,
    out: &FrameOut,
    stop: &AtomicBool,
    with_damage: bool,
) {
    let stopped = || stop.load(Ordering::SeqCst) || out.is_stopped();
    let mut next = Instant::now();
    let mut skipped = 0u32;
    while !stopped() {
        if !out.wants_frames() {
            std::thread::sleep(STOP_POLL / 2);
            continue;
        }
        // Throttle at the source: no copy is requested faster than the
        // session rate.
        if let Some(period) = period(out.fps()) {
            let now = Instant::now();
            if now < next {
                std::thread::sleep(next - now);
            }
            next = Instant::now() + period;
        }
        let mut tier_change = None;
        let outcome = engine.frame(with_damage, &stopped, &mut tier_change);
        if let Some(event) = tier_change {
            out.event(event);
            out.event(CaptureEvent::Renegotiated(engine.negotiated()));
        }
        if let Some(exhausted) = engine.readback_episode.take() {
            out.exhausted(
                exhausted,
                engine.settings.pool_depth,
                engine.settings.exhaustion,
            );
        }
        match outcome {
            Ok(Step::Frame(meta, buffer)) => {
                let mut meta = *meta;
                meta.skipped_before = std::mem::take(&mut skipped);
                if out.deliver(meta, buffer) == Delivery::Closed {
                    break;
                }
            }
            Ok(Step::Exhausted(exhausted)) => {
                out.exhausted(
                    exhausted,
                    engine.settings.pool_depth,
                    engine.settings.exhaustion,
                );
                skipped = skipped.saturating_add(1);
                match engine.settings.exhaustion {
                    // Stall: wait for a buffer to come back.
                    Exhaustion::Stall | Exhaustion::CopyToCpu => {
                        let released = match engine.transport {
                            Transport::Dmabuf { .. } => engine.dma.accounting().released(),
                            Transport::Shm { .. } => engine.shm.accounting().released(),
                        };
                        let _woken = released.recv_timeout(STOP_POLL);
                    }
                    // A direct protocol can pause; dropping means not
                    // requesting this period's frame.
                    Exhaustion::DropNewest => {
                        std::thread::sleep(period(out.fps()).unwrap_or(STOP_POLL / 10));
                    }
                }
            }
            Ok(Step::Skipped) => skipped = skipped.saturating_add(1),
            Ok(Step::Stopped) => break,
            Err(err) => {
                tracing::warn!(backend = %out.backend(), %err, "screen-capture: session ended");
                out.fail(err);
                return;
            }
        }
    }
}

/// Wait for the in-flight frame of `client` to finish, checking `stop`
/// every [`STOP_POLL`]. `None` when stopped.
///
/// # Errors
/// A protocol or socket error.
pub(crate) fn wait_frame(
    client: &mut Client,
    stop: &dyn Fn() -> bool,
) -> Result<Option<FrameEvents>, CaptureError> {
    loop {
        let answered = client.dispatch_until("frame", STOP_POLL, |s| {
            s.frame.status != FrameStatus::Pending
        })?;
        if answered {
            return Ok(Some(std::mem::take(&mut client.state.frame)));
        }
        if stop() {
            return Ok(None);
        }
    }
}

/// Whether frames are requested damage-driven (the default) or as full
/// copies ([`FULL_COPY_ENV`]).
#[cfg(feature = "linux-wlr-screencopy")]
pub(crate) fn damage_driven() -> bool {
    std::env::var_os(FULL_COPY_ENV).is_none_or(|v| v != "1")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offers() -> Offers {
        Offers {
            size: Size::new(64, 32),
            shm: vec![(1, 256)],
            dmabuf: vec![(
                PixelFormat::Bgrx8.drm_fourcc(),
                vec![0x0300_0000_0060_6014, 0],
            )],
            device: Some(0xe280),
        }
    }

    #[test]
    fn dmabuf_offers_need_an_allocator() {
        assert_eq!(offers().as_select(false).len(), 1);
        let all = offers().as_select(true);
        assert_eq!(all[0].memory, BufferMemory::DmaBuf);
        assert_eq!(all[1].copies, [CopyStage::ShmTransport]);
    }

    #[test]
    fn layout_prefers_what_every_side_takes() {
        let request = FormatRequest {
            tiers: vec![crate::types::FormatTier::new(vec![
                crate::types::FormatChoice::dmabuf(
                    PixelFormat::Bgrx8,
                    vec![0x1234, 0x0300_0000_0060_6014],
                ),
            ])],
        };
        let chosen = select::negotiate(&request, &offers().as_select(true), 0).expect("tier");
        assert_eq!(chosen.modifiers, [0x0300_0000_0060_6014]);
        assert_eq!(
            chosen_transport(&chosen, &offers()),
            Some(Transport::Dmabuf {
                fourcc: PixelFormat::Bgrx8.drm_fourcc(),
                modifiers: vec![0x0300_0000_0060_6014]
            })
        );
    }

    #[test]
    fn layout_without_importer_is_cpu_only() {
        // Without an allocator only the shm offer is negotiable.
        let chosen = select::negotiate(&FormatRequest::default(), &offers().as_select(false), 0)
            .expect("shm");
        assert_eq!(chosen.memory, BufferMemory::Cpu);
        assert_eq!(
            chosen_transport(&chosen, &offers()),
            Some(Transport::Shm { code: 1 })
        );
    }

    #[test]
    fn layout_without_feedback_never_picks_the_implicit_modifier() {
        let mut offers = offers();
        offers.dmabuf[0].1 = vec![select::MOD_INVALID];
        let any = FormatRequest {
            tiers: vec![crate::types::FormatTier::new(vec![
                crate::types::FormatChoice::dmabuf(PixelFormat::Bgrx8, Vec::new()),
            ])],
        };
        assert!(select::negotiate(&any, &offers.as_select(true), 0).is_none());
    }
}
