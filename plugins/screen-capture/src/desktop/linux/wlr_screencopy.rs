//! `zwlr_screencopy_manager_v1` backend (wlroots compositors, niri),
//! ported from ferricast's `wayland_direct`.
//!
//! - DMA-BUF zero-copy: GBM buffers on the `linux-dmabuf` feedback main
//!   device, modifiers = feedback tranche for the fourcc ∩ the caller's
//!   tier; reused from a pool, each frame a clone of its slot's
//!   `Arc<DmaBuf>` tagged with its `BufferIdentity`.
//! - `wl_shm` when a CPU tier is listed: pixels copied out of the memfd
//!   (`CopyStage::ShmTransport`), never by mapping a GPU buffer.
//! - `copy_with_damage` by default (an idle output costs nothing; the
//!   pacer repeats frames); full copies for screenshots and with
//!   `ISTMO_SCREEN_CAPTURE_FULL_COPY=1`.
//! - No transform event: orientation is the output's `wl_output`
//!   transform composed with the frame's `Y_INVERT`.
//! - `ready` means the copy finished; frames are `Ready` on compositors
//!   known to wait for their render first (niri), `Implicit` elsewhere.
//! - ferricast's silent switch to `wl_shm` (allocation failures, refused
//!   imports, every buffer leased) is gone: a refused tier moves to the
//!   next listed one with `Downgraded`, a held pool follows `Exhaustion`.

use std::sync::Arc;

use wayland_client::protocol::wl_buffer::WlBuffer;
use wayland_client::protocol::wl_output::WlOutput;
use wayland_protocols_wlr::screencopy::v1::client::{
    zwlr_screencopy_frame_v1::ZwlrScreencopyFrameV1,
    zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1,
};

use crate::types::{
    Availability, BackendCaps, BackendId, CapabilityNote, CaptureError, CompositeCaps,
    CompositeSupport, CopyStage, CursorMode, DamageSupport, Exhaustion, GpuIdentity,
    NegotiatedStream, ProtectedContent, Provenance, Requirement, Size, SourceKind, SourceList,
    SourceListing, SyncKind,
};

use super::super::backend::{
    Backend, BackendSession, Captured, ScreenshotPlan, Shot, StreamPlan, Target,
};
use super::super::output::FrameOut;
use super::gbm::Allocator;
use super::wayland::stream::{self, Engine, FrameSource, Offers, Settings};
use super::wayland::{
    Client, EVENT_TIMEOUT, Feedback, FrameEvents, FrameStatus, dmabuf_caps, orientation_of,
    shm_caps, sync_facts,
};

const ID: BackendId = BackendId::LinuxWlrScreencopy;
const MANAGER: &str = "zwlr_screencopy_manager_v1";

pub(crate) fn backend() -> Arc<dyn Backend> {
    Arc::new(WlrScreencopy)
}

/// The backend: stateless, every call opens its own connection (the
/// capture thread keeps its own for the session's life).
#[derive(Debug)]
struct WlrScreencopy;

impl Backend for WlrScreencopy {
    fn id(&self) -> BackendId {
        ID
    }

    fn availability(&self) -> Availability {
        if !super::wayland_session() {
            return Availability::NotOnThisSession("no Wayland session".to_owned());
        }
        match Client::connect(ID) {
            Ok(client) if client.has_global(MANAGER, 3) => Availability::Ready,
            Ok(_) => Availability::NotOnThisSession(format!("no {MANAGER} v3 global")),
            Err(err) => Availability::NotOnThisSession(err.to_string()),
        }
    }

    fn joins_auto(&self) -> bool {
        // Checklist passed on headless sway 1.11 (wl_shm) and live
        // niri 26.04 (NVIDIA DMA-BUF).
        true
    }

