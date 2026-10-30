//! Shared egui view for `camera-demo`.
//!
//! One file, one `App` — the same code renders on native (via
//! `DesktopCamera`) and in the browser (via `WebCamera`). Both
//! backends sit behind the macro-emitted `CameraClient`, so this
//! file never names a target-specific type.

use std::sync::Arc;

use egui::{Color32, ColorImage, Context, TextureHandle, TextureOptions};
use istmo_camera::{
    AccessStatus, CameraClient, CameraClientExt, CameraControls, CameraDevice, CameraError,
    CameraFacing, CameraFrame, PixelFormat, Resolution, SessionConfig, SessionHandle,
};
use istmo_core::StreamItem;

/// Snapshot of the latest frame we've handed to egui plus a few
/// counters so the UI can show frame rate + drop rate.
#[derive(Default)]
struct FrameStats {
    frames_seen: u64,
    dropped: u64,
    last_resolution: Option<Resolution>,
}

#[allow(missing_debug_implementations)]
pub struct CameraApp {
    client: Arc<CameraClient>,
    devices: Vec<CameraDevice>,
    selected: Option<usize>,
    access: AccessStatus,
    error: Option<String>,
    session: Option<SessionHandle>,
    frames_rx: Option<istmo_core::TypedStream<CameraFrame>>,
    texture: Option<TextureHandle>,
    stats: FrameStats,
    zoom: f32,
    torch: bool,
}

impl CameraApp {
    #[must_use]
    pub fn new(client: Arc<CameraClient>) -> Self {
        Self {
            client,
            devices: Vec::new(),
            selected: None,
            access: AccessStatus::NotDetermined,
            error: None,
            session: None,
            frames_rx: None,
            texture: None,
            stats: FrameStats::default(),
            zoom: 1.0,
            torch: false,
        }
    }

    fn refresh_devices(&mut self) {
        match pollster::block_on(self.client.devices()).map_err(CameraError::from) {
            Ok(devices) => {
                if self.selected.is_none() && !devices.is_empty() {
                    self.selected = Some(0);
                }
                self.devices = devices;
                self.error = None;
            }
            Err(err) => self.error = Some(render_err(&err)),
        }
    }

    fn refresh_access(&mut self) {
        match pollster::block_on(self.client.access_status()).map_err(CameraError::from) {
            Ok(status) => {
                self.access = status;
            }
            Err(err) => self.error = Some(render_err(&err)),
        }
    }

    fn request_access(&mut self) {
        match pollster::block_on(self.client.request_access()).map_err(CameraError::from) {
            Ok(status) => {
                self.access = status;
                if matches!(status, AccessStatus::Granted) {
                    self.refresh_devices();
                }
            }
            Err(err) => self.error = Some(render_err(&err)),
        }
    }

    fn open_selected(&mut self) {
        let Some(idx) = self.selected else { return };
        let Some(dev) = self.devices.get(idx) else {
            return;
        };
        let cfg = SessionConfig::new(&dev.id).with_pixel_format(PixelFormat::Rgba8);

        match pollster::block_on(self.client.open_session(cfg)) {
            Ok(session) => match session.frames() {
                Ok(rx) => {
                    self.frames_rx = Some(rx);
                    self.stats = FrameStats::default();
                    self.session = Some(session);
                    self.error = None;
                }
                Err(err) => {
                    self.error = Some(render_err(&err));
                    let _ = pollster::block_on(session.close());
                }
            },
            Err(err) => self.error = Some(render_err(&err)),
        }
    }

    fn close_session(&mut self) {
        self.frames_rx = None;
        if let Some(handle) = self.session.take() {
            let _ = pollster::block_on(handle.close());
        }
        self.texture = None;
    }

    fn apply_zoom(&mut self) {
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let controls = CameraControls {
            zoom: Some(self.zoom),
            ..Default::default()
        };
        if let Err(err) = pollster::block_on(session.apply_controls(controls)) {
            self.error = Some(render_err(&err));
        }
    }

    fn apply_torch(&mut self) {
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let controls = CameraControls {
            torch: Some(self.torch),
            ..Default::default()
        };
        if let Err(err) = pollster::block_on(session.apply_controls(controls)) {
            self.error = Some(render_err(&err));
        }
    }

