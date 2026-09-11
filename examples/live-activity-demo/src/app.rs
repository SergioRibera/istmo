//! Shared egui UI.
//!
//! Runs on desktop (against the emulator), Android (against
//! `LiveActivityBackendImpl.kt` + tiered notification renderers) and iOS
//! (against `LiveActivityBackendImpl.swift` + ActivityKit). Zero
//! `#[cfg(target_os = ...)]` in the eframe app impl.

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui;
use istmo_core::{IstmoError, NativeHandle, NativeHandleId, Runtime};
use istmo_live_activity::{
    ActivityStyle, AlertConfig, AlertSound, AndroidTierHint, DismissalPolicy, LiveActivityToken,
    PlatformCapabilities, TypedLiveActivity,
};
use istmo_macros::message;

/// Wire identifier that keys this timer's handler on the native side.
pub const ACTIVITY_TYPE: &str = "timer";

#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimerAttributes {
    pub title: String,
    pub target_seconds: u32,
}

#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimerState {
    pub elapsed_seconds: u32,
    pub label: String,
}

/// Convenience constructor used by both desktop and mobile entry points.
pub fn activities(
    runtime: &Arc<Runtime>,
) -> Result<TypedLiveActivity<TimerAttributes, TimerState>, IstmoError> {
    TypedLiveActivity::<TimerAttributes, TimerState>::new(runtime, ACTIVITY_TYPE)
}

/// One line in the UI transcript panel.
#[derive(Debug, Clone)]
pub struct LogLine {
    pub op: String,
    pub outcome: String,
}

#[derive(Debug)]
pub struct SharedState {
    activities: TypedLiveActivity<TimerAttributes, TimerState>,
    egui_ctx: egui::Context,
    transcript: Mutex<Vec<LogLine>>,
    active_handle: Mutex<Option<NativeHandle<LiveActivityToken>>>,
    capabilities: Mutex<Option<PlatformCapabilities>>,
    timer_start: Mutex<Option<Instant>>,
}

impl SharedState {
    pub fn new(
        activities: TypedLiveActivity<TimerAttributes, TimerState>,
        egui_ctx: egui::Context,
    ) -> Self {
        Self {
            activities,
            egui_ctx,
            transcript: Mutex::new(Vec::new()),
            active_handle: Mutex::new(None),
            capabilities: Mutex::new(None),
            timer_start: Mutex::new(None),
        }
    }

    fn push_log(&self, op: impl Into<String>, outcome: impl Into<String>) {
        let mut lines = self.transcript.lock().expect("transcript mutex");
        lines.push(LogLine { op: op.into(), outcome: outcome.into() });
        self.egui_ctx.request_repaint();
    }

    fn set_handle(&self, handle: Option<NativeHandle<LiveActivityToken>>) {
        *self.active_handle.lock().expect("handle mutex") = handle;
        self.egui_ctx.request_repaint();
    }

    fn set_capabilities(&self, caps: PlatformCapabilities) {
        *self.capabilities.lock().expect("capabilities mutex") = Some(caps);
        self.egui_ctx.request_repaint();
    }

    fn set_timer_start(&self, now: Option<Instant>) {
        *self.timer_start.lock().expect("timer mutex") = now;
    }

    fn active_handle_id(&self) -> Option<NativeHandleId> {
        self.active_handle
            .lock()
            .expect("handle mutex")
            .as_ref()
            .map(|h| h.id())
    }
}

#[derive(Debug)]
pub struct DemoApp {
    shared: Arc<SharedState>,
    title_input: String,
    target_input: String,
    last_tick: Instant,
}

impl DemoApp {
    pub fn new(shared: Arc<SharedState>) -> Self {
        // Kick off capability probe on boot so the UI can show tier info.
        let boot = Arc::clone(&shared);
        thread::spawn(move || match pollster::block_on(boot.activities.capabilities()) {
            Ok(caps) => boot.set_capabilities(caps),
            Err(err) => boot.push_log("capabilities", format!("error: {err}")),
        });
        Self {
            shared,
            title_input: "Focus block".to_owned(),
            target_input: "25".to_owned(),
            last_tick: Instant::now(),
        }
    }

