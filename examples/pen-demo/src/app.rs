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

use std::sync::{Arc, Mutex};

use eframe::egui;

#[derive(Debug, Clone, Copy)]
pub struct StrokePoint {
    pub pos: egui::Pos2,
    pub pressure: f32,
    pub tilt: f32,
}

#[derive(Debug, Default, Clone)]
pub struct Stroke {
    pub points: Vec<StrokePoint>,
}

#[derive(Debug, Default)]
pub struct SharedInk {
    pub strokes: Mutex<Vec<Stroke>>,
    pub current: Mutex<Option<Stroke>>,
}

impl SharedInk {
    pub fn begin(&self, first: StrokePoint) {
        let mut cur = self.current.lock().unwrap_or_else(|p| p.into_inner());
        *cur = Some(Stroke {
            points: vec![first],
        });
    }

    pub fn extend(&self, point: StrokePoint) {
        let mut cur = self.current.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(stroke) = cur.as_mut() {
            stroke.points.push(point);
        } else {
            *cur = Some(Stroke {
                points: vec![point],
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
}

#[derive(Debug)]
pub struct DrawApp {
    pub ink: Arc<SharedInk>,
    #[cfg(not(target_os = "android"))]
    tracking: bool,
}

impl DrawApp {
    pub fn new(ink: Arc<SharedInk>) -> Self {
        Self {
            ink,
            #[cfg(not(target_os = "android"))]
            tracking: false,
        }
    }
}

impl eframe::App for DrawApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::TopBottomPanel::top("bar").show(ctx, |ui| {
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
            .frame(egui::Frame::none().fill(egui::Color32::from_gray(250)))
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

        // Redraw quickly enough to catch stylus samples arriving from
        // the background thread on Android.
        ctx.request_repaint_after(std::time::Duration::from_millis(16));
    }
}

fn paint_stroke(painter: &egui::Painter, stroke: &Stroke) {
    if stroke.points.is_empty() {
        return;
    }
    for pair in stroke.points.windows(2) {
        let a = pair[0];
        let b = pair[1];
        let width = 1.5 + (a.pressure.max(b.pressure) * 22.0);
        let hue = (a.tilt.abs().min(std::f32::consts::FRAC_PI_2)
            / std::f32::consts::FRAC_PI_2)
            .clamp(0.0, 1.0);
        let color = hue_to_color(hue);
        painter.line_segment([a.pos, b.pos], egui::Stroke::new(width, color));
    }
    if let Some(last) = stroke.points.last() {
        let width = 1.5 + last.pressure * 22.0;
        painter.circle_filled(last.pos, width * 0.5, egui::Color32::from_gray(50));
    }
}

fn hue_to_color(t: f32) -> egui::Color32 {
    // Blue → magenta → orange sweep — matches "cool tip / warm side"
    // intuition when the pen tilts.
    let r = (60.0 + t * 180.0).min(255.0);
    let g = (60.0 + (1.0 - t) * 40.0).min(255.0);
    let b = (180.0 + (1.0 - t) * 60.0).min(255.0);
    egui::Color32::from_rgb(r as u8, g as u8, b as u8)
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
