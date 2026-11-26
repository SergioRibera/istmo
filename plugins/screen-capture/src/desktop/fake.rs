//! A scripted backend for host tests: monitors with a known pixel
//! pattern, CPU frames from a pooled producer thread.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use istmo_core::NativeBuffer;

use crate::pool::PoolAccounting;
use crate::types::{
    Availability, BackendCaps, BackendId, BorderState, BufferMemory, CaptureError, Colour,
    ConsentModel, CopyStage, CursorMode, DamageSupport, Exhaustion, FormatCaps, FrameMeta,
    FrameRate, IndicatorState, NegotiatedStream, Orientation, OrientationSupport, PixelFormat,
    PlaneLayout, Provenance, PtsClock, Rect, RegionCrop, SessionUpdate, Size, Source, SourceId,
    SourceKind, SourceList, SourceListing, SyncKind,
};

use super::backend::{Backend, BackendSession, Captured, ScreenshotPlan, Shot, StreamPlan, Target};
use super::clock;
use super::output::{Delivery, FrameOut};

/// The red channel of pixel `(x, y)` of monitor `index`.
pub(crate) fn red(index: usize, x: u32, y: u32) -> u8 {
    let value = (index as u64 * 100 + u64::from(x) + u64::from(y) * 7) % 251;
    u8::try_from(value).unwrap_or(u8::MAX)
}

#[derive(Debug, Default)]
pub(crate) struct Counters {
    pub(crate) live_threads: AtomicUsize,
    pub(crate) delivered: AtomicUsize,
    pub(crate) readbacks: AtomicUsize,
}

#[derive(Debug)]
pub(crate) struct Fake {
    pub(crate) id: BackendId,
    pub(crate) monitors: Vec<Source>,
    pub(crate) availability: Availability,
    pub(crate) listing: SourceListing,
    pub(crate) counters: Arc<Counters>,
    /// Frames a stream produces before it stops by itself (0 = forever).
    pub(crate) frames: usize,
    /// After `frames`, fail with `SourceLost` (else go idle).
    pub(crate) lose_source: bool,
    pub(crate) joins_auto: bool,
}

pub(crate) fn monitor(id: BackendId, index: usize, geometry: Rect, scale: u32) -> Source {
    let mut source = Source::new(
        SourceId::new(id, format!("out-{index}")),
        SourceKind::Monitor,
        format!("OUT-{index}"),
    );
    source.geometry = Some(geometry);
    source.scale_permille = Some(scale);
    source.physical_size = Some(Size::new(
        geometry.width * scale / 1000,
        geometry.height * scale / 1000,
    ));
    source.flags.primary = index == 0;
    source
}

impl Fake {
    pub(crate) fn new(id: BackendId, monitors: Vec<Source>) -> Self {
        Self {
            id,
            monitors,
            availability: Availability::Ready,
            listing: SourceListing::Enumerable,
            counters: Arc::new(Counters::default()),
            frames: 0,
            lose_source: true,
            joins_auto: true,
        }
    }

    fn index_of(&self, source: &Source) -> usize {
        self.monitors
            .iter()
            .position(|m| m.id == source.id)
            .unwrap_or(0)
    }

    fn negotiated(&self, source: &Source, size: Size) -> NegotiatedStream {
        NegotiatedStream {
            backend: self.id,
            source: source.clone(),
            size,
            content_rect: Rect::from_size(size),
            format: PixelFormat::Bgrx8,
            memory: BufferMemory::Cpu,
            modifier: None,
            tier: 0,
            copies: Vec::new(),
            fps: FrameRate::Native,
            source_rate_mhz: Some(60_000),
            producer: None,
            sync: SyncKind::Ready,
            pool_depth: 0,
            exhaustion: Exhaustion::Stall,
            orientation: OrientationSupport::InMeta,
            crop: RegionCrop::InMeta,
            damage: DamageSupport::Frame,
            cursor: CursorMode::Hidden,
            colour: Colour::SRGB,
            hdr: false,
            composite: None,
            consent: ConsentModel::NoneNeeded,
            indicator: IndicatorState::NotApplicable,
            border: BorderState::NotApplicable,
            restore_token: None,
            restored: false,
            audio: None,
        }
    }
}

