//! Wayland plumbing shared by the direct backends (wlr-screencopy,
//! ext-image-copy): one connection with its globals, the outputs
//! (`wl_output` + xdg-output), `linux-dmabuf` feedback, `wl_shm` buffers
//! read back without `mmap`, and one dispatch state that handles every
//! object either backend creates.

pub(crate) mod stream;

use std::collections::HashMap;
use std::os::fd::{AsFd, OwnedFd};
use std::time::{Duration, Instant};

use wayland_client::globals::{GlobalList, GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_buffer::WlBuffer;
use wayland_client::protocol::wl_output::{self, WlOutput};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_shm::{self, WlShm};
use wayland_client::protocol::wl_shm_pool::WlShmPool;
use wayland_client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum, event_created_child,
};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::{
    ext_foreign_toplevel_handle_v1::{self, ExtForeignToplevelHandleV1},
    ext_foreign_toplevel_list_v1::{self, ExtForeignToplevelListV1},
};
use wayland_protocols::ext::image_capture_source::v1::client::{
    ext_foreign_toplevel_image_capture_source_manager_v1::ExtForeignToplevelImageCaptureSourceManagerV1,
    ext_image_capture_source_v1::ExtImageCaptureSourceV1,
    ext_output_image_capture_source_manager_v1::ExtOutputImageCaptureSourceManagerV1,
};
use wayland_protocols::ext::image_copy_capture::v1::client::{
    ext_image_copy_capture_frame_v1::{self, ExtImageCopyCaptureFrameV1},
    ext_image_copy_capture_manager_v1::ExtImageCopyCaptureManagerV1,
    ext_image_copy_capture_session_v1::{self, ExtImageCopyCaptureSessionV1},
};
use wayland_protocols::wp::linux_dmabuf::zv1::client::{
    zwp_linux_buffer_params_v1::{self, ZwpLinuxBufferParamsV1},
    zwp_linux_dmabuf_feedback_v1::{self, ZwpLinuxDmabufFeedbackV1},
    zwp_linux_dmabuf_v1::{self, ZwpLinuxDmabufV1},
};
use wayland_protocols::xdg::xdg_output::zv1::client::{
    zxdg_output_manager_v1::ZxdgOutputManagerV1,
    zxdg_output_v1::{self, ZxdgOutputV1},
};
use wayland_protocols_wlr::screencopy::v1::client::{
    zwlr_screencopy_frame_v1::{self, ZwlrScreencopyFrameV1},
    zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1,
};

use super::gbm::is_nvidia;
#[cfg(feature = "linux-wlr-screencopy")]
use crate::select::MOD_INVALID;
use crate::types::{
    BackendId, BufferMemory, CapabilityNote, CaptureError, CopyStage, Damage, FormatCaps,
    Orientation, PixelFormat, Rect, Rotation, Size, Source, SourceId, SourceKind, SyncKind,
};

/// How long one protocol exchange may take before it counts as lost.
pub(crate) const EVENT_TIMEOUT: Duration = Duration::from_secs(2);

/// Compositors known to send `ready` only after their render finished
/// (niri waits on its render's sync point), so frames are `Ready`
/// rather than `Implicit`.
pub(crate) fn compositor_waits_before_ready() -> bool {
    std::env::var_os("NIRI_SOCKET").is_some()
        || std::env::var("XDG_CURRENT_DESKTOP")
            .is_ok_and(|d| d.split(':').any(|p| p.eq_ignore_ascii_case("niri")))
}

/// `wl_output.transform` as the fix-up a frame carries, `Y_INVERT`
/// (when set) applied first.
pub(crate) const fn orientation_of(transform: wl_output::Transform, y_invert: bool) -> Orientation {
    use wl_output::Transform;
    let (mirrored, rotation) = match transform {
        Transform::_90 => (false, Rotation::R90),
        Transform::_180 => (false, Rotation::R180),
        Transform::_270 => (false, Rotation::R270),
        Transform::Flipped => (true, Rotation::R0),
        Transform::Flipped90 => (true, Rotation::R90),
        Transform::Flipped180 => (true, Rotation::R180),
        Transform::Flipped270 => (true, Rotation::R270),
        // `Normal`, and a future transform: a mis-rotated frame beats a
        // dropped one.
        _ => (false, Rotation::R0),
    };
    let transform = Orientation { mirrored, rotation };
    if y_invert {
        Orientation::Y_INVERT.then(transform)
    } else {
        transform
    }
}

/// A `wl_shm` / DRM format code as a plugin pixel format. `wl_shm`
/// codes 0 and 1 are ARGB / XRGB8888; every other code is the DRM
/// fourcc.
pub(crate) const fn shm_format(code: u32) -> Option<PixelFormat> {
    match code {
        0 => Some(PixelFormat::Bgra8),
        1 => Some(PixelFormat::Bgrx8),
        other => PixelFormat::from_drm_fourcc(other),
    }
}

/// The `wl_shm` code of a pixel format.
#[cfg(test)]
const fn shm_code(format: PixelFormat) -> u32 {
    match format {
        PixelFormat::Bgra8 => 0,
        PixelFormat::Bgrx8 => 1,
        other => other.drm_fourcc(),
    }
}

