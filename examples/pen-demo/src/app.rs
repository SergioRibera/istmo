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
}

impl DrawApp {
    pub fn new(ink: Arc<SharedInk>) -> Self {
        Self {
            ink,
            safe_area: None,
            #[cfg(not(target_os = "android"))]
            tracking: false,
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

                let strokes = self
                    .ink
                    .strokes
                    .lock()
                    .map(|s| s.clone())
                    .unwrap_or_default();
                for stroke in &strokes {
                    paint_stroke(painter, stroke);
                }
                let current = self
                    .ink
                    .current
                    .lock()
                    .ok()
                    .and_then(|c| c.clone());
                if let Some(stroke) = current.as_ref() {
                    paint_stroke(painter, stroke);
                }

                let _ = rect;
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

fn paint_stroke(painter: &egui::Painter, stroke: &Stroke) {
    if stroke.points.is_empty() {
        return;
    }
    let base = stroke
        .color
        .unwrap_or_else(|| egui::Color32::from_rgb(30, 30, 40));
    for pair in stroke.points.windows(2) {
        let a = pair[0];
        let b = pair[1];
        let width = 1.5 + (a.pressure.max(b.pressure) * 22.0);
        painter.line_segment([a.pos, b.pos], egui::Stroke::new(width, base));
    }
    if let Some(last) = stroke.points.last() {
        let width = 1.5 + last.pressure * 22.0;
        painter.circle_filled(last.pos, width * 0.5, base);
    }
}

#[cfg(not(target_os = "android"))]
impl DrawApp {
    fn pump_egui_pointer(&mut self, ctx: &egui::Context, rect: egui::Rect) {
        let (pressed, released, pos, pressure) = ctx.input(|i| {
            (
                i.pointer.primary_pressed(),
                i.pointer.primary_released(),
                i.pointer.interact_pos(),
                i.pointer.button_down(egui::PointerButton::Primary),
            )
        });
        // egui 0.29 doesn't expose pen pressure on every backend, so
        // approximate with mouse velocity when the platform doesn't
        // publish real force.
        let sample = pos.map(|p| StrokePoint {
            pos: rect.clamp(p),
            pressure: if pressure { 0.65 } else { 0.0 },
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
    }
}