/// The buffer of monitor `index` (`Bgrx8`, tight rows), pre-orientation.
fn pixels(index: usize, size: Size) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(size.width as usize * size.height as usize * 4);
    for y in 0..size.height {
        for x in 0..size.width {
            bytes.extend_from_slice(&[0, 0, red(index, x, y), 0]);
        }
    }
    bytes
}

fn buffer_size(source: &Source) -> Size {
    let physical = source.physical_size.unwrap_or_default();
    // The buffer is as scanned out: a rotated monitor's buffer is
    // transposed relative to its upright size.
    if source.orientation.rotation.swaps_axes() {
        physical.transposed()
    } else {
        physical
    }
}

fn meta(source: &Source, size: Size) -> FrameMeta {
    FrameMeta {
        seq: 0,
        pts_ns: clock::monotonic_ns(),
        pts_clock: PtsClock::Monotonic,
        wall_us: 0,
        size,
        content_rect: Rect::from_size(size),
        format: PixelFormat::Bgrx8,
        memory: BufferMemory::Cpu,
        planes: vec![PlaneLayout {
            offset: 0,
            stride: size.width * 4,
            size: u64::from(size.width) * 4 * u64::from(size.height),
        }],
        modifier: None,
        buffer: crate::types::BufferIdentity::default(),
        tier: 0,
        copied: None,
        producer: None,
        colour: Colour::SRGB,
        hdr: None,
        orientation: source.orientation,
        damage: crate::types::Damage::Full,
        cursor: None,
        sync: SyncKind::Ready,
        scale_permille: source.scale(),
        source_id: source.id.clone(),
        repeat: false,
        skipped_before: 0,
        protected_content: false,
        composite: None,
    }
}

impl Backend for Fake {
    fn id(&self) -> BackendId {
        self.id
    }

    fn availability(&self) -> Availability {
        self.availability.clone()
    }

    fn joins_auto(&self) -> bool {
        self.joins_auto
    }

    fn caps(&self) -> Result<BackendCaps, CaptureError> {
        let mut caps = BackendCaps::new(self.id);
        caps.listing = self.listing;
        caps.video = vec![FormatCaps {
            format: PixelFormat::Bgrx8,
            memory: BufferMemory::Cpu,
            modifiers: Vec::new(),
            copies: Vec::new(),
        }];
        caps.damage = DamageSupport::Frame;
        caps.pool.exhaustion = vec![
            Exhaustion::Stall,
            Exhaustion::DropNewest,
            Exhaustion::CopyToCpu,
        ];
        caps.provenance.video = Provenance::Probe;
        if self.listing == SourceListing::PickerOnly {
            caps.consent = ConsentModel::PerSessionDialog;
        }
        Ok(caps)
    }

    fn sources(&self) -> Result<SourceList, CaptureError> {
        Ok(SourceList {
            sources: if self.listing == SourceListing::Enumerable {
                self.monitors.clone()
            } else {
                Vec::new()
            },
            listing: self.listing,
        })
    }

    fn screenshot(&self, plan: &ScreenshotPlan) -> Result<Shot, CaptureError> {
        let sources = match &plan.target {
            Target::Sources { sources, .. } => sources.clone(),
            Target::Picker(_) => vec![
                self.monitors
                    .first()
                    .cloned()
                    .ok_or(CaptureError::SourceLost)?,
            ],
        };
        let frames: Vec<Captured> = sources
            .iter()
            .map(|source| {
                let size = buffer_size(source);
                Captured {
                    meta: meta(source, size),
                    buffer: NativeBuffer::from_bytes(pixels(self.index_of(source), size)),
                    source: source.clone(),
                }
            })
            .collect();
        let first = sources.first().ok_or(CaptureError::SourceLost)?;
        let negotiated = self.negotiated(first, buffer_size(first));
        Ok(Shot { frames, negotiated })
    }

