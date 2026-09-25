//! Shared eframe UI for the biometric demo.
//!
//! Covers the availability probe, authentication and the biometric-bound
//! vault, with a transcript of every call. Each call runs on its own
//! thread so the UI stays live while the system prompt is up, and can be
//! cancelled from the UI.

use std::future::{Future, poll_fn};
use std::pin::pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::Poll;
use std::thread;

use eframe::egui;
use istmo_biometric::{AuthPolicy, AuthPrompt, BiometricClient, BiometricError};
use istmo_core::IstmoError;

/// Room for status bars / notches on phones; the demo does not track
/// the exact safe area.
#[cfg(any(target_os = "android", target_os = "ios"))]
const TOP_INSET: f32 = 48.0;
#[cfg(not(any(target_os = "android", target_os = "ios")))]
const TOP_INSET: f32 = 0.0;

const POLICIES: [(AuthPolicy, &str); 3] = [
    (AuthPolicy::BiometricStrong, "strong biometrics"),
    (AuthPolicy::BiometricWeak, "weak biometrics"),
    (
        AuthPolicy::BiometricOrDeviceCredential,
        "biometrics or device credential",
    ),
];

#[derive(Debug, Clone)]
struct LogLine {
    op: String,
    outcome: String,
}

/// State shared between the UI and the call threads.
#[derive(Debug)]
pub struct SharedState {
    client: BiometricClient,
    egui_ctx: egui::Context,
    transcript: Mutex<Vec<LogLine>>,
    /// Label of the in-flight call and the switch that cancels it.
    pending: Mutex<Option<(String, flume::Sender<()>)>>,
}

impl SharedState {
    #[must_use]
    pub fn new(client: BiometricClient, egui_ctx: egui::Context) -> Arc<Self> {
        Arc::new(Self {
            client,
            egui_ctx,
            transcript: Mutex::new(Vec::new()),
            pending: Mutex::new(None),
        })
    }

    fn push_log(&self, op: String, outcome: String) {
        lock(&self.transcript).push(LogLine { op, outcome });
        self.egui_ctx.request_repaint();
    }

    fn set_pending(&self, pending: Option<(String, flume::Sender<()>)>) {
        *lock(&self.pending) = pending;
        self.egui_ctx.request_repaint();
    }

    fn pending_label(&self) -> Option<String> {
        lock(&self.pending).as_ref().map(|(label, _)| label.clone())
    }

    fn cancel_pending(&self) {
        let pending = lock(&self.pending).take();
        if let Some((_, cancel)) = pending {
            let _ = cancel.send(());
        }
    }
}

#[derive(Debug)]
pub struct DemoApp {
    shared: Arc<SharedState>,
    policy: AuthPolicy,
    reason: String,
    alias: String,
    secret: String,
}

impl DemoApp {
    #[must_use]
    pub fn new(shared: Arc<SharedState>) -> Self {
        Self {
            shared,
            policy: AuthPolicy::BiometricOrDeviceCredential,
            reason: "Confirm it's you to continue".to_owned(),
            alias: "demo.token".to_owned(),
            secret: "correct horse battery staple".to_owned(),
        }
    }

    fn prompt(&self) -> AuthPrompt {
        AuthPrompt::new("istmo biometric demo", self.reason.clone())
            .subtitle("Biometric plugin")
            .policy(self.policy)
    }

    /// Run `call` on a worker thread; the UI's cancel button drops its
    /// future, which cancels the prompt on the platform side.
    fn spawn_call<F>(&self, op: String, call: F)
    where
        F: FnOnce(&BiometricClient, &flume::Receiver<()>) -> String + Send + 'static,
    {
        let shared = Arc::clone(&self.shared);
        let (cancel_tx, cancel_rx) = flume::bounded(1);
        shared.set_pending(Some((op.clone(), cancel_tx)));
        thread::spawn(move || {
            let outcome = call(&shared.client, &cancel_rx);
            shared.set_pending(None);
            shared.push_log(op, outcome);
        });
    }

    fn on_availability(&self) {
        let policy = self.policy;
        self.spawn_call(
            format!("availability({policy:?})"),
            move |client, cancel| {
                run(client.availability(policy), cancel, |a| {
                    format!(
                        "{:?}, sensors {:?}, device credential: {}, vault: {}",
                        a.status, a.kinds, a.device_credential_available, a.vault_available
                    )
                })
            },
        );
    }

    fn on_authenticate(&self) {
        let prompt = self.prompt();
        self.spawn_call("authenticate".to_owned(), move |client, cancel| {
            run(client.authenticate(prompt), cancel, |method| {
                format!("verified with {method:?}")
            })
        });
    }

    fn on_store(&self) {
        let prompt = self.prompt();
        let alias = self.alias.clone();
        let secret = self.secret.clone().into_bytes();
        self.spawn_call(format!("store_secret({alias})"), move |client, cancel| {
            run(client.store_secret(alias, secret, prompt), cancel, |()| {
                "stored".to_owned()
            })
        });
    }