/// `ready` semantics on this compositor, plus the caveats that apply.
pub(crate) fn sync_facts(producer: Option<u64>) -> (SyncKind, Vec<CapabilityNote>) {
    if compositor_waits_before_ready() {
        return (SyncKind::Ready, Vec::new());
    }
    let mut notes = vec![CapabilityNote::ReadyNotKnownToWait];
    if producer.is_some_and(is_nvidia) {
        notes.push(CapabilityNote::NvidiaImplicitSyncBestEffort);
    }
    (SyncKind::Implicit, notes)
}

/// The CPU formats every `wl_shm` compositor can deliver (ARGB / XRGB
/// are mandatory, the byte-swapped ones common).
pub(crate) fn shm_caps() -> Vec<FormatCaps> {
    [
        PixelFormat::Bgrx8,
        PixelFormat::Bgra8,
        PixelFormat::Rgbx8,
        PixelFormat::Rgba8,
    ]
    .into_iter()
    .map(|format| FormatCaps {
        format,
        memory: BufferMemory::Cpu,
        modifiers: Vec::new(),
        copies: vec![CopyStage::ShmTransport],
    })
    .collect()
}

#[cfg(feature = "linux-wlr-screencopy")]
/// The DMA-BUF formats `feedback` lists, without the implicit modifier.
pub(crate) fn dmabuf_caps(feedback: &Feedback) -> Vec<FormatCaps> {
    let mut caps: Vec<FormatCaps> = feedback
        .accepted
        .iter()
        .filter_map(|(fourcc, modifiers)| {
            let modifiers: Vec<u64> = modifiers
                .iter()
                .copied()
                .filter(|m| *m != MOD_INVALID)
                .collect();
            (!modifiers.is_empty()).then_some(FormatCaps {
                format: PixelFormat::from_drm_fourcc(*fourcc)?,
                memory: BufferMemory::DmaBuf,
                modifiers,
                copies: Vec::new(),
            })
        })
        .collect();
    caps.sort_by_key(|c| c.format.drm_fourcc());
    caps
}

/// `dev_t` as the protocol sends it: 8 bytes, native order.
pub(crate) fn decode_dev_t(device: &[u8]) -> Option<u64> {
    <[u8; 8]>::try_from(device).ok().map(u64::from_ne_bytes)
}

/// One output as the compositor describes it.
#[derive(Debug, Clone)]
pub(crate) struct OutputInfo {
    /// The registry name of its global (removal is announced by it).
    pub(crate) global: u32,
    pub(crate) proxy: WlOutput,
    pub(crate) name: Option<String>,
    pub(crate) description: Option<String>,
    pub(crate) make: Option<String>,
    pub(crate) model: Option<String>,
    /// xdg-output logical geometry.
    pub(crate) logical_position: Option<(i32, i32)>,
    pub(crate) logical_size: Option<(i32, i32)>,
    /// Current mode (buffer pixels, before the transform).
    pub(crate) mode: Option<(i32, i32)>,
    pub(crate) refresh_mhz: Option<u32>,
    pub(crate) scale: i32,
    pub(crate) transform: wl_output::Transform,
    pub(crate) position: (i32, i32),
}

impl OutputInfo {
    const fn new(global: u32, proxy: WlOutput) -> Self {
        Self {
            global,
            proxy,
            name: None,
            description: None,
            make: None,
            model: None,
            logical_position: None,
            logical_size: None,
            mode: None,
            refresh_mhz: None,
            scale: 1,
            transform: wl_output::Transform::Normal,
            position: (0, 0),
        }
    }

    /// The source id value: the connector name, else the global.
    pub(crate) fn id_value(&self) -> String {
        self.name
            .clone()
            .unwrap_or_else(|| format!("wl_output-{}", self.global))
    }

    /// The source as the plugin reports it.
    pub(crate) fn source(&self, backend: BackendId, primary: bool) -> Source {
        let mut source = Source::new(
            SourceId::new(backend, self.id_value()),
            SourceKind::Monitor,
            self.description
                .clone()
                .or_else(|| self.name.clone())
                .unwrap_or_else(|| self.id_value()),
        );
        let orientation = orientation_of(self.transform, false);
        let mode = self
            .mode
            .and_then(|(w, h)| Some(Size::new(u32::try_from(w).ok()?, u32::try_from(h).ok()?)));
        // Upright physical size: the mode turned by the transform.
        let upright = mode.map(|m| orientation.applied_size(m));
        let logical = match (self.logical_position, self.logical_size) {
            (Some((x, y)), Some((w, h))) => Some(Rect::new(
                x,
                y,
                u32::try_from(w).unwrap_or(0),
                u32::try_from(h).unwrap_or(0),
            )),
            _ => upright.map(|u| {
                let scale = u32::try_from(self.scale.max(1)).unwrap_or(1);
                Rect::new(
                    self.position.0,
                    self.position.1,
                    u.width / scale,
                    u.height / scale,
                )
            }),
        };
        source.geometry = logical;
        source.physical_size = upright;
        source.scale_permille = match (upright, logical) {
            // Fractional scales only show in the ratio of the two sizes.
            (Some(u), Some(l)) if l.width > 0 => Some(u.width.saturating_mul(1000) / l.width),
            _ => Some(u32::try_from(self.scale.max(1)).unwrap_or(1) * 1000),
        };
        source.refresh_mhz = self.refresh_mhz;
        source.orientation = orientation;
        source.flags.primary = primary;
        source
    }
}

