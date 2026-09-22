//! Shared eframe drawing app for the pen-demo.
//!
//! `DrawApp` maintains a list of strokes rendered on a full-viewport
//! canvas. Stroke points carry pressure + tilt so each rendered segment
//! is drawn as a series of tapered circles — width tracks pressure,
//! hue tracks tilt magnitude.
//!
//! Input source is platform-dependent:
//! - **Android**: a background thread drains `PenClient.events()` (the
//!   istmo wire) and pushes samples into the shared stroke buffer.
//! - **Desktop**: the same buffer is populated inside `update()` from
//!   `egui::InputState.pointer`, which surfaces mouse + wintab
//!   pressure via `PointerState::pressure`.
//!
//! Both paths feed the same rendering code so the canvas looks
//! identical across targets.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use eframe::egui;
use istmo::plugins::SafeArea;

#[derive(Debug, Clone, Copy)]
pub struct StrokePoint {
    pub pos: egui::Pos2,
    pub pressure: f32,
    pub tilt: f32,
}

#[derive(Debug, Default, Clone)]
pub struct Stroke {
    pub points: Vec<StrokePoint>,
    pub color: Option<egui::Color32>,
}

/// Live snapshot of the last stylus sample — populated by the pen
/// wire pump and rendered by the on-screen debug overlay.
#[derive(Debug, Default, Clone)]
pub struct PenDebug {
    pub x: f32,
    pub y: f32,
    pub pressure: f32,
    pub tilt_x: f32,
    pub tilt_y: f32,
    pub azimuth: f32,
    pub altitude: f32,
    pub twist: f32,
    pub tangential_pressure: f32,
    pub z_offset: f32,
    pub timestamp_us: u64,
    pub sequence: u32,
    pub tool_id: u32,
    pub tool_kind: &'static str,
    pub buttons: u32,
    pub last_event: &'static str,
    pub touches: u64,
}

#[derive(Debug, Default)]
pub struct SharedInk {
    pub strokes: Mutex<Vec<Stroke>>,
    pub current: Mutex<Option<Stroke>>,
    pub debug: Mutex<PenDebug>,
    pub color: Mutex<egui::Color32>,
    pub color_menu_open: AtomicBool,
}

impl SharedInk {
    pub fn begin(&self, first: StrokePoint) {
        let color = self.current_color();
        let mut cur = self.current.lock().unwrap_or_else(|p| p.into_inner());
        *cur = Some(Stroke {
            points: vec![first],
            color: Some(color),
        });
    }

    pub fn extend(&self, point: StrokePoint) {
        let color = self.current_color();
        let mut cur = self.current.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(stroke) = cur.as_mut() {
            stroke.points.push(point);
        } else {
            *cur = Some(Stroke {
                points: vec![point],
                color: Some(color),
            });
        }
    }

    pub fn end(&self) {
        let mut cur = self.current.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(stroke) = cur.take() {
            let mut strokes = self.strokes.lock().unwrap_or_else(|p| p.into_inner());
            strokes.push(stroke);
        }
    }

    pub fn clear(&self) {
        let mut cur = self.current.lock().unwrap_or_else(|p| p.into_inner());
        cur.take();
        let mut strokes = self.strokes.lock().unwrap_or_else(|p| p.into_inner());
        strokes.clear();
    }

    pub fn update_debug(&self, next: PenDebug) {
        let mut d = self.debug.lock().unwrap_or_else(|p| p.into_inner());
        let touches = d.touches;
        *d = PenDebug {
            touches: touches.saturating_add(1),
            ..next
        };
    }

    pub fn current_color(&self) -> egui::Color32 {
        let guard = self.color.lock().unwrap_or_else(|p| p.into_inner());
        if guard.a() == 0 {
            egui::Color32::from_rgb(30, 30, 40)
        } else {
            *guard
        }
    }