    fn caps(&self) -> Result<BackendCaps, CaptureError> {
        let mut client = Client::connect(ID)?;
        let feedback = client.feedback().ok().flatten();
        let mut caps = BackendCaps::new(ID);
        caps.provenance.source_kinds = Provenance::Probe;
        caps.provenance.listing = Provenance::Probe;
        let device = feedback.as_ref().and_then(|f| f.main_device);
        let allocator = feedback.as_ref().map(|_| Allocator::open(device));
        caps.video = match (&feedback, &allocator) {
            (Some(feedback), Some(Ok(_))) => {
                caps.provenance.video = Provenance::Probe;
                dmabuf_caps(feedback)
            }
            _ => Vec::new(),
        };
        if caps.video.is_empty() {
            caps.notes.push(CapabilityNote::NoDmabufFeedback);
        }
        if matches!(allocator, Some(Err(_))) {
            caps.notes.push(CapabilityNote::NoGbmDevice);
        }
        caps.video.extend(shm_caps());
        let producer = match &allocator {
            Some(Ok(allocator)) => {
                caps.provenance.producer = Provenance::Probe;
                Some(allocator.dev_t)
            }
            _ => None,
        };
        caps.producer = producer.map(GpuIdentity::Drm);
        let (sync, notes) = sync_facts(producer);
        caps.sync = vec![sync, SyncKind::Ready];
        caps.sync.dedup();
        caps.notes.extend(notes);
        caps.cursor = vec![CursorMode::Hidden, CursorMode::Embedded];
        caps.damage = DamageSupport::Rects;
        caps.composite = CompositeCaps {
            screenshot: CompositeSupport::CpuOnly,
            streaming: CompositeSupport::Unsupported,
        };
        caps.pool.exhaustion = vec![
            Exhaustion::Stall,
            Exhaustion::DropNewest,
            Exhaustion::CopyToCpu,
        ];
        caps.copies = vec![CopyStage::ShmTransport];
        caps.protected_content = ProtectedContent::MaskedUndetectable;
        caps.max_fps = None;
        Ok(caps)
    }

    fn sources(&self) -> Result<SourceList, CaptureError> {
        let client = Client::connect(ID)?;
        Ok(SourceList {
            sources: client.monitors(),
            listing: SourceListing::Enumerable,
        })
    }

    fn screenshot(&self, plan: &ScreenshotPlan) -> Result<Shot, CaptureError> {
        let Target::Sources { sources, .. } = &plan.target else {
            return Err(CaptureError::unsatisfiable(Some(ID), Requirement::Picker));
        };
        let mut frames = Vec::with_capacity(sources.len());
        let mut negotiated: Option<NegotiatedStream> = None;
        for wanted in sources {
            if wanted.kind != SourceKind::Monitor {
                return Err(CaptureError::unsatisfiable(
                    Some(ID),
                    Requirement::Kind(wanted.kind),
                ));
            }
            let settings = Settings {
                formats: plan.formats.clone(),
                exhaustion: Exhaustion::Stall,
                pool_depth: 2,
                cursor: if plan.cursor.mode == CursorMode::Hidden {
                    CursorMode::Hidden
                } else {
                    CursorMode::Embedded
                },
                damage: DamageSupport::Rects,
            };
            let mut engine = engine_for(&wanted.id, settings)?;
            let (meta, buffer) = engine.screenshot()?;
            if negotiated.is_none() {
                negotiated = Some(engine.negotiated());
            }
            frames.push(Captured {
                meta,
                buffer,
                source: engine.info.clone(),
            });
        }
        let negotiated = negotiated.ok_or(CaptureError::SourceLost)?;
        Ok(Shot { frames, negotiated })
    }

    fn open(
        &self,
        plan: StreamPlan,
        out: FrameOut,
    ) -> Result<(Box<dyn BackendSession>, NegotiatedStream), CaptureError> {
        if plan.picker.is_some() {
            return Err(CaptureError::unsatisfiable(Some(ID), Requirement::Picker));
        }
        if plan.source.kind != SourceKind::Monitor {
            return Err(CaptureError::unsatisfiable(
                Some(ID),
                Requirement::Kind(plan.source.kind),
            ));
        }
        let settings = Settings {
            formats: plan.formats.clone(),
            exhaustion: plan.exhaustion,
            pool_depth: plan.pool_depth,
            cursor: crate::select::cursor_mode(
                plan.cursor,
                &[CursorMode::Hidden, CursorMode::Embedded],
            ),
            damage: DamageSupport::Rects,
        };
        let id = plan.source.id;
        stream::spawn("wlr-screencopy", out, stream::damage_driven(), move || {
            engine_for(&id, settings)
        })
    }
}