/// A toplevel from `ext_foreign_toplevel_list_v1`.
#[derive(Debug, Clone)]
pub(crate) struct ToplevelInfo {
    pub(crate) handle: ExtForeignToplevelHandleV1,
    pub(crate) identifier: Option<String>,
    pub(crate) title: Option<String>,
    pub(crate) app_id: Option<String>,
    pub(crate) closed: bool,
}

/// `linux-dmabuf` feedback: the compositor's main device and the
/// modifiers it imports per fourcc, tranche order kept.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Feedback {
    pub(crate) main_device: Option<u64>,
    pub(crate) accepted: HashMap<u32, Vec<u64>>,
}

#[derive(Debug, Default)]
struct FeedbackProgress {
    done: bool,
    main_device: Option<u64>,
    table: Option<(OwnedFd, u32)>,
    tranches: Vec<(Option<u64>, Vec<u16>)>,
    current: Option<(Option<u64>, Vec<u16>)>,
}

/// The `wl_shm` offer of an in-flight wlr frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ShmOffer {
    pub(crate) format: u32,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) stride: u32,
}

/// The DMA-BUF offer of an in-flight wlr frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DmabufOffer {
    pub(crate) fourcc: u32,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

/// Outcome of a frame copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum FrameStatus {
    #[default]
    Pending,
    Ready,
    Failed,
}

/// Events of the in-flight frame (wlr or ext).
#[derive(Debug, Clone, Default)]
pub(crate) struct FrameEvents {
    pub(crate) shm: Option<ShmOffer>,
    pub(crate) dmabuf: Option<DmabufOffer>,
    pub(crate) buffer_done: bool,
    pub(crate) y_invert: bool,
    pub(crate) transform: Option<wl_output::Transform>,
    pub(crate) damage: Vec<Rect>,
    /// `CLOCK_MONOTONIC` nanoseconds.
    pub(crate) pts_ns: Option<u64>,
    pub(crate) status: FrameStatus,
}

impl FrameEvents {
    /// The damage as the frame meta carries it.
    pub(crate) fn damage(&self, full: bool) -> Damage {
        if full || self.damage.is_empty() {
            Damage::Full
        } else {
            Damage::Rects(self.damage.clone())
        }
    }
}

/// ext-image-copy session constraints (`done` closes a batch).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Constraints {
    pub(crate) size: Option<(u32, u32)>,
    pub(crate) shm_formats: Vec<u32>,
    pub(crate) dmabuf_device: Option<u64>,
    /// fourcc and its modifiers, compositor order.
    pub(crate) dmabuf_formats: Vec<(u32, Vec<u64>)>,
    /// Bumped on every `done`.
    pub(crate) generation: u32,
    pub(crate) stopped: bool,
}

/// The dispatch state of every direct-backend connection.
#[derive(Debug, Default)]
pub(crate) struct WlState {
    pub(crate) outputs: Vec<OutputInfo>,
    pub(crate) toplevels: Vec<ToplevelInfo>,
    /// Registry names of globals removed since the connection opened.
    pub(crate) removed: Vec<u32>,
    pub(crate) frame: FrameEvents,
    pub(crate) constraints: Constraints,
    /// Batch being collected until the next `done`.
    pending_constraints: Constraints,
    v3_modifiers: HashMap<u32, Vec<u64>>,
    feedback: FeedbackProgress,
}

/// A connection with its queue and the globals both backends use.
pub(crate) struct Client {
    pub(crate) backend: BackendId,
    queue: EventQueue<WlState>,
    pub(crate) qh: QueueHandle<WlState>,
    globals: GlobalList,
    pub(crate) state: WlState,
    shm: WlShm,
    dmabuf: Option<ZwpLinuxDmabufV1>,
    // Last: the objects above are destroyed before the connection goes.
    _conn: Connection,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("backend", &self.backend)
            .field("outputs", &self.state.outputs.len())
            .finish_non_exhaustive()
    }
}

impl Client {
    /// Connect, bind `wl_shm`, every output (with xdg-output) and
    /// `linux-dmabuf`, and read the outputs' descriptions.
    ///
    /// # Errors
    /// No compositor, or a protocol error, naming `backend`.
    pub(crate) fn connect(backend: BackendId) -> Result<Self, CaptureError> {
        let conn = Connection::connect_to_env()
            .map_err(|e| CaptureError::protocol(backend, "wayland connect", e))?;
        let (globals, queue) = registry_queue_init::<WlState>(&conn)
            .map_err(|e| CaptureError::protocol(backend, "wayland registry", e))?;
        let qh = queue.handle();
        let shm: WlShm = globals
            .bind(&qh, 1..=1, ())
            .map_err(|e| CaptureError::protocol(backend, "bind wl_shm", e))?;
        let dmabuf: Option<ZwpLinuxDmabufV1> = globals.bind(&qh, 3..=4, ()).ok();
        let xdg: Option<ZxdgOutputManagerV1> = globals.bind(&qh, 2..=3, ()).ok();
        let mut state = WlState::default();
        for global in globals.contents().clone_list() {
            if global.interface == WlOutput::interface().name {
                let proxy: WlOutput =
                    globals
                        .registry()
                        .bind(global.name, global.version.min(4), &qh, global.name);
                if let Some(xdg) = &xdg {
                    xdg.get_xdg_output(&proxy, &qh, global.name);
                }
                state.outputs.push(OutputInfo::new(global.name, proxy));
            }
        }
        let mut client = Self {
            backend,
            queue,
            qh,
            globals,
            state,
            shm,
            dmabuf,
            _conn: conn,
        };
        client.roundtrip("outputs")?;
        // xdg-output answers after the outputs' own events.
        client.roundtrip("xdg outputs")?;
        Ok(client)
    }