    pub fn set_color(&self, color: egui::Color32) {
        let mut guard = self.color.lock().unwrap_or_else(|p| p.into_inner());
        *guard = color;
    }

    pub fn toggle_color_menu(&self) {
        let prev = self.color_menu_open.load(Ordering::Relaxed);
        self.color_menu_open.store(!prev, Ordering::Relaxed);
    }

    pub fn set_color_menu(&self, open: bool) {
        self.color_menu_open.store(open, Ordering::Relaxed);
    }

    pub fn color_menu_open(&self) -> bool {
        self.color_menu_open.load(Ordering::Relaxed)
    }
}

#[derive(Debug)]
pub struct DrawApp {
    pub ink: Arc<SharedInk>,
    safe_area: Option<SafeArea>,
    #[cfg(not(target_os = "android"))]
    tracking: bool,
    #[cfg(not(target_os = "android"))]
    publisher: Option<Arc<istmo_pen::publisher::PenPublisher>>,
    #[cfg(not(target_os = "android"))]
    seq: std::sync::atomic::AtomicU32,
    #[cfg(not(target_os = "android"))]
    attach: std::time::Instant,
    /// Last accepted pointer position while `tracking == true`. Used to
    /// synthesise coalesced sub-samples between the previous frame's
    /// pointer and this one — mouse polling gives us one sample per
    /// eframe repaint (~60 Hz), which leaves visible gaps on fast
    /// strokes. Rebuilding a per-pixel sub-sample train shortens
    /// segments enough that the ribbon painter has clean data to
    /// tessellate.
    #[cfg(not(target_os = "android"))]
    last_pointer: Option<egui::Pos2>,
}

impl DrawApp {
    pub fn new(ink: Arc<SharedInk>) -> Self {
        Self {
            ink,
            safe_area: None,
            #[cfg(not(target_os = "android"))]
            tracking: false,
            #[cfg(not(target_os = "android"))]
            publisher: None,
            #[cfg(not(target_os = "android"))]
            seq: std::sync::atomic::AtomicU32::new(0),
            #[cfg(not(target_os = "android"))]
            attach: std::time::Instant::now(),
            #[cfg(not(target_os = "android"))]
            last_pointer: None,
        }
    }

    /// Desktop-only ctor that also stores an [`Arc<PenPublisher>`] so
    /// `pump_egui_pointer` forwards mouse/pen events through the plugin
    /// wire — the same code path a real stylus would exercise on
    /// Windows / macOS / Linux (libinput).
    #[cfg(not(target_os = "android"))]
    pub fn new_desktop(
        ink: Arc<SharedInk>,
        publisher: Arc<istmo_pen::publisher::PenPublisher>,
    ) -> Self {
        Self {
            ink,
            safe_area: None,
            tracking: false,
            publisher: Some(publisher),
            seq: std::sync::atomic::AtomicU32::new(0),
            attach: std::time::Instant::now(),
            last_pointer: None,
        }
    }

    fn ensure_safe_area(&mut self, ctx: &egui::Context) {
        if self.safe_area.is_some() {
            return;
        }
        match SafeArea::acquire() {
            Ok(sa) => {
                let stream = sa.stream();
                let ctx_clone = ctx.clone();
                std::thread::spawn(move || {
                    while stream.recv().is_ok() {
                        ctx_clone.request_repaint();
                    }
                });
                self.safe_area = Some(sa);
            }
            Err(err) => {
                log::debug!("safe_area not ready: {err}");
            }
        }
    }

    fn current_insets(&self) -> (f32, f32, f32, f32) {
        let Some(sa) = self.safe_area.as_ref() else {
            return (0.0, 0.0, 0.0, 0.0);
        };
        let insets = sa.current_or_zero();
        let top = insets
            .system_bars
            .top
            .max(insets.display_cutout.top);
        let right = insets
            .system_bars
            .right
            .max(insets.display_cutout.right);
        let bottom = insets
            .system_bars
            .bottom
            .max(insets.display_cutout.bottom)
            .max(insets.ime.bottom);
        let left = insets
            .system_bars
            .left
            .max(insets.display_cutout.left);
        (top, right, bottom, left)
    }
}