/// Connect, find the output, read the feedback and negotiate.
fn engine_for(
    id: &crate::types::SourceId,
    settings: Settings,
) -> Result<Engine<WlrSource>, CaptureError> {
    let mut client = Client::connect(ID)?;
    let manager = client
        .screencopy_manager()
        .ok_or_else(|| CaptureError::protocol(ID, "bind", format!("no {MANAGER} v3 global")))?;
    let feedback = client.feedback().ok().flatten();
    let info = client.monitors().into_iter().find(|s| s.id == *id);
    let output = client
        .output(id)
        .ok_or_else(|| CaptureError::SourceNotFound(id.clone()))?;
    let source = WlrSource {
        manager,
        output: output.proxy.clone(),
        global: output.global,
        cursor: settings.cursor == CursorMode::Embedded,
        frame: None,
        feedback,
    };
    let info = info.ok_or_else(|| CaptureError::SourceNotFound(id.clone()))?;
    let (sync, _notes) = sync_facts(source.feedback.as_ref().and_then(|f| f.main_device));
    Engine::negotiate(client, source, info, settings, sync)
}

/// One output captured with `capture_output`.
struct WlrSource {
    manager: ZwlrScreencopyManagerV1,
    output: WlOutput,
    global: u32,
    cursor: bool,
    /// The frame `offers` created, waiting for its copy.
    frame: Option<ZwlrScreencopyFrameV1>,
    feedback: Option<Feedback>,
}

impl FrameSource for WlrSource {
    fn offers(&mut self, client: &mut Client) -> Result<Offers, CaptureError> {
        self.abandon();
        client.state.frame = FrameEvents::default();
        let frame =
            self.manager
                .capture_output(i32::from(self.cursor), &self.output, &client.qh, ());
        let answered = client.dispatch_until("capture_output", EVENT_TIMEOUT, |s| {
            s.frame.buffer_done || s.frame.status == FrameStatus::Failed
        })?;
        if !answered || client.state.frame.status == FrameStatus::Failed {
            frame.destroy();
            return Err(if self.lost(client) {
                CaptureError::SourceLost
            } else {
                CaptureError::protocol(ID, "capture_output", "the compositor offered no buffer")
            });
        }
        self.frame = Some(frame);
        let events = &client.state.frame;
        let (width, height) = events
            .shm
            .map(|s| (s.width, s.height))
            .or_else(|| events.dmabuf.map(|d| (d.width, d.height)))
            .unwrap_or((0, 0));
        let dmabuf = events
            .dmabuf
            .map(|offer| {
                let modifiers = self
                    .feedback
                    .as_ref()
                    .and_then(|f| f.accepted.get(&offer.fourcc))
                    .cloned()
                    .unwrap_or_default();
                vec![(offer.fourcc, modifiers)]
            })
            .unwrap_or_default();
        Ok(Offers {
            size: Size::new(width, height),
            shm: events
                .shm
                .map(|s| vec![(s.format, s.stride)])
                .unwrap_or_default(),
            dmabuf,
            device: self.feedback.as_ref().and_then(|f| f.main_device),
        })
    }

    fn abandon(&mut self) {
        if let Some(frame) = self.frame.take() {
            frame.destroy();
        }
    }

    fn copy(
        &mut self,
        client: &mut Client,
        buffer: &WlBuffer,
        _size: Size,
        with_damage: bool,
        stop: &dyn Fn() -> bool,
    ) -> Result<Option<FrameEvents>, CaptureError> {
        let Some(frame) = self.frame.take() else {
            return Err(CaptureError::protocol(ID, "copy", "no frame prepared"));
        };
        client.state.frame.status = FrameStatus::Pending;
        client.state.frame.damage.clear();
        if with_damage {
            frame.copy_with_damage(buffer);
        } else {
            frame.copy(buffer);
        }
        let lost = |client: &Client| client.removed(self.global);
        let events = stream::wait_frame(client, &|| stop())?;
        // Single-use object: destroyed on every path, or each capture
        // leaks a protocol object on both sides.
        frame.destroy();
        if events.is_none() && lost(client) {
            return Err(CaptureError::SourceLost);
        }
        Ok(events)
    }

    fn orientation(&self, client: &Client, events: &FrameEvents) -> crate::types::Orientation {
        let transform = client
            .state
            .outputs
            .iter()
            .find(|o| o.global == self.global)
            .map_or(
                wayland_client::protocol::wl_output::Transform::Normal,
                |o| o.transform,
            );
        orientation_of(transform, events.y_invert)
    }

    fn lost(&self, client: &Client) -> bool {
        client.removed(self.global)
    }
}

impl Drop for WlrSource {
    fn drop(&mut self) {
        self.abandon();
    }
}