    /// Whether the compositor advertises `interface` at `min_version` or
    /// later.
    pub(crate) fn has_global(&self, interface: &str, min_version: u32) -> bool {
        self.globals.contents().with_list(|list| {
            list.iter()
                .any(|g| g.interface == interface && g.version >= min_version)
        })
    }

    /// Whether DMA-BUF buffers can be wrapped at all.
    pub(crate) const fn has_dmabuf(&self) -> bool {
        self.dmabuf.is_some()
    }

    /// One round trip.
    ///
    /// # Errors
    /// A protocol or socket error.
    pub(crate) fn roundtrip(&mut self, step: &str) -> Result<(), CaptureError> {
        self.queue
            .roundtrip(&mut self.state)
            .map(|_| ())
            .map_err(|e| CaptureError::protocol(self.backend, step, e))
    }

    /// Dispatch until `done(state)` holds or `timeout` elapses; `false`
    /// on timeout. Polls the socket with the remaining budget so a quiet
    /// compositor cannot hold the thread past its deadline.
    ///
    /// # Errors
    /// A protocol or socket error.
    pub(crate) fn dispatch_until(
        &mut self,
        step: &str,
        timeout: Duration,
        mut done: impl FnMut(&WlState) -> bool,
    ) -> Result<bool, CaptureError> {
        let backend = self.backend;
        let fail = |e: &dyn std::fmt::Display| CaptureError::protocol(backend, step, e.to_string());
        let start = Instant::now();
        loop {
            self.queue
                .dispatch_pending(&mut self.state)
                .map_err(|e| fail(&e))?;
            if done(&self.state) {
                return Ok(true);
            }
            let remaining = timeout.saturating_sub(start.elapsed());
            if remaining.is_zero() {
                return Ok(false);
            }
            self.queue.flush().map_err(|e| fail(&e))?;
            let Some(guard) = self.queue.prepare_read() else {
                continue;
            };
            let ready = {
                let fd = guard.connection_fd();
                let mut fds = [rustix::event::PollFd::new(
                    &fd,
                    rustix::event::PollFlags::IN,
                )];
                let timespec = rustix::event::Timespec {
                    tv_sec: i64::try_from(remaining.as_secs()).unwrap_or(i64::MAX),
                    tv_nsec: i64::from(remaining.subsec_nanos()),
                };
                match rustix::event::poll(&mut fds, Some(&timespec)) {
                    Ok(n) => n > 0,
                    Err(rustix::io::Errno::INTR) => false,
                    Err(e) => return Err(fail(&e)),
                }
            };
            if ready {
                match guard.read() {
                    Ok(_) => {}
                    Err(wayland_client::backend::WaylandError::Io(e))
                        if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(e) => return Err(fail(&e)),
                }
            }
        }
    }

    /// The outputs as sources, the first one primary (Wayland has no
    /// notion of a primary output).
    pub(crate) fn monitors(&self) -> Vec<Source> {
        let mut sources: Vec<Source> = self
            .state
            .outputs
            .iter()
            .map(|o| o.source(self.backend, false))
            .collect();
        // Registry order is arbitrary (sway lists the newest output
        // first): order by position, the top-left output is primary.
        sources.sort_by_key(|s| s.geometry.map(|g| (g.y, g.x)));
        if let Some(first) = sources.first_mut() {
            first.flags.primary = true;
        }
        sources
    }

    #[cfg(feature = "linux-wlr-screencopy")]
    /// The output a source id names.
    pub(crate) fn output(&self, id: &SourceId) -> Option<&OutputInfo> {
        self.state.outputs.iter().find(|o| o.id_value() == id.value)
    }

    /// Whether the output with registry name `global` went away.
    pub(crate) fn removed(&self, global: u32) -> bool {
        self.state.removed.contains(&global)
    }