    fn open(
        &self,
        plan: StreamPlan,
        out: FrameOut,
    ) -> Result<(Box<dyn BackendSession>, NegotiatedStream), CaptureError> {
        let source = if plan.picker.is_some() {
            self.monitors
                .first()
                .cloned()
                .ok_or(CaptureError::SourceLost)?
        } else {
            plan.source.clone()
        };
        let size = buffer_size(&source);
        let index = self.index_of(&source);
        let mut negotiated = self.negotiated(&source, size);
        negotiated.pool_depth = plan.pool_depth;
        negotiated.exhaustion = plan.exhaustion;
        let stop = Arc::new(AtomicBool::new(false));
        let pool = PoolAccounting::new(usize::from(plan.pool_depth));
        let counters = Arc::clone(&self.counters);
        let frames = self.frames;
        let lose_source = self.lose_source;
        let thread_stop = Arc::clone(&stop);
        let thread_pool = pool;
        let depth = plan.pool_depth;
        let policy = plan.exhaustion;
        counters.live_threads.fetch_add(1, Ordering::SeqCst);
        let thread = std::thread::Builder::new()
            .name("fake-capture".to_owned())
            .spawn(move || {
                let mut produced = 0usize;
                let mut slot = 0u32;
                while !thread_stop.load(Ordering::SeqCst) && !out.is_stopped() {
                    if frames != 0 && produced == frames {
                        if lose_source {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    let bytes = pixels(index, size);
                    let meta = meta(&source, size);
                    let delivery = match thread_pool.hold(slot % u32::from(depth.max(1))) {
                        Ok(lease) => {
                            let mut meta = meta;
                            meta.buffer = lease.identity();
                            out.deliver(
                                meta,
                                NativeBuffer::from_bytes(bytes).with_release(move || drop(lease)),
                            )
                        }
                        Err(crate::pool::HoldError::SlotOutOfRange { .. }) => break,
                        Err(crate::pool::HoldError::Exhausted(exhausted)) => {
                            out.exhausted(exhausted, depth, policy);
                            match policy {
                                Exhaustion::Stall => {
                                    let _woken = thread_pool
                                        .released()
                                        .recv_timeout(Duration::from_millis(20));
                                    continue;
                                }
                                Exhaustion::DropNewest => Delivery::Dropped,
                                Exhaustion::CopyToCpu => {
                                    counters.readbacks.fetch_add(1, Ordering::SeqCst);
                                    let mut meta = meta;
                                    meta.copied = Some(CopyStage::ExhaustionReadback);
                                    out.deliver(meta, NativeBuffer::from_bytes(bytes))
                                }
                            }
                        }
                    };
                    match delivery {
                        Delivery::Closed => break,
                        Delivery::Delivered => {
                            counters.delivered.fetch_add(1, Ordering::SeqCst);
                        }
                        Delivery::Deferred | Delivery::Dropped => {}
                    }
                    produced += 1;
                    slot = slot.wrapping_add(1);
                    std::thread::sleep(Duration::from_millis(2));
                }
                if lose_source && frames != 0 && produced == frames {
                    out.fail(CaptureError::SourceLost);
                }
                counters.live_threads.fetch_sub(1, Ordering::SeqCst);
            })
            .map_err(|err| CaptureError::protocol(self.id, "spawn", err))?;
        Ok((
            Box::new(FakeSession {
                stop,
                thread: Some(thread),
            }),
            negotiated,
        ))
    }
}

pub(crate) struct FakeSession {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl BackendSession for FakeSession {
    fn update(&mut self, update: &SessionUpdate) -> Result<NegotiatedStream, CaptureError> {
        Err(CaptureError::unsatisfiable(
            Some(BackendId::LinuxX11),
            match update {
                SessionUpdate::Cursor(_) => crate::types::Requirement::Cursor,
                _ => crate::types::Requirement::Formats,
            },
        ))
    }
}

impl Drop for FakeSession {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _joined = thread.join();
        }
    }
}

/// An upright orientation helper for tests.
pub(crate) const ROT90: Orientation = Orientation {
    mirrored: false,
    rotation: crate::types::Rotation::R90,
};