    fn on_read(&self) {
        let prompt = self.prompt();
        let alias = self.alias.clone();
        self.spawn_call(format!("read_secret({alias})"), move |client, cancel| {
            run(client.read_secret(alias, prompt), cancel, |secret| {
                format!("\"{}\"", String::from_utf8_lossy(&secret))
            })
        });
    }

    fn on_has(&self) {
        let alias = self.alias.clone();
        self.spawn_call(format!("has_secret({alias})"), move |client, cancel| {
            run(client.has_secret(alias), cancel, |has| has.to_string())
        });
    }

    fn on_enrollment_state(&self) {
        self.spawn_call("enrollment_state".to_owned(), move |client, cancel| {
            run(client.enrollment_state(), cancel, |state| {
                state.map_or_else(|| "none".to_owned(), |token| hex(&token))
            })
        });
    }

    fn on_delete(&self) {
        let alias = self.alias.clone();
        self.spawn_call(format!("delete_secret({alias})"), move |client, cancel| {
            run(client.delete_secret(alias), cancel, |()| {
                "deleted".to_owned()
            })
        });
    }
}

impl eframe::App for DemoApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(TOP_INSET);
            ui.heading("istmo biometric demo");
            ui.separator();

            let pending = self.shared.pending_label();
            let idle = pending.is_none();

            egui::Grid::new("prompt").num_columns(2).show(ui, |ui| {
                ui.label("policy");
                egui::ComboBox::from_id_salt("policy")
                    .selected_text(policy_label(self.policy))
                    .show_ui(ui, |ui| {
                        for (policy, label) in POLICIES {
                            ui.selectable_value(&mut self.policy, policy, label);
                        }
                    });
                ui.end_row();

                ui.label("reason");
                ui.text_edit_singleline(&mut self.reason);
                ui.end_row();
            });
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_enabled(idle, egui::Button::new("availability"))
                    .clicked()
                {
                    self.on_availability();
                }
                if ui
                    .add_enabled(idle, egui::Button::new("authenticate"))
                    .clicked()
                {
                    self.on_authenticate();
                }
                if ui
                    .add_enabled(idle, egui::Button::new("enrollment state"))
                    .clicked()
                {
                    self.on_enrollment_state();
                }
            });

            ui.separator();
            ui.label("vault");
            egui::Grid::new("vault").num_columns(2).show(ui, |ui| {
                ui.label("alias");
                ui.text_edit_singleline(&mut self.alias);
                ui.end_row();

                ui.label("secret");
                ui.text_edit_singleline(&mut self.secret);
                ui.end_row();
            });
            ui.horizontal_wrapped(|ui| {
                if ui.add_enabled(idle, egui::Button::new("store")).clicked() {
                    self.on_store();
                }
                if ui.add_enabled(idle, egui::Button::new("read")).clicked() {
                    self.on_read();
                }
                if ui.add_enabled(idle, egui::Button::new("has")).clicked() {
                    self.on_has();
                }
                if ui.add_enabled(idle, egui::Button::new("delete")).clicked() {
                    self.on_delete();
                }
            });

            if let Some(op) = pending {
                ui.separator();
                ui.horizontal_wrapped(|ui| {
                    ui.spinner();
                    ui.label(format!("{op}: waiting for the user…"));
                    if ui.button("cancel").clicked() {
                        self.shared.cancel_pending();
                    }
                });
                #[cfg(target_os = "linux")]
                ui.label("Linux has no system prompt: touch the fingerprint reader now.");
            }

            ui.separator();
            ui.label("transcript");
            let lines = lock(&self.shared.transcript).clone();
            egui::ScrollArea::vertical().show(ui, |ui| {
                for line in lines.iter().rev() {
                    ui.monospace(format!("{} -> {}", line.op, line.outcome));
                }
            });
        });
    }
}

/// Drive `call` to completion unless the UI cancels it first, which
/// drops the call's future and so cancels it on the platform side.
fn run<T>(
    call: impl Future<Output = Result<T, IstmoError>>,
    cancel: &flume::Receiver<()>,
    render: impl FnOnce(T) -> String,
) -> String {
    let outcome = pollster::block_on(async {
        let mut call = pin!(call);
        let mut cancelled = pin!(cancel.recv_async());
        poll_fn(|cx| {
            if let Poll::Ready(result) = call.as_mut().poll(cx) {
                return Poll::Ready(Some(result));
            }
            if cancelled.as_mut().poll(cx).is_ready() {
                return Poll::Ready(None);
            }
            Poll::Pending
        })
        .await
    });
    match outcome {
        Some(Ok(value)) => render(value),
        Some(Err(err)) => match BiometricError::try_from(err) {
            Ok(domain) => format!("ERROR: {domain}"),
            Err(transport) => format!("TRANSPORT ERROR: {transport}"),
        },
        None => "cancelled from the UI".to_owned(),
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

fn policy_label(policy: AuthPolicy) -> &'static str {
    POLICIES
        .iter()
        .find(|(p, _)| *p == policy)
        .map_or("?", |(_, label)| label)
}

/// The demo never panics while holding these locks, but a poisoned
/// transcript is still worth rendering.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