    /// The compositor's `linux-dmabuf` feedback: its main device and the
    /// modifiers it imports there, per fourcc, tranche order kept.
    /// `None` without the global.
    ///
    /// # Errors
    /// A protocol or socket error.
    pub(crate) fn feedback(&mut self) -> Result<Option<Feedback>, CaptureError> {
        let Some(dmabuf) = self.dmabuf.clone() else {
            return Ok(None);
        };
        if dmabuf.version() < 4 {
            // v3: no feedback object; the global listed its pairs with
            // `modifier` events during the first round trip. No main
            // device: allocation falls back to the first render node.
            return Ok(Some(Feedback {
                main_device: None,
                accepted: self.state.v3_modifiers.clone(),
            }));
        }
        self.state.feedback = FeedbackProgress::default();
        let feedback = dmabuf.get_default_feedback(&self.qh, ());
        let done = self.dispatch_until("dmabuf feedback", EVENT_TIMEOUT, |s| s.feedback.done)?;
        feedback.destroy();
        if !done {
            return Err(CaptureError::protocol(
                self.backend,
                "dmabuf feedback",
                "no `done` within 2 s",
            ));
        }
        let progress = std::mem::take(&mut self.state.feedback);
        let mut accepted: HashMap<u32, Vec<u64>> = HashMap::new();
        if let (Some((fd, size)), Some(main)) = (progress.table.as_ref(), progress.main_device) {
            // The table is read with `pread`, not mapped: no unsafe
            // mapping of a compositor-owned fd.
            let mut table = vec![0u8; usize::try_from(*size).unwrap_or(0)];
            let read = rustix::io::pread(fd, &mut table[..], 0)
                .map_err(|e| CaptureError::protocol(self.backend, "dmabuf format table", e))?;
            table.truncate(read);
            for (target, indices) in &progress.tranches {
                if *target != Some(main) {
                    continue;
                }
                for index in indices {
                    let at = usize::from(*index) * 16;
                    let Some(entry) = table.get(at..at + 16) else {
                        continue;
                    };
                    let format = entry
                        .get(0..4)
                        .and_then(|b| <[u8; 4]>::try_from(b).ok())
                        .map_or(0, u32::from_ne_bytes);
                    let modifier = entry
                        .get(8..16)
                        .and_then(|b| <[u8; 8]>::try_from(b).ok())
                        .map_or(0, u64::from_ne_bytes);
                    let list = accepted.entry(format).or_default();
                    if !list.contains(&modifier) {
                        list.push(modifier);
                    }
                }
            }
        }
        Ok(Some(Feedback {
            main_device: progress.main_device,
            accepted,
        }))
    }

    /// Wrap a GBM buffer as a `wl_buffer`.
    pub(crate) fn dmabuf_buffer(&self, buffer: &super::gbm::Bo) -> Option<WlBuffer> {
        let dmabuf = self.dmabuf.as_ref()?;
        let params: ZwpLinuxBufferParamsV1 = dmabuf.create_params(&self.qh, ());
        let modifier = buffer.dmabuf.modifier;
        for (index, plane) in buffer.dmabuf.planes.iter().enumerate() {
            params.add(
                plane.fd.as_fd(),
                u32::try_from(index).unwrap_or(0),
                plane.offset,
                plane.stride,
                u32::try_from(modifier >> 32).unwrap_or(0),
                u32::try_from(modifier & 0xffff_ffff).unwrap_or(0),
            );
        }
        let wl_buffer = params.create_immed(
            i32::try_from(buffer.dmabuf.width).unwrap_or(i32::MAX),
            i32::try_from(buffer.dmabuf.height).unwrap_or(i32::MAX),
            buffer.dmabuf.fourcc,
            zwp_linux_buffer_params_v1::Flags::empty(),
            &self.qh,
            (),
        );
        params.destroy();
        Some(wl_buffer)
    }

    /// A `wl_shm` buffer in a fresh memfd.
    ///
    /// # Errors
    /// memfd creation or sizing failed, or an unknown format.
    pub(crate) fn shm_buffer(
        &self,
        size: Size,
        stride: u32,
        format: u32,
    ) -> Result<ShmBuffer, CaptureError> {
        let bytes = u64::from(stride) * u64::from(size.height);
        let len = i32::try_from(bytes).map_err(|_| {
            CaptureError::protocol(self.backend, "wl_shm buffer", "buffer too large")
        })?;
        let wl_format = wl_shm::Format::try_from(format).map_err(|()| {
            CaptureError::protocol(
                self.backend,
                "wl_shm buffer",
                format!("format 0x{format:08x}"),
            )
        })?;
        let fd = rustix::fs::memfd_create("istmo-screen-capture", rustix::fs::MemfdFlags::CLOEXEC)
            .map_err(|e| {
                CaptureError::os(self.backend, "memfd_create", i64::from(e.raw_os_error()))
            })?;
        rustix::fs::ftruncate(&fd, bytes).map_err(|e| {
            CaptureError::os(self.backend, "ftruncate", i64::from(e.raw_os_error()))
        })?;
        let pool = self.shm.create_pool(fd.as_fd(), len, &self.qh, ());
        let buffer = pool.create_buffer(
            0,
            i32::try_from(size.width).unwrap_or(i32::MAX),
            i32::try_from(size.height).unwrap_or(i32::MAX),
            i32::try_from(stride).unwrap_or(i32::MAX),
            wl_format,
            &self.qh,
            (),
        );
        pool.destroy();
        Ok(ShmBuffer {
            buffer,
            fd,
            size,
            stride,
        })
    }