impl eframe::App for DrawApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.ensure_safe_area(ctx);
        let (top, right, bottom, left) = self.current_insets();

        egui::TopBottomPanel::top("bar")
            .frame(
                egui::Frame::none()
                    .inner_margin(egui::Margin {
                        top,
                        right,
                        bottom: 6.0,
                        left,
                    })
                    .fill(egui::Color32::from_gray(235)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.heading("istmo-pen demo");
                    ui.separator();
                    ui.label("Draw with a stylus — width tracks pressure, hue tracks tilt.");
                    ui.separator();
                    if ui.button("Clear").clicked() {
                        self.ink.clear();
                    }
                });
            });

        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(egui::Color32::from_gray(250))
                    .inner_margin(egui::Margin {
                        top: 0.0,
                        right,
                        bottom,
                        left,
                    }),
            )
            .show(ctx, |ui| {
                let painter = ui.painter();
                let rect = ui.max_rect();

                #[cfg(not(target_os = "android"))]
                self.pump_egui_pointer(ctx, rect);

                // Sample coords are canvas-local on desktop (subtracted
                // in `pump_egui_pointer`), fullscreen-relative on Android
                // (PenCaptureView doesn't know Rust's rect). Paint accordingly.
                #[cfg(not(target_os = "android"))]
                let paint_offset = rect.min.to_vec2();
                #[cfg(target_os = "android")]
                let paint_offset = egui::Vec2::ZERO;

                let strokes = self
                    .ink
                    .strokes
                    .lock()
                    .map(|s| s.clone())
                    .unwrap_or_default();
                for stroke in &strokes {
                    paint_stroke(painter, stroke, paint_offset);
                }
                let current = self
                    .ink
                    .current
                    .lock()
                    .ok()
                    .and_then(|c| c.clone());
                if let Some(stroke) = current.as_ref() {
                    paint_stroke(painter, stroke, paint_offset);
                }
            });

        self.show_debug_overlay(ctx, top, left);
        self.show_color_menu(ctx);

        // Redraw quickly enough to catch stylus samples arriving from
        // the background thread on Android.
        ctx.request_repaint_after(std::time::Duration::from_millis(16));
    }
}

impl DrawApp {
    fn show_debug_overlay(&self, ctx: &egui::Context, top_inset: f32, left_inset: f32) {
        let debug = self
            .ink
            .debug
            .lock()
            .map(|d| d.clone())
            .unwrap_or_default();
        egui::Window::new("pen debug")
            .anchor(
                egui::Align2::LEFT_TOP,
                egui::vec2(left_inset + 8.0, top_inset + 48.0),
            )
            .collapsible(true)
            .resizable(false)
            .default_open(true)
            .show(ctx, |ui| {
                ui.monospace(format!(
                    "last event   {:>15}\n\
                     touches      {:>15}\n\
                     x            {:>15.2}\n\
                     y            {:>15.2}\n\
                     pressure     {:>15.3}\n\
                     tilt_x       {:>15.3}\n\
                     tilt_y       {:>15.3}\n\
                     azimuth      {:>15.3}\n\
                     altitude     {:>15.3}\n\
                     twist        {:>15.3}\n\
                     tangential   {:>15.3}\n\
                     z_offset     {:>15.3}\n\
                     tool         {:>15}\n\
                     tool_id      {:>15}\n\
                     buttons      {:>15}\n\
                     seq          {:>15}\n\
                     time_us      {:>15}",
                    debug.last_event,
                    debug.touches,
                    debug.x,
                    debug.y,
                    debug.pressure,
                    debug.tilt_x,
                    debug.tilt_y,
                    debug.azimuth,
                    debug.altitude,
                    debug.twist,
                    debug.tangential_pressure,
                    debug.z_offset,
                    debug.tool_kind,
                    debug.tool_id,
                    format!("{:08b}", debug.buttons),
                    debug.sequence,
                    debug.timestamp_us,
                ));
                let color = self.ink.current_color();
                ui.horizontal(|ui| {
                    ui.label("brush");
                    let (rect, _) = ui.allocate_exact_size(
                        egui::vec2(24.0, 16.0),
                        egui::Sense::hover(),
                    );
                    ui.painter().rect_filled(rect, 3.0, color);
                });
            });
    }

