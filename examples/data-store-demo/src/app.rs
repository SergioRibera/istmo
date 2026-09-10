//! Shared egui UI.
//!
//! Runs on desktop (against the emulator), Android (against
//! `DataStoreBackendImpl.kt` + `SharedPreferences`), and iOS (against
//! `DataStoreBackendImpl.swift` + `UserDefaults`). Zero
//! `#[cfg(target_os = ...)]` in the eframe app impl — the code path is
//! literally identical.

use std::sync::{Arc, Mutex};
use std::thread;

use eframe::egui;
use istmo_core::codec;
use istmo_data_store::{DataStoreClient, DataStoreError};

const DEMO_NAMESPACE: &str = "demo_prefs";

/// Retention shape for one op's outcome — the transcript panel walks these
/// in reverse insertion order.
#[derive(Debug, Clone)]
pub struct LogLine {
    pub op: String,
    pub outcome: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueKind {
    String,
    I64,
    F64,
    Bool,
    Bytes,
}

impl ValueKind {
    const ALL: [Self; 5] = [Self::String, Self::I64, Self::F64, Self::Bool, Self::Bytes];

    const fn label(self) -> &'static str {
        match self {
            Self::String => "string",
            Self::I64 => "i64",
            Self::F64 => "f64",
            Self::Bool => "bool",
            Self::Bytes => "bytes (hex)",
        }
    }

    const fn wire(self) -> &'static str {
        match self {
            Self::String => "string",
            Self::I64 => "i64",
            Self::F64 => "f64",
            Self::Bool => "bool",
            Self::Bytes => "bytes",
        }
    }
}

/// Shared handle every worker task borrows: the client (for ops), the egui
/// context (to trigger repaints when a task finishes) and the transcript.
#[derive(Debug)]
pub struct SharedState {
    pub client: DataStoreClient,
    pub egui_ctx: egui::Context,
    pub transcript: Mutex<Vec<LogLine>>,
    pub keys_snapshot: Mutex<Vec<String>>,
}

impl SharedState {
    fn push_log(&self, op: impl Into<String>, outcome: impl Into<String>) {
        self.transcript
            .lock()
            .unwrap()
            .push(LogLine { op: op.into(), outcome: outcome.into() });
        self.egui_ctx.request_repaint();
    }

    fn replace_keys(&self, keys: Vec<String>) {
        *self.keys_snapshot.lock().unwrap() = keys;
        self.egui_ctx.request_repaint();
    }
}

#[derive(Debug)]
pub struct DemoApp {
    shared: Arc<SharedState>,
    key: String,
    value: String,
    kind: ValueKind,
}

impl DemoApp {
    #[must_use]
    pub fn new(shared: Arc<SharedState>) -> Self {
        Self { shared, key: "user_id".into(), value: "u_42".into(), kind: ValueKind::String }
    }

    fn spawn_op<F>(&self, op: impl Into<String>, task: F)
    where
        F: FnOnce(&DataStoreClient) -> Result<String, String> + Send + 'static,
    {
        let shared = Arc::clone(&self.shared);
        let op = op.into();
        thread::spawn(move || {
            let outcome = match task(&shared.client) {
                Ok(msg) => msg,
                Err(msg) => format!("ERROR: {msg}"),
            };
            shared.push_log(op, outcome);
            if let Ok(keys) = pollster::block_on(shared.client.keys()) {
                shared.replace_keys(keys);
            }
        });
    }

    fn on_set(&self) {
        let key = self.key.clone();
        let value_text = self.value.clone();
        let kind = self.kind;
        let op = format!("set_{}({key}, {value_text})", kind.wire());
        self.spawn_op(op, move |client| match kind {
            ValueKind::String => pollster::block_on(client.set_string(key, value_text))
                .map(|()| "ok".to_owned())
                .map_err(err_to_string),
            ValueKind::I64 => {
                let v: i64 = value_text
                    .parse()
                    .map_err(|e: std::num::ParseIntError| e.to_string())?;
                pollster::block_on(client.set_i64(key, v))
                    .map(|()| "ok".to_owned())
                    .map_err(err_to_string)
            }
            ValueKind::F64 => {
                let v: f64 = value_text
                    .parse()
                    .map_err(|e: std::num::ParseFloatError| e.to_string())?;
                pollster::block_on(client.set_f64(key, v))
                    .map(|()| "ok".to_owned())
                    .map_err(err_to_string)
            }
            ValueKind::Bool => {
                let v: bool = value_text
                    .parse()
                    .map_err(|e: std::str::ParseBoolError| e.to_string())?;
                pollster::block_on(client.set_bool(key, v))
                    .map(|()| "ok".to_owned())
                    .map_err(err_to_string)
            }
            ValueKind::Bytes => {
                let v = decode_hex(&value_text)?;
                pollster::block_on(client.set_bytes(key, v))
                    .map(|()| "ok".to_owned())
                    .map_err(err_to_string)
            }
        });
    }

