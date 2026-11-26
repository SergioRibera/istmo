//! `ext-image-copy-capture-v1` backend (Sway 1.11, Hyprland, COSMIC,
//! labwc, river, Jay), ported from sss's `ext_image_copy`.
//!
//! - Sources from `ext_output_image_capture_source_manager_v1` (outputs)
//!   and, when advertised, `ext_foreign_toplevel_image_capture_source_manager_v1`
//!   with `ext_foreign_toplevel_list_v1` (windows).
//! - The session's constraints (`buffer_size`, `shm_format`,
//!   `dmabuf_device`, `dmabuf_format`) drive allocation; a new batch of
//!   them retires the pool (new `pool_generation`).
//! - Damage and `presentation_time` per frame, the transform per frame.
//! - No sync in the protocol: frames are `Ready` after `ready` only on a
//!   compositor known to wait, `Implicit` otherwise.
//! - Formats are known only once a session for the chosen source is
//!   open: the report holds the theoretical set
//!   (`UnknownUntilNegotiated`).

use std::sync::Arc;

use wayland_client::protocol::wl_buffer::WlBuffer;
use wayland_client::protocol::wl_output::Transform;
use wayland_protocols::ext::image_capture_source::v1::client::ext_image_capture_source_v1::ExtImageCaptureSourceV1;
use wayland_protocols::ext::image_copy_capture::v1::client::{
    ext_image_copy_capture_manager_v1::Options,
    ext_image_copy_capture_session_v1::ExtImageCopyCaptureSessionV1,
};

use crate::types::{
    Availability, BackendCaps, BackendId, BufferMemory, CaptureError, CompositeCaps,
    CompositeSupport, CopyStage, CursorMode, DamageSupport, Exhaustion, FormatCaps,
    NegotiatedStream, Orientation, PixelFormat, ProtectedContent, Provenance, Requirement, Size,
    Source, SourceId, SourceKind, SourceList, SourceListing,
};

use super::super::backend::{
    Backend, BackendSession, Captured, ScreenshotPlan, Shot, StreamPlan, Target,
};
use super::super::output::FrameOut;
use super::wayland::stream::{self, Engine, FrameSource, Offers, Settings};
use super::wayland::{Client, EVENT_TIMEOUT, FrameEvents, orientation_of, shm_format};
use super::wayland::{shm_caps, sync_facts};

const ID: BackendId = BackendId::LinuxExtImageCopy;
const MANAGER: &str = "ext_image_copy_capture_manager_v1";
const OUTPUTS: &str = "ext_output_image_capture_source_manager_v1";
const TOPLEVELS: &str = "ext_foreign_toplevel_image_capture_source_manager_v1";
const TOPLEVEL_LIST: &str = "ext_foreign_toplevel_list_v1";
/// Window source ids are prefixed so they never collide with outputs.
const WINDOW_PREFIX: &str = "toplevel:";

pub(crate) fn backend() -> Arc<dyn Backend> {
    Arc::new(ExtImageCopy)
}

#[derive(Debug)]
struct ExtImageCopy;

fn windows(client: &mut Client) -> Result<Vec<Source>, CaptureError> {
    if !client.has_global(TOPLEVELS, 1) || !client.list_toplevels()? {
        return Ok(Vec::new());
    }
    Ok(client
        .state
        .toplevels
        .iter()
        .filter(|t| !t.closed)
        .filter_map(|t| {
            let identifier = t.identifier.as_ref()?;
            let mut source = Source::new(
                SourceId::new(ID, format!("{WINDOW_PREFIX}{identifier}")),
                SourceKind::Window,
                t.title.clone().unwrap_or_default(),
            );
            source.app_id.clone_from(&t.app_id);
            Some(source)
        })
        .collect())
}

impl Backend for ExtImageCopy {
    fn id(&self) -> BackendId {
        ID
    }