    fn show_color_menu(&self, ctx: &egui::Context) {
        if !self.ink.color_menu_open() {
            return;
        }
        egui::Window::new("brush colors")
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label("Press pen barrel button again to close.");
                ui.add_space(4.0);
                let palette = [
                    egui::Color32::from_rgb(30, 30, 40),
                    egui::Color32::from_rgb(200, 40, 40),
                    egui::Color32::from_rgb(220, 100, 20),
                    egui::Color32::from_rgb(220, 190, 0),
                    egui::Color32::from_rgb(80, 170, 60),
                    egui::Color32::from_rgb(30, 130, 200),
                    egui::Color32::from_rgb(120, 60, 200),
                    egui::Color32::from_rgb(230, 120, 200),
                ];
                let current = self.ink.current_color();
                egui::Grid::new("palette").spacing(egui::vec2(6.0, 6.0)).show(ui, |ui| {
                    for (i, color) in palette.iter().enumerate() {
                        let selected = current == *color;
                        let (rect, response) = ui.allocate_exact_size(
                            egui::vec2(40.0, 40.0),
                            egui::Sense::click(),
                        );
                        ui.painter().rect_filled(rect, 6.0, *color);
                        if selected {
                            ui.painter().rect_stroke(
                                rect,
                                6.0,
                                egui::Stroke::new(3.0, egui::Color32::BLACK),
                            );
                        }
                        if response.clicked() {
                            self.ink.set_color(*color);
                            self.ink.set_color_menu(false);
                        }
                        if (i + 1) % 4 == 0 {
                            ui.end_row();
                        }
                    }
                });
                if ui.button("Close").clicked() {
                    self.ink.set_color_menu(false);
                }
            });
    }
}