    fn on_start(&self) {
        let title = self.title_input.trim().to_owned();
        let target: u32 = self.target_input.trim().parse().unwrap_or(25);
        let attrs = TimerAttributes {
            title,
            target_seconds: target * 60,
        };
        let state = TimerState {
            elapsed_seconds: 0,
            label: format!("0:00 / {target}:00"),
        };
        let shared = Arc::clone(&self.shared);
        thread::spawn(move || {
            let result = pollster::block_on(shared.activities.start_with_hint(
                attrs,
                state,
                ActivityStyle::Standard,
                Some(3600),
                Some(AndroidTierHint::PreferSystemTemplates),
            ));
            match result {
                Ok(handle) => {
                    shared.push_log("start", format!("handle #{}", handle.id().0));
                    shared.set_handle(Some(handle));
                    shared.set_timer_start(Some(Instant::now()));
                }
                Err(err) => {
                    shared.push_log("start", format!("error: {err}"));
                }
            }
        });
    }

    fn on_update(&self, alert: bool) {
        let Some(handle_id) = self.shared.active_handle_id() else {
            self.shared.push_log("update", "no active activity");
            return;
        };
        let elapsed = self.current_elapsed_seconds();
        let state = TimerState {
            elapsed_seconds: elapsed,
            label: format_elapsed(elapsed),
        };
        let alert_config = alert.then(|| AlertConfig {
            title: "Timer".into(),
            body: format!("{}s elapsed", elapsed),
            sound: AlertSound::Default,
        });
        let shared = Arc::clone(&self.shared);
        thread::spawn(move || {
            match pollster::block_on(shared.activities.update(handle_id, state, alert_config)) {
                Ok(()) => shared.push_log("update", format!("elapsed={elapsed}s")),
                Err(err) => shared.push_log("update", format!("error: {err}")),
            }
        });
    }

    fn on_end(&self, dismissal: DismissalPolicy) {
        let Some(handle_id) = self.shared.active_handle_id() else {
            self.shared.push_log("end", "no active activity");
            return;
        };
        let elapsed = self.current_elapsed_seconds();
        let final_state = TimerState {
            elapsed_seconds: elapsed,
            label: format!("done — {}", format_elapsed(elapsed)),
        };
        let shared = Arc::clone(&self.shared);
        thread::spawn(move || {
            match pollster::block_on(shared.activities.end(
                handle_id,
                Some(final_state),
                dismissal,
            )) {
                Ok(()) => {
                    shared.push_log("end", "ok");
                    shared.set_handle(None);
                    shared.set_timer_start(None);
                }
                Err(err) => shared.push_log("end", format!("error: {err}")),
            }
        });
    }

    fn on_probe_enabled(&self) {
        let shared = Arc::clone(&self.shared);
        thread::spawn(
            move || match pollster::block_on(shared.activities.are_activities_enabled()) {
                Ok(true) => shared.push_log("are_activities_enabled", "true"),
                Ok(false) => shared.push_log("are_activities_enabled", "false"),
                Err(err) => shared.push_log("are_activities_enabled", format!("error: {err}")),
            },
        );
    }

    fn on_restore(&self) {
        let shared = Arc::clone(&self.shared);
        thread::spawn(
            move || match pollster::block_on(shared.activities.restore()) {
                Ok(items) => {
                    shared.push_log("restore", format!("{} activities", items.len()));
                    if let Some(first) = items.into_iter().next() {
                        shared.push_log(
                            "restore",
                            format!(
                                "handle #{}, elapsed={}s",
                                first.handle.id().0,
                                first.state.elapsed_seconds
                            ),
                        );
                        shared.set_handle(Some(first.handle));
                    }
                }
                Err(err) => shared.push_log("restore", format!("error: {err}")),
            },
        );
    }