    fn availability(&self) -> Availability {
        if !super::wayland_session() {
            return Availability::NotOnThisSession("no Wayland session".to_owned());
        }
        match Client::connect(ID) {
            Ok(client) if client.has_global(MANAGER, 1) && client.has_global(OUTPUTS, 1) => {
                Availability::Ready
            }
            Ok(_) => Availability::NotOnThisSession(format!("no {MANAGER} / {OUTPUTS} global")),
            Err(err) => Availability::NotOnThisSession(err.to_string()),
        }
    }

    fn joins_auto(&self) -> bool {
        // Checklist passed on headless sway 1.11 (wl_shm). No DMA-BUF
        // session was verified (headless sway renders with pixman).
        true
    }

    fn caps(&self) -> Result<BackendCaps, CaptureError> {
        let mut client = Client::connect(ID)?;
        let mut caps = BackendCaps::new(ID);
        caps.source_kinds = vec![SourceKind::Monitor];
        if client.has_global(TOPLEVELS, 1) && client.has_global(TOPLEVEL_LIST, 1) {
            caps.source_kinds.push(SourceKind::Window);
        }
        caps.provenance.source_kinds = Provenance::Probe;
        caps.provenance.listing = Provenance::Probe;
        // The session's constraints are per source and only arrive once
        // one is open: report the theoretical set, every 8-bit packing
        // as DMA-BUF (any modifier the session lists) and as `wl_shm`.
        caps.video = [
            PixelFormat::Bgrx8,
            PixelFormat::Bgra8,
            PixelFormat::Rgbx8,
            PixelFormat::Rgba8,
        ]
        .into_iter()
        .map(|format| FormatCaps {
            format,
            memory: BufferMemory::DmaBuf,
            modifiers: Vec::new(),
            copies: Vec::new(),
        })
        .collect();
        caps.video.extend(shm_caps());
        caps.provenance.video = Provenance::UnknownUntilNegotiated;
        caps.provenance.producer = Provenance::UnknownUntilNegotiated;
        let feedback = client.feedback().ok().flatten();
        let (sync, notes) = sync_facts(feedback.and_then(|f| f.main_device));
        caps.sync = vec![sync];
        caps.notes = notes;
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
        Ok(caps)
    }