fn paint_stroke(painter: &egui::Painter, stroke: &Stroke, offset: egui::Vec2) {
    let raw = &stroke.points;
    if raw.is_empty() {
        return;
    }
    let base = stroke
        .color
        .unwrap_or_else(|| egui::Color32::from_rgb(30, 30, 40));
    let width_at = |p: &StrokePoint| 1.5 + p.pressure * 22.0;

    // Single-point stroke: just a dot.
    if raw.len() == 1 {
        painter.circle_filled(
            raw[0].pos + offset,
            width_at(&raw[0]) * 0.5,
            base,
        );
        return;
    }

    // Chaikin subdivision (2 iterations) smooths the polyline so the
    // ribbon tessellator gets a curve close to a Catmull-Rom / cubic
    // spline without needing per-segment shape objects. Pressure /
    // tilt travel with the interpolated points so width evolves
    // continuously along the ribbon.
    let mut pts: Vec<StrokePoint> = raw
        .iter()
        .map(|p| StrokePoint {
            pos: p.pos + offset,
            pressure: p.pressure,
            tilt: p.tilt,
        })
        .collect();
    for _ in 0..2 {
        pts = chaikin_smooth(&pts);
    }

    // Drop degenerate (near-zero-length) segments after smoothing so the
    // miter math doesn't divide by ~0 direction vectors.
    pts.dedup_by(|a, b| (a.pos - b.pos).length_sq() < 0.01);
    if pts.len() < 2 {
        painter.circle_filled(pts[0].pos, width_at(&pts[0]) * 0.5, base);
        return;
    }

    // Ribbon tessellation. For each point compute a perpendicular
    // miter direction (the average of the incoming/outgoing normals)
    // extended so the ribbon edges meet cleanly on both sides. Clamp
    // the miter length at sharp corners to avoid spikes.
    let n = pts.len();
    let mut mesh = egui::epaint::Mesh::default();
    mesh.vertices.reserve(n * 2);
    mesh.indices.reserve((n - 1) * 6);
    for i in 0..n {
        let dir_in = if i > 0 {
            (pts[i].pos - pts[i - 1].pos).normalized()
        } else {
            (pts[i + 1].pos - pts[i].pos).normalized()
        };
        let dir_out = if i + 1 < n {
            (pts[i + 1].pos - pts[i].pos).normalized()
        } else {
            dir_in
        };
        let miter = (dir_in + dir_out).normalized();
        let mut normal = egui::Vec2::new(-miter.y, miter.x);
        let perp_out = egui::Vec2::new(-dir_out.y, dir_out.x);
        let scale = normal.dot(perp_out).abs().max(0.35);
        normal /= scale;
        let half = width_at(&pts[i]) * 0.5;
        let left = pts[i].pos + normal * half;
        let right = pts[i].pos - normal * half;
        mesh.vertices.push(egui::epaint::Vertex {
            pos: left,
            uv: egui::epaint::WHITE_UV,
            color: base,
        });
        mesh.vertices.push(egui::epaint::Vertex {
            pos: right,
            uv: egui::epaint::WHITE_UV,
            color: base,
        });
    }
    for i in 0..n - 1 {
        let l0 = (i * 2) as u32;
        let r0 = l0 + 1;
        let l1 = l0 + 2;
        let r1 = l0 + 3;
        mesh.indices.extend_from_slice(&[l0, r0, l1, r0, r1, l1]);
    }
    painter.add(egui::Shape::mesh(mesh));

    // Round caps at both ends smooth the otherwise flat perpendicular
    // termination of the ribbon.
    painter.circle_filled(pts[0].pos, width_at(&pts[0]) * 0.5, base);
    painter.circle_filled(
        pts[n - 1].pos,
        width_at(&pts[n - 1]) * 0.5,
        base,
    );
}

fn chaikin_smooth(pts: &[StrokePoint]) -> Vec<StrokePoint> {
    if pts.len() < 3 {
        return pts.to_vec();
    }
    let mut out = Vec::with_capacity(pts.len() * 2);
    out.push(pts[0]);
    for pair in pts.windows(2) {
        let a = &pair[0];
        let b = &pair[1];
        let q_pos = egui::Pos2::new(
            a.pos.x * 0.75 + b.pos.x * 0.25,
            a.pos.y * 0.75 + b.pos.y * 0.25,
        );
        let r_pos = egui::Pos2::new(
            a.pos.x * 0.25 + b.pos.x * 0.75,
            a.pos.y * 0.25 + b.pos.y * 0.75,
        );
        out.push(StrokePoint {
            pos: q_pos,
            pressure: a.pressure * 0.75 + b.pressure * 0.25,
            tilt: a.tilt * 0.75 + b.tilt * 0.25,
        });
        out.push(StrokePoint {
            pos: r_pos,
            pressure: a.pressure * 0.25 + b.pressure * 0.75,
            tilt: a.tilt * 0.25 + b.tilt * 0.75,
        });
    }
    out.push(pts[pts.len() - 1]);
    out
}