    #[cfg(feature = "linux-wlr-screencopy")]
    pub(crate) fn screencopy_manager(&self) -> Option<ZwlrScreencopyManagerV1> {
        self.globals.bind(&self.qh, 3..=3, ()).ok()
    }

    #[cfg(feature = "linux-ext-image-copy")]
    pub(crate) fn image_copy_manager(&self) -> Option<ExtImageCopyCaptureManagerV1> {
        self.globals.bind(&self.qh, 1..=1, ()).ok()
    }

    #[cfg(feature = "linux-ext-image-copy")]
    pub(crate) fn output_source_manager(&self) -> Option<ExtOutputImageCaptureSourceManagerV1> {
        self.globals.bind(&self.qh, 1..=1, ()).ok()
    }

    #[cfg(feature = "linux-ext-image-copy")]
    pub(crate) fn toplevel_source_manager(
        &self,
    ) -> Option<ExtForeignToplevelImageCaptureSourceManagerV1> {
        self.globals.bind(&self.qh, 1..=1, ()).ok()
    }

    #[cfg(feature = "linux-ext-image-copy")]
    /// Bind `ext_foreign_toplevel_list_v1` and collect the toplevels;
    /// `false` without the global.
    ///
    /// # Errors
    /// A protocol or socket error.
    pub(crate) fn list_toplevels(&mut self) -> Result<bool, CaptureError> {
        let Ok(list) = self
            .globals
            .bind::<ExtForeignToplevelListV1, _, _>(&self.qh, 1..=1, ())
        else {
            return Ok(false);
        };
        self.roundtrip("toplevels")?;
        self.roundtrip("toplevel details")?;
        list.stop();
        Ok(true)
    }
}

/// A `wl_shm` buffer whose content is read with `pread`.
#[derive(Debug)]
pub(crate) struct ShmBuffer {
    pub(crate) buffer: WlBuffer,
    fd: OwnedFd,
    pub(crate) size: Size,
    pub(crate) stride: u32,
}

impl ShmBuffer {
    /// Copy the pixels out (the declared `ShmTransport` copy).
    ///
    /// # Errors
    /// The read failed or came up short.
    pub(crate) fn read(&self, backend: BackendId) -> Result<Vec<u8>, CaptureError> {
        let len = usize::try_from(u64::from(self.stride) * u64::from(self.size.height))
            .map_err(|_| CaptureError::protocol(backend, "wl_shm read", "buffer too large"))?;
        let mut bytes = vec![0u8; len];
        let mut done = 0;
        while done < len {
            let n = rustix::io::pread(&self.fd, &mut bytes[done..], done as u64).map_err(|e| {
                CaptureError::os(backend, "pread wl_shm", i64::from(e.raw_os_error()))
            })?;
            if n == 0 {
                return Err(CaptureError::protocol(
                    backend,
                    "wl_shm read",
                    "short buffer",
                ));
            }
            done += n;
        }
        Ok(bytes)
    }
}

impl Drop for ShmBuffer {
    fn drop(&mut self) {
        self.buffer.destroy();
    }
}

// ----- dispatch ---------------------------------------------------------

impl Dispatch<WlRegistry, GlobalListContents> for WlState {
    fn event(
        state: &mut Self,
        _: &WlRegistry,
        event: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::GlobalRemove { name } = event {
            state.removed.push(name);
        }
    }
}

impl Dispatch<WlOutput, u32> for WlState {
    fn event(
        state: &mut Self,
        _: &WlOutput,
        event: wl_output::Event,
        global: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(out) = state.outputs.iter_mut().find(|o| o.global == *global) else {
            return;
        };
        match event {
            wl_output::Event::Geometry {
                x,
                y,
                make,
                model,
                transform,
                ..
            } => {
                out.position = (x, y);
                out.make = Some(make);
                out.model = Some(model);
                out.transform = match transform {
                    WEnum::Value(t) => t,
                    WEnum::Unknown(_) => wl_output::Transform::Normal,
                };
            }
            wl_output::Event::Mode {
                flags,
                width,
                height,
                refresh,
            } => {
                let current = match flags {
                    WEnum::Value(f) => f.contains(wl_output::Mode::Current),
                    WEnum::Unknown(bits) => bits & 1 != 0,
                };
                if current {
                    out.mode = Some((width, height));
                    out.refresh_mhz = u32::try_from(refresh).ok();
                }
            }
            wl_output::Event::Scale { factor } => out.scale = factor,
            wl_output::Event::Name { name } => out.name = Some(name),
            wl_output::Event::Description { description } => out.description = Some(description),
            _ => {}
        }
    }
}