    fn current_elapsed_seconds(&self) -> u32 {
        self.shared
            .timer_start
            .lock()
            .expect("timer mutex")
            .map_or(0, |t| Instant::now().duration_since(t).as_secs() as u32)
    }
}

fn format_elapsed(seconds: u32) -> String {
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

impl eframe::App for DemoApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Repaint every ~500ms while an activity is live so the elapsed
        // counter ticks visibly.
        if self.shared.timer_start.lock().expect("timer mutex").is_some()
            && self.last_tick.elapsed() >= Duration::from_millis(500)
        {
            self.last_tick = Instant::now();
            ctx.request_repaint_after(Duration::from_millis(500));
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("istmo Live Activity Demo");
            ui.label("Timer that flows through the LiveActivity plugin — Rust core, native rendering.");

            ui.separator();

            ui.horizontal(|ui| {
                ui.label("Title:");
                ui.text_edit_singleline(&mut self.title_input);
            });
            ui.horizontal(|ui| {
                ui.label("Target (minutes):");
                ui.text_edit_singleline(&mut self.target_input);
            });

            ui.separator();

            let has_active = self.shared.active_handle_id().is_some();
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(!has_active, egui::Button::new("Start"))
                    .clicked()
                {
                    self.on_start();
                }
                if ui
                    .add_enabled(has_active, egui::Button::new("Update"))
                    .clicked()
                {
                    self.on_update(false);
                }
                if ui
                    .add_enabled(has_active, egui::Button::new("Update + alert"))
                    .clicked()
                {
                    self.on_update(true);
                }
                if ui
                    .add_enabled(has_active, egui::Button::new("End"))
                    .clicked()
                {
                    self.on_end(DismissalPolicy::Default);
                }
                if ui
                    .add_enabled(has_active, egui::Button::new("End immediate"))
                    .clicked()
                {
                    self.on_end(DismissalPolicy::Immediate);
                }
            });

            ui.horizontal(|ui| {
                if ui.button("are_activities_enabled?").clicked() {
                    self.on_probe_enabled();
                }
                if ui.button("restore_active").clicked() {
                    self.on_restore();
                }
            });

            ui.separator();
            ui.label(format!("Elapsed: {}", format_elapsed(self.current_elapsed_seconds())));

            ui.separator();
            ui.strong("Platform capabilities");
            if let Some(caps) = self
                .shared
                .capabilities
                .lock()
                .expect("capabilities mutex")
                .as_ref()
            {
                match caps {
                    PlatformCapabilities::Ios(ios) => {
                        ui.label(format!(
                            "iOS — ActivityKit: {}, enabled: {}, Dynamic Island: {}, push: {}",
                            ios.activity_kit_available,
                            ios.activities_enabled,
                            ios.dynamic_island,
                            ios.push_updates,
                        ));
                    }
                    PlatformCapabilities::Android(a) => {
                        ui.label(format!(
                            "Android — custom: {}, ProgressStyle: {}, LiveUpdate: {}, notifications enabled: {}",
                            a.supports_custom,
                            a.supports_progress_style,
                            a.supports_live_update,
                            a.notifications_enabled,
                        ));
                    }
                    PlatformCapabilities::Unsupported => {
                        ui.label("Unsupported platform (desktop / WASM).");
                    }
                }
            } else {
                ui.label("probing…");
            }

            ui.separator();
            ui.strong("Transcript");
            egui::ScrollArea::vertical().max_height(200.0).show(ui, |ui| {
                let lines = self.shared.transcript.lock().expect("transcript mutex");
                for line in lines.iter().rev() {
                    ui.horizontal(|ui| {
                        ui.strong(&line.op);
                        ui.label("→");
                        ui.label(&line.outcome);
                    });
                }
            });
        });
    }
}