    fn on_get(&self) {
        let key = self.key.clone();
        let kind = self.kind;
        let op = format!("get_{}({key})", kind.wire());
        self.spawn_op(op, move |client| match kind {
            ValueKind::String => pollster::block_on(client.get_string(key))
                .map(render_opt)
                .map_err(err_to_string),
            ValueKind::I64 => pollster::block_on(client.get_i64(key))
                .map(render_opt)
                .map_err(err_to_string),
            ValueKind::F64 => pollster::block_on(client.get_f64(key))
                .map(render_opt)
                .map_err(err_to_string),
            ValueKind::Bool => pollster::block_on(client.get_bool(key))
                .map(render_opt)
                .map_err(err_to_string),
            ValueKind::Bytes => pollster::block_on(client.get_bytes(key))
                .map(|opt| opt.map_or_else(|| "None".to_owned(), |b| hex(&b)))
                .map_err(err_to_string),
        });
    }

    fn on_remove(&self) {
        let key = self.key.clone();
        let op = format!("remove({key})");
        self.spawn_op(op, move |client| {
            pollster::block_on(client.remove(key))
                .map(|removed| if removed { "removed" } else { "absent" }.to_owned())
                .map_err(err_to_string)
        });
    }

    fn on_contains(&self) {
        let key = self.key.clone();
        let op = format!("contains({key})");
        self.spawn_op(op, move |client| {
            pollster::block_on(client.contains(key))
                .map(|b| b.to_string())
                .map_err(err_to_string)
        });
    }

    fn on_clear(&self) {
        self.spawn_op("clear()", move |client| {
            pollster::block_on(client.clear())
                .map(|()| "cleared".to_owned())
                .map_err(err_to_string)
        });
    }
}

impl eframe::App for DemoApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("istmo data-store demo");
            ui.label(format!("namespace: {DEMO_NAMESPACE}"));
            ui.separator();

            egui::Grid::new("form").num_columns(2).show(ui, |ui| {
                ui.label("key");
                ui.text_edit_singleline(&mut self.key);
                ui.end_row();

                ui.label("value");
                ui.text_edit_singleline(&mut self.value);
                ui.end_row();

                ui.label("type");
                egui::ComboBox::new("kind", "")
                    .selected_text(self.kind.label())
                    .show_ui(ui, |ui| {
                        for k in ValueKind::ALL {
                            ui.selectable_value(&mut self.kind, k, k.label());
                        }
                    });
                ui.end_row();
            });

            ui.horizontal_wrapped(|ui| {
                if ui.button("set").clicked() { self.on_set(); }
                if ui.button("get").clicked() { self.on_get(); }
                if ui.button("remove").clicked() { self.on_remove(); }
                if ui.button("contains").clicked() { self.on_contains(); }
                if ui.button("clear").clicked() { self.on_clear(); }
            });

            ui.separator();
            ui.columns(2, |cols| {
                cols[0].heading("keys");
                let keys = self.shared.keys_snapshot.lock().unwrap().clone();
                if keys.is_empty() {
                    cols[0].label("(none)");
                } else {
                    for k in keys {
                        cols[0].label(k);
                    }
                }

                cols[1].heading("transcript");
                let lines = self.shared.transcript.lock().unwrap().clone();
                egui::ScrollArea::vertical()
                    .stick_to_bottom(true)
                    .show(&mut cols[1], |ui| {
                        for line in lines.iter().rev().take(200) {
                            ui.monospace(format!("{} -> {}", line.op, line.outcome));
                        }
                    });
            });
        });
    }
}

fn err_to_string(err: istmo_core::IstmoError) -> String {
    match err {
        istmo_core::IstmoError::PluginError { bytes } => {
            match codec::decode::<DataStoreError>(&bytes) {
                Ok((e, _)) => e.to_string(),
                Err(_) => "PluginError (undecodable)".to_owned(),
            }
        }
        other => other.to_string(),
    }
}

fn render_opt<T: std::fmt::Display>(opt: Option<T>) -> String {
    opt.map_or_else(|| "None".to_owned(), |v| format!("Some({v})"))
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

fn decode_hex(input: &str) -> Result<Vec<u8>, String> {
    let clean: String = input.chars().filter(|c| !c.is_whitespace()).collect();
    if clean.len() % 2 != 0 {
        return Err("hex string has odd length".into());
    }
    (0..clean.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&clean[i..i + 2], 16).map_err(|e| e.to_string()))
        .collect()
}

/// Namespace used by every entrypoint. Kept in one place so the
/// desktop/Android/iOS boots stay in lockstep.
#[must_use]
pub const fn namespace() -> &'static str {
    DEMO_NAMESPACE
}