impl Dispatch<ZxdgOutputV1, u32> for WlState {
    fn event(
        state: &mut Self,
        _: &ZxdgOutputV1,
        event: zxdg_output_v1::Event,
        global: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(out) = state.outputs.iter_mut().find(|o| o.global == *global) else {
            return;
        };
        match event {
            zxdg_output_v1::Event::LogicalPosition { x, y } => out.logical_position = Some((x, y)),
            zxdg_output_v1::Event::LogicalSize { width, height } => {
                out.logical_size = Some((width, height));
            }
            // Fills in for wl_output < 4 (no `name` there).
            zxdg_output_v1::Event::Name { name } if out.name.is_none() => out.name = Some(name),
            zxdg_output_v1::Event::Description { description } if out.description.is_none() => {
                out.description = Some(description);
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwpLinuxDmabufV1, ()> for WlState {
    fn event(
        state: &mut Self,
        _: &ZwpLinuxDmabufV1,
        event: zwp_linux_dmabuf_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // v3 lists its pairs here; v4 uses the feedback object.
        if let zwp_linux_dmabuf_v1::Event::Modifier {
            format,
            modifier_hi,
            modifier_lo,
        } = event
        {
            let modifier = (u64::from(modifier_hi) << 32) | u64::from(modifier_lo);
            let list = state.v3_modifiers.entry(format).or_default();
            if !list.contains(&modifier) {
                list.push(modifier);
            }
        }
    }
}

impl Dispatch<ZwpLinuxDmabufFeedbackV1, ()> for WlState {
    fn event(
        state: &mut Self,
        _: &ZwpLinuxDmabufFeedbackV1,
        event: zwp_linux_dmabuf_feedback_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use zwp_linux_dmabuf_feedback_v1::Event;
        let progress = &mut state.feedback;
        match event {
            Event::Done => progress.done = true,
            Event::FormatTable { fd, size } => progress.table = Some((fd, size)),
            Event::MainDevice { device } => progress.main_device = decode_dev_t(&device),
            Event::TrancheTargetDevice { device } => {
                progress.current.get_or_insert_with(Default::default).0 = decode_dev_t(&device);
            }
            Event::TrancheFormats { indices } => {
                let current = progress.current.get_or_insert_with(Default::default);
                current.1.extend(
                    indices
                        .chunks_exact(2)
                        .map(|c| u16::from_ne_bytes([c[0], c[1]])),
                );
            }
            Event::TrancheDone => {
                if let Some(tranche) = progress.current.take() {
                    progress.tranches.push(tranche);
                }
            }
            // Scanout tranche flags do not matter to a capture import.
            _ => {}
        }
    }
}

impl Dispatch<ZwlrScreencopyFrameV1, ()> for WlState {
    fn event(
        state: &mut Self,
        _: &ZwlrScreencopyFrameV1,
        event: zwlr_screencopy_frame_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use zwlr_screencopy_frame_v1::Event;
        let frame = &mut state.frame;
        match event {
            Event::Buffer {
                format,
                width,
                height,
                stride,
            } => {
                let format = match format {
                    WEnum::Value(v) => v as u32,
                    WEnum::Unknown(v) => v,
                };
                frame.shm = Some(ShmOffer {
                    format,
                    width,
                    height,
                    stride,
                });
            }
            Event::LinuxDmabuf {
                format,
                width,
                height,
            } => {
                frame.dmabuf = Some(DmabufOffer {
                    fourcc: format,
                    width,
                    height,
                });
            }
            Event::BufferDone => frame.buffer_done = true,
            Event::Flags { flags } => {
                frame.y_invert = match flags {
                    WEnum::Value(f) => f.contains(zwlr_screencopy_frame_v1::Flags::YInvert),
                    WEnum::Unknown(bits) => bits & 1 != 0,
                };
            }
            Event::Damage {
                x,
                y,
                width,
                height,
            } => {
                frame.damage.push(Rect::new(
                    i32::try_from(x).unwrap_or(i32::MAX),
                    i32::try_from(y).unwrap_or(i32::MAX),
                    width,
                    height,
                ));
            }
            Event::Ready {
                tv_sec_hi,
                tv_sec_lo,
                tv_nsec,
            } => {
                let secs = (u64::from(tv_sec_hi) << 32) | u64::from(tv_sec_lo);
                frame.pts_ns = Some(
                    secs.saturating_mul(1_000_000_000)
                        .saturating_add(u64::from(tv_nsec)),
                );
                frame.status = FrameStatus::Ready;
            }
            Event::Failed => frame.status = FrameStatus::Failed,
            _ => {}
        }
    }
}

impl Dispatch<ExtImageCopyCaptureSessionV1, ()> for WlState {
    fn event(
        state: &mut Self,
        _: &ExtImageCopyCaptureSessionV1,
        event: ext_image_copy_capture_session_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use ext_image_copy_capture_session_v1::Event;
        let pending = &mut state.pending_constraints;
        match event {
            Event::BufferSize { width, height } => pending.size = Some((width, height)),
            Event::ShmFormat { format } => {
                let code = match format {
                    WEnum::Value(v) => v as u32,
                    WEnum::Unknown(v) => v,
                };
                pending.shm_formats.push(code);
            }
            Event::DmabufDevice { device } => pending.dmabuf_device = decode_dev_t(&device),
            Event::DmabufFormat { format, modifiers } => {
                let modifiers = modifiers
                    .chunks_exact(8)
                    .filter_map(|c| <[u8; 8]>::try_from(c).ok())
                    .map(u64::from_ne_bytes)
                    .collect();
                pending.dmabuf_formats.push((format, modifiers));
            }
            Event::Done => {
                let mut batch = std::mem::take(pending);
                batch.generation = state.constraints.generation.wrapping_add(1);
                batch.stopped = state.constraints.stopped;
                state.constraints = batch;
            }
            Event::Stopped => state.constraints.stopped = true,
            _ => {}
        }
    }
}

impl Dispatch<ExtImageCopyCaptureFrameV1, ()> for WlState {
    fn event(
        state: &mut Self,
        _: &ExtImageCopyCaptureFrameV1,
        event: ext_image_copy_capture_frame_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use ext_image_copy_capture_frame_v1::Event;
        let frame = &mut state.frame;
        match event {
            Event::Transform { transform } => {
                frame.transform = Some(match transform {
                    WEnum::Value(t) => t,
                    WEnum::Unknown(_) => wl_output::Transform::Normal,
                });
            }
            Event::Damage {
                x,
                y,
                width,
                height,
            } => frame.damage.push(Rect::new(
                x,
                y,
                u32::try_from(width).unwrap_or(0),
                u32::try_from(height).unwrap_or(0),
            )),
            Event::PresentationTime {
                tv_sec_hi,
                tv_sec_lo,
                tv_nsec,
            } => {
                let secs = (u64::from(tv_sec_hi) << 32) | u64::from(tv_sec_lo);
                frame.pts_ns = Some(
                    secs.saturating_mul(1_000_000_000)
                        .saturating_add(u64::from(tv_nsec)),
                );
            }
            Event::Ready => frame.status = FrameStatus::Ready,
            Event::Failed { .. } => frame.status = FrameStatus::Failed,
            _ => {}
        }
    }
}

impl Dispatch<ExtForeignToplevelListV1, ()> for WlState {
    fn event(
        state: &mut Self,
        _: &ExtForeignToplevelListV1,
        event: ext_foreign_toplevel_list_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let ext_foreign_toplevel_list_v1::Event::Toplevel { toplevel } = event {
            state.toplevels.push(ToplevelInfo {
                handle: toplevel,
                identifier: None,
                title: None,
                app_id: None,
                closed: false,
            });
        }
    }

    event_created_child!(WlState, ExtForeignToplevelListV1, [
        ext_foreign_toplevel_list_v1::EVT_TOPLEVEL_OPCODE => (ExtForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ExtForeignToplevelHandleV1, ()> for WlState {
    fn event(
        state: &mut Self,
        handle: &ExtForeignToplevelHandleV1,
        event: ext_foreign_toplevel_handle_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(info) = state.toplevels.iter_mut().find(|t| &t.handle == handle) else {
            return;
        };
        match event {
            ext_foreign_toplevel_handle_v1::Event::Identifier { identifier } => {
                info.identifier = Some(identifier);
            }
            ext_foreign_toplevel_handle_v1::Event::Title { title } => info.title = Some(title),
            ext_foreign_toplevel_handle_v1::Event::AppId { app_id } => info.app_id = Some(app_id),
            ext_foreign_toplevel_handle_v1::Event::Closed => info.closed = true,
            _ => {}
        }
    }
}

/// Objects whose events carry nothing the backends use.
macro_rules! ignore_events {
    ($($proxy:ty),* $(,)?) => {
        $(
            impl Dispatch<$proxy, ()> for WlState {
                fn event(
                    _: &mut Self,
                    _: &$proxy,
                    _: <$proxy as Proxy>::Event,
                    (): &(),
                    _: &Connection,
                    _: &QueueHandle<Self>,
                ) {
                }
            }
        )*
    };
}

ignore_events!(
    WlShm,
    WlShmPool,
    WlBuffer,
    ZwpLinuxBufferParamsV1,
    ZxdgOutputManagerV1,
    ZwlrScreencopyManagerV1,
    ExtImageCopyCaptureManagerV1,
    ExtOutputImageCaptureSourceManagerV1,
    ExtForeignToplevelImageCaptureSourceManagerV1,
    ExtImageCaptureSourceV1,
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transforms_map_to_mirror_then_turn() {
        use wl_output::Transform;
        assert!(orientation_of(Transform::Normal, false).is_upright());
        assert_eq!(
            orientation_of(Transform::_90, false).rotation,
            Rotation::R90
        );
        let flipped = orientation_of(Transform::Flipped270, false);
        assert!(flipped.mirrored);
        assert_eq!(flipped.rotation, Rotation::R270);
        // Y_INVERT alone is a vertical flip.
        assert_eq!(
            orientation_of(Transform::Normal, true),
            Orientation::Y_INVERT
        );
        // Y_INVERT on a 180° output cancels to a horizontal mirror.
        assert_eq!(
            orientation_of(Transform::_180, true),
            Orientation {
                mirrored: true,
                rotation: Rotation::R0
            }
        );
    }

    #[test]
    fn shm_codes_round_trip() {
        for format in [
            PixelFormat::Bgra8,
            PixelFormat::Bgrx8,
            PixelFormat::Rgba8,
            PixelFormat::Xr30,
        ] {
            assert_eq!(shm_format(shm_code(format)), Some(format));
        }
    }

    #[test]
    fn dev_t_is_eight_native_bytes() {
        assert_eq!(decode_dev_t(&0xe280_u64.to_ne_bytes()), Some(0xe280));
        assert_eq!(decode_dev_t(&[1, 2, 3]), None);
    }
}