#[cfg(not(target_os = "android"))]
impl DrawApp {
    fn pump_egui_pointer(&mut self, ctx: &egui::Context, rect: egui::Rect) {
        use istmo_pen::{PenEvent, PenMove};

        let (pressed, released, pos, held) = ctx.input(|i| {
            (
                i.pointer.primary_pressed(),
                i.pointer.primary_released(),
                i.pointer.interact_pos(),
                i.pointer.button_down(egui::PointerButton::Primary),
            )
        });

        let Some(publisher) = self.publisher.as_ref() else {
            // Legacy path: no publisher wired (tests / other embedders).
            // Fall back to direct ink mutation so the canvas still works.
            let sample = pos.map(|p| StrokePoint {
                pos: rect.clamp(p),
                pressure: if held { 0.65 } else { 0.0 },
                tilt: 0.0,
            });
            if pressed {
                if let Some(p) = sample {
                    self.ink.begin(p);
                    self.tracking = true;
                }
            } else if released {
                if self.tracking {
                    self.ink.end();
                    self.tracking = false;
                }
            } else if self.tracking {
                if let Some(p) = sample {
                    self.ink.extend(p);
                }
            }
            return;
        };

        let Some(raw) = pos else {
            return;
        };
        // Only start tracking when the press-down lands inside the drawing
        // canvas rect. Top-bar clicks, resize handles, and out-of-bounds
        // pointer drags shouldn't seed a stroke.
        if pressed && !rect.contains(raw) {
            return;
        }
        let clamped = rect.clamp(raw);
        // Canvas-local coordinates: (0, 0) at the CentralPanel top-left so
        // the sample matches the pointer regardless of top-bar height /
        // safe-area insets. Painting adds `rect.min` back through
        // `paint_offset()`.
        let local = egui::Pos2::new(clamped.x - rect.min.x, clamped.y - rect.min.y);
        let pressure = if held { 0.65 } else { 0.0 };
        let sample = self.build_sample(local, pressure);
        if pressed {
            publisher.push_event(crate::WINDOW_ID, PenEvent::Down(sample));
            self.tracking = true;
            self.last_pointer = Some(local);
        } else if released && self.tracking {
            publisher.push_event(crate::WINDOW_ID, PenEvent::Up(sample));
            self.tracking = false;
            self.last_pointer = None;
        } else if self.tracking {
            // Synthesise coalesced sub-samples along the segment
            // `last_pointer → local` — one per ~4 logical points, capped
            // to keep the wire cost bounded on very fast pointer moves.
            // Interpolated pressure sits between the previous and current
            // frame so the ribbon width evolves smoothly across the
            // segment instead of stepping at each frame boundary.
            let coalesced = if let Some(prev) = self.last_pointer {
                let delta = local - prev;
                let dist = delta.length();
                let steps = (dist / 4.0).ceil().clamp(0.0, 8.0) as usize;
                let mut out = Vec::with_capacity(steps);
                for step in 1..steps {
                    let t = step as f32 / steps as f32;
                    let sub_pos = egui::Pos2::new(prev.x + delta.x * t, prev.y + delta.y * t);
                    // Constant pressure per stroke on desktop mouse — no
                    // per-sub-sample interpolation needed; wintab / real
                    // stylus paths already carry real pressure.
                    out.push(self.build_sample(sub_pos, pressure));
                }
                out
            } else {
                Vec::new()
            };
            publisher.push_event(
                crate::WINDOW_ID,
                PenEvent::Move(PenMove {
                    sample,
                    coalesced,
                    predicted: Vec::new(),
                }),
            );
            self.last_pointer = Some(local);
        }
    }

    fn build_sample(&self, pos: egui::Pos2, pressure: f32) -> istmo_pen::PenSample {
        use istmo_pen::{PenSample, PenToolKind};
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        let timestamp_us = self.attach.elapsed().as_micros() as u64;
        PenSample {
            x: pos.x,
            y: pos.y,
            pressure,
            tilt_x: 0.0,
            tilt_y: 0.0,
            azimuth: 0.0,
            altitude: 0.0,
            twist: 0.0,
            tangential_pressure: 0.0,
            z_offset: 0.0,
            timestamp_us,
            sequence: seq,
            tool_id: 1,
            tool_kind: PenToolKind::Tip,
            buttons: if pressure > 0.0 { 1 } else { 0 },
        }
    }
}