    fn drain_frames(&mut self, ctx: &Context) {
        let Some(rx) = self.frames_rx.as_ref() else {
            return;
        };
        let mut latest: Option<CameraFrame> = None;
        while let Some(item) = rx.try_recv() {
            match item {
                Ok(StreamItem::Event(frame)) => {
                    self.stats.frames_seen += 1;
                    self.stats.dropped += u64::from(frame.dropped_before);
                    latest = Some(frame);
                }
                Ok(StreamItem::Completed | StreamItem::Cancelled) => {
                    self.frames_rx = None;
                    break;
                }
                Ok(StreamItem::Failed(())) => {
                    self.error = Some("frame stream failed".into());
                    self.frames_rx = None;
                    break;
                }
                Err(err) => {
                    self.error = Some(err.to_string());
                    self.frames_rx = None;
                    break;
                }
            }
        }

        if let Some(frame) = latest {
            self.stats.last_resolution = Some(frame.resolution);
            update_texture(ctx, &mut self.texture, &frame);
        }
    }
}

impl eframe::App for CameraApp {
    fn update(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        self.drain_frames(ctx);

        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("istmo-camera demo");
                ui.separator();
                ui.label(format!("access: {:?}", self.access));
                if ui.button("recheck").clicked() {
                    self.refresh_access();
                }
                if matches!(
                    self.access,
                    AccessStatus::NotDetermined | AccessStatus::Denied
                ) && ui.button("request").clicked()
                {
                    self.request_access();
                }
                if ui.button("rescan devices").clicked() {
                    self.refresh_devices();
                }
            });
        });

        egui::SidePanel::left("left")
            .default_width(260.0)
            .show(ctx, |ui| {
                ui.label("Devices");
                ui.separator();
                for (idx, dev) in self.devices.iter().enumerate() {
                    let selected = self.selected == Some(idx);
                    let label = format!("{} ({:?})", truncate(&dev.label, 28), dev.facing);
                    if ui.selectable_label(selected, label).clicked() {
                        self.selected = Some(idx);
                    }
                }
                ui.separator();

                let has_session = self.session.is_some();
                ui.horizontal(|ui| {
                    if !has_session && ui.button("open").clicked() {
                        self.open_selected();
                    }
                    if has_session && ui.button("close").clicked() {
                        self.close_session();
                    }
                });

                if has_session {
                    ui.separator();
                    ui.label("Controls");
                    if ui
                        .add(egui::Slider::new(&mut self.zoom, 0.5..=5.0).text("zoom"))
                        .changed()
                    {
                        self.apply_zoom();
                    }
                    if ui.checkbox(&mut self.torch, "torch").changed() {
                        self.apply_torch();
                    }
                    ui.separator();
                    ui.label(format!("frames: {}", self.stats.frames_seen));
                    ui.label(format!("dropped: {}", self.stats.dropped));
                    if let Some(res) = self.stats.last_resolution {
                        ui.label(format!("resolution: {}x{}", res.width, res.height));
                    }
                }
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            if let Some(err) = &self.error {
                ui.colored_label(Color32::RED, err);
                ui.separator();
            }
            match &self.texture {
                Some(tex) => {
                    let max = ui.available_size();
                    let scale = {
                        let [w, h] = tex.size().map(|v| v as f32);
                        (max.x / w).min(max.y / h)
                    };
                    let size = tex.size_vec2() * scale;
                    ui.image((tex.id(), size));
                }
                None => {
                    ui.vertical_centered(|ui| {
                        ui.add_space(80.0);
                        ui.heading("no preview");
                        ui.label("Grant camera access, pick a device, then press `open`.");
                    });
                }
            }
        });

        ctx.request_repaint();
    }
}

/// Lifted so it can be exercised both from a bg thread (native) and
/// inline inside `update` (wasm).
fn update_texture(ctx: &Context, slot: &mut Option<TextureHandle>, frame: &CameraFrame) {
    let width = frame.resolution.width as usize;
    let height = frame.resolution.height as usize;
    if frame.bytes.len() < width * height * 4 {
        return;
    }
    let image = ColorImage::from_rgba_unmultiplied([width, height], &frame.bytes);
    match slot.as_mut() {
        Some(handle) => handle.set(image, TextureOptions::LINEAR),
        None => {
            *slot = Some(ctx.load_texture("camera-preview", image, TextureOptions::LINEAR));
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.into()
    } else {
        let mut out: String = s.chars().take(max).collect();
        out.push('…');
        out
    }
}

fn render_err(err: &CameraError) -> String {
    err.to_string()
}

// Allow the lint for `#[non_exhaustive]`-style facings when they
// reach `format!("{:?}")`.
const _: fn(CameraFacing) = |_| {};