    fn sources(&self) -> Result<SourceList, CaptureError> {
        let mut client = Client::connect(ID)?;
        let mut sources = client.monitors();
        sources.extend(windows(&mut client)?);
        Ok(SourceList {
            sources,
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
        // ext-image-copy waits for damage by itself; the full-copy switch
        // has no protocol equivalent here.
        stream::spawn("ext-image-copy", out, true, move || {
            engine_for(&id, settings)
        })
    }
}

/// Connect, open a capture session on `id` and negotiate.
fn engine_for(id: &SourceId, settings: Settings) -> Result<Engine<ExtSource>, CaptureError> {
    let mut client = Client::connect(ID)?;
    let manager = client
        .image_copy_manager()
        .ok_or_else(|| CaptureError::protocol(ID, "bind", format!("no {MANAGER} global")))?;
    let (capture_source, info, global) =
        if let Some(identifier) = id.value.strip_prefix(WINDOW_PREFIX) {
            let windows = windows(&mut client)?;
            let info = windows
                .into_iter()
                .find(|w| w.id == *id)
                .ok_or_else(|| CaptureError::SourceNotFound(id.clone()))?;
            let handle = client
                .state
                .toplevels
                .iter()
                .find(|t| t.identifier.as_deref() == Some(identifier))
                .map(|t| t.handle.clone())
                .ok_or_else(|| CaptureError::SourceNotFound(id.clone()))?;
            let manager = client.toplevel_source_manager().ok_or_else(|| {
                CaptureError::unsatisfiable(Some(ID), Requirement::Kind(SourceKind::Window))
            })?;
            (manager.create_source(&handle, &client.qh, ()), info, None)
        } else {
            let index = client
                .state
                .outputs
                .iter()
                .position(|o| o.id_value() == id.value)
                .ok_or_else(|| CaptureError::SourceNotFound(id.clone()))?;
            let output = client
                .state
                .outputs
                .get(index)
                .ok_or_else(|| CaptureError::SourceNotFound(id.clone()))?;
            let info = output.source(ID, index == 0);
            let proxy = output.proxy.clone();
            let global = output.global;
            let manager = client.output_source_manager().ok_or_else(|| {
                CaptureError::protocol(ID, "bind", format!("no {OUTPUTS} global"))
            })?;
            (
                manager.create_source(&proxy, &client.qh, ()),
                info,
                Some(global),
            )
        };
    let options = if settings.cursor == CursorMode::Embedded {
        Options::PaintCursors
    } else {
        Options::empty()
    };
    let session = manager.create_session(&capture_source, options, &client.qh, ());
    let source = ExtSource {
        session,
        capture_source,
        global,
        generation: 0,
    };
    let sync = sync_facts(None).0;
    Engine::negotiate(client, source, info, settings, sync)
}

/// One capture session.
struct ExtSource {
    session: ExtImageCopyCaptureSessionV1,
    capture_source: ExtImageCaptureSourceV1,
    /// The output's registry name (none for a window).
    global: Option<u32>,
    /// Constraint batch the last offers came from.
    generation: u32,
}

impl FrameSource for ExtSource {
    fn offers(&mut self, client: &mut Client) -> Result<Offers, CaptureError> {
        let answered = client.dispatch_until("session constraints", EVENT_TIMEOUT, |s| {
            s.constraints.generation > 0 || s.constraints.stopped
        })?;
        let constraints = &client.state.constraints;
        if constraints.stopped {
            return Err(CaptureError::SourceLost);
        }
        if !answered {
            return Err(CaptureError::protocol(
                ID,
                "session constraints",
                "no `done` within 2 s",
            ));
        }
        self.generation = constraints.generation;
        let (width, height) = constraints.size.unwrap_or((0, 0));
        let shm = constraints
            .shm_formats
            .iter()
            .filter_map(|code| Some((*code, width * shm_format(*code)?.bytes_per_pixel())))
            .collect();
        Ok(Offers {
            size: Size::new(width, height),
            shm,
            dmabuf: constraints.dmabuf_formats.clone(),
            device: constraints.dmabuf_device,
        })
    }

    fn abandon(&mut self) {}

    fn copy(
        &mut self,
        client: &mut Client,
        buffer: &WlBuffer,
        size: Size,
        _with_damage: bool,
        stop: &dyn Fn() -> bool,
    ) -> Result<Option<FrameEvents>, CaptureError> {
        client.state.frame = FrameEvents::default();
        let frame = self.session.create_frame(&client.qh, ());
        frame.attach_buffer(buffer);
        // The client never writes into its buffers: the whole buffer is
        // stale for the compositor (it may hold another slot's frame).
        frame.damage_buffer(
            0,
            0,
            i32::try_from(size.width).unwrap_or(i32::MAX),
            i32::try_from(size.height).unwrap_or(i32::MAX),
        );
        frame.capture();
        // A stopped session fails the frame (`failed`), which ends the wait.
        let events = stream::wait_frame(client, stop)?;
        frame.destroy();
        if client.state.constraints.stopped {
            return Err(CaptureError::SourceLost);
        }
        Ok(events)
    }

    fn orientation(&self, _client: &Client, events: &FrameEvents) -> Orientation {
        orientation_of(events.transform.unwrap_or(Transform::Normal), false)
    }

    fn lost(&self, client: &Client) -> bool {
        client.state.constraints.stopped || self.global.is_some_and(|g| client.removed(g))
    }
}

impl Drop for ExtSource {
    fn drop(&mut self) {
        self.session.destroy();
        self.capture_source.destroy();
    }
}
