//! Shared eframe UI for the share demo.
//!
//! Exercises every part of `istmo-share` a device can check: sending
//! text, links, generated files and images (alone or mixed), rich
//! previews, cancelling an open sheet, direct-share targets, and the
//! inbox of content other apps shared into the demo. Each call runs on
//! its own thread so the UI stays live while the sheet is up.

use std::future::{Future, poll_fn};
use std::pin::pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::Poll;
use std::thread;
use std::time::Duration;

use eframe::egui;
use istmo_core::IstmoError;
use istmo_share::{
    IncomingShare, ShareAnchor, ShareCapabilities, ShareClient, ShareError, ShareFile, ShareInbox,
    SharePreview, ShareRequest, ShareTarget,
};

use crate::png;

/// Room for status bars / notches on phones; the demo does not track
/// the exact safe area.
#[cfg(any(target_os = "android", target_os = "ios"))]
const TOP_INSET: f32 = 48.0;
#[cfg(not(any(target_os = "android", target_os = "ios")))]
const TOP_INSET: f32 = 0.0;

/// Window id the desktop `main.rs` registers in the istmo-window registry.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
const WINDOW_ID: Option<u64> = Some(1);
#[cfg(any(target_os = "android", target_os = "ios"))]
const WINDOW_ID: Option<u64> = None;

/// How long "share, then cancel" leaves the sheet up.
const CANCEL_AFTER: Duration = Duration::from_secs(3);

#[derive(Debug, Clone)]
struct LogLine {
    op: String,
    outcome: String,
}

/// State shared between the UI, the call threads and the inbox thread.
#[derive(Debug)]
pub struct SharedState {
    client: ShareClient,
    egui_ctx: egui::Context,
    transcript: Mutex<Vec<LogLine>>,
    /// Label of the in-flight call and the switch that cancels it.
    pending: Mutex<Option<(String, flume::Sender<()>)>>,
    capabilities: Mutex<Option<ShareCapabilities>>,
    received: Mutex<Vec<IncomingShare>>,
}

impl SharedState {
    /// Build the state and start listening on `inbox`: shares that
    /// cold-started the app were buffered and arrive first.
    #[must_use]
    pub fn new(client: ShareClient, inbox: ShareInbox, egui_ctx: egui::Context) -> Arc<Self> {
        let shared = Arc::new(Self {
            client,
            egui_ctx,
            transcript: Mutex::new(Vec::new()),
            pending: Mutex::new(None),
            capabilities: Mutex::new(None),
            received: Mutex::new(Vec::new()),
        });
        let listener = Arc::clone(&shared);
        thread::spawn(move || {
            let stream = inbox.stream();
            while let Ok(share) = stream.recv() {
                listener.push_log("received".to_owned(), summarize(&share));
                lock(&listener.received).insert(0, share);
                listener.egui_ctx.request_repaint();
            }
        });
        shared
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
            cancel.send(()).ok();
        }
    }
}

#[derive(Debug)]
#[allow(clippy::struct_excessive_bools)]
pub struct DemoApp {
    shared: Arc<SharedState>,
    text: String,
    url: String,
    subject: String,
    attach_note: bool,
    attach_image: bool,
    rich_preview: bool,
    fetch_metadata: bool,
}

impl DemoApp {
    #[must_use]
    pub fn new(shared: Arc<SharedState>) -> Self {
        let app = Self {
            shared,
            text: "Sent from the istmo share demo".to_owned(),
            url: "https://github.com/SergioRibera/istmo".to_owned(),
            subject: "istmo share demo".to_owned(),
            attach_note: false,
            attach_image: false,
            rich_preview: false,
            fetch_metadata: false,
        };
        app.on_capabilities();
        app
    }

    /// Run `call` on a worker thread; the UI's cancel button (or
    /// `cancel_after`) drops its future, which cancels the share.
    fn spawn_call<F>(&self, op: String, cancel_after: Option<Duration>, call: F)
    where
        F: FnOnce(&ShareClient, &flume::Receiver<()>) -> String + Send + 'static,
    {
        let shared = Arc::clone(&self.shared);
        let (cancel_tx, cancel_rx) = flume::bounded(1);
        if let Some(delay) = cancel_after {
            let timer = cancel_tx.clone();
            thread::spawn(move || {
                thread::sleep(delay);
                timer.send(()).ok();
            });
        }
        shared.set_pending(Some((op.clone(), cancel_tx)));
        thread::spawn(move || {
            let outcome = call(&shared.client, &cancel_rx);
            shared.set_pending(None);
            shared.push_log(op, outcome);
        });
    }

    fn request(&self) -> ShareRequest {
        let mut request = ShareRequest::default();
        if !self.text.trim().is_empty() {
            request = request.with_text(self.text.clone());
        }
        if !self.url.trim().is_empty() {
            request = request.with_url(self.url.trim().to_owned());
        }
        if !self.subject.trim().is_empty() {
            request = request.with_subject(self.subject.clone());
        }
        if self.attach_note {
            let note = format!(
                "{}\n\n{}\n\nGenerated by the istmo share demo.\n",
                self.subject, self.text
            );
            request = request.with_file(
                ShareFile::bytes("istmo-note.txt", note.into_bytes()).with_mime_type("text/plain"),
            );
        }
        if self.attach_image {
            request = request.with_file(
                ShareFile::bytes("istmo-gradient.png", png::gradient(40))
                    .with_mime_type("image/png"),
            );
        }
        if self.rich_preview {
            request = request.with_preview(SharePreview {
                title: Some(format!("{} (preview)", self.subject)),
                thumbnail: Some(
                    ShareFile::bytes("preview.png", png::gradient(200)).with_mime_type("image/png"),
                ),
                fetch_link_metadata: self.fetch_metadata,
            });
        }
        request.with_anchor(ShareAnchor {
            window_id: WINDOW_ID,
            rect: None,
        })
    }

    fn on_share(&self, cancel_after: Option<Duration>) {
        let request = self.request();
        let op = cancel_after.map_or_else(
            || "share".to_owned(),
            |delay| format!("share (cancel after {}s)", delay.as_secs()),
        );
        self.spawn_call(op, cancel_after, move |client, cancel| {
            run(client.share(request), cancel, |outcome| {
                format!("{outcome:?}")
            })
        });
    }

    fn on_capabilities(&self) {
        let shared = Arc::clone(&self.shared);
        self.spawn_call("capabilities".to_owned(), None, move |client, cancel| {
            run(client.capabilities(), cancel, |caps| {
                *lock(&shared.capabilities) = Some(caps);
                format!("{caps:?}")
            })
        });
    }

    fn on_set_targets(&self) {
        let targets = vec![
            ShareTarget::new("family", "Family chat")
                .with_icon(ShareFile::bytes("family.png", png::gradient(90))),
            ShareTarget::new("work", "Work notes")
                .with_icon(ShareFile::bytes("work.png", png::gradient(170))),
        ];
        self.spawn_call(
            "set_share_targets(2)".to_owned(),
            None,
            move |client, cancel| {
                run(client.set_share_targets(targets), cancel, |()| {
                    "published — share from another app to see them".to_owned()
                })
            },
        );
    }

    fn on_clear_targets(&self) {
        self.spawn_call(
            "set_share_targets([])".to_owned(),
            None,
            move |client, cancel| {
                run(client.set_share_targets(Vec::new()), cancel, |()| {
                    "cleared".to_owned()
                })
            },
        );
    }

    fn capabilities_ui(&self, ui: &mut egui::Ui) {
        let caps = *lock(&self.shared.capabilities);
        let Some(caps) = caps else {
            ui.label("capabilities: loading…");
            return;
        };
        let flags = [
            ("send", caps.send),
            ("files", caps.files),
            ("mixed", caps.mixed_content),
            ("preview", caps.rich_preview),
            ("completion", caps.reports_completion),
            ("target", caps.reports_target),
            ("receive", caps.receive),
            ("direct share", caps.direct_share),
            ("dismiss on cancel", caps.dismiss_on_cancel),
        ];
        ui.horizontal_wrapped(|ui| {
            for (name, on) in flags {
                ui.label(format!("{} {name}", if on { "✔" } else { "✘" }));
            }
        });
    }

    fn send_ui(&mut self, ui: &mut egui::Ui, idle: bool) {
        egui::Grid::new("request").num_columns(2).show(ui, |ui| {
            ui.label("text");
            ui.text_edit_singleline(&mut self.text);
            ui.end_row();
            ui.label("link");
            ui.text_edit_singleline(&mut self.url);
            ui.end_row();
            ui.label("subject");
            ui.text_edit_singleline(&mut self.subject);
            ui.end_row();
        });
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(&mut self.attach_note, "attach note.txt");
            ui.checkbox(&mut self.attach_image, "attach image.png");
            ui.checkbox(&mut self.rich_preview, "rich preview");
            ui.add_enabled(
                self.rich_preview,
                egui::Checkbox::new(&mut self.fetch_metadata, "fetch link metadata (iOS)"),
            );
        });
        ui.horizontal_wrapped(|ui| {
            if ui.add_enabled(idle, egui::Button::new("share")).clicked() {
                self.on_share(None);
            }
            if ui
                .add_enabled(idle, egui::Button::new("share, cancel after 3 s"))
                .clicked()
            {
                self.on_share(Some(CANCEL_AFTER));
            }
            if ui
                .add_enabled(idle, egui::Button::new("refresh capabilities"))
                .clicked()
            {
                self.on_capabilities();
            }
        });
    }

    fn received_ui(&self, ui: &mut egui::Ui) {
        let received = lock(&self.shared.received).clone();
        if received.is_empty() {
            ui.label("Nothing yet — share text, a link or files to \"istmo share demo\" from another app.");
            return;
        }
        let mut cleaned = None;
        for (index, share) in received.iter().enumerate() {
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.monospace(summarize(share));
                if let Some(text) = &share.text {
                    ui.label(format!("text: {text}"));
                }
                if let Some(url) = &share.url {
                    ui.hyperlink(url);
                }
                for file in &share.files {
                    ui.monospace(format!(
                        "{} · {} · {} bytes",
                        file.name,
                        file.mime_type.as_deref().unwrap_or("?"),
                        file.size.unwrap_or_default(),
                    ));
                    if let Some(preview) = text_preview(&file.path, file.mime_type.as_deref()) {
                        ui.label(preview);
                    }
                }
                if !share.files.is_empty() && ui.button("delete received files").clicked() {
                    cleaned = Some(index);
                }
            });
        }
        if let Some(index) = cleaned {
            let share = lock(&self.shared.received).remove(index);
            let outcome = ShareInbox::cleanup(&share).map_or_else(
                |err| format!("ERROR: {err}"),
                |()| "files deleted".to_owned(),
            );
            self.shared.push_log("cleanup".to_owned(), outcome);
        }
    }
}

impl eframe::App for DemoApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.add_space(TOP_INSET);
                ui.heading("istmo share demo");
                self.capabilities_ui(ui);
                ui.separator();

                let pending = self.shared.pending_label();
                let idle = pending.is_none();

                ui.strong("send");
                self.send_ui(ui, idle);

                if let Some(op) = pending {
                    ui.horizontal_wrapped(|ui| {
                        ui.spinner();
                        ui.label(format!("{op}: sheet open…"));
                        if ui.button("cancel").clicked() {
                            self.shared.cancel_pending();
                        }
                    });
                }

                ui.separator();
                ui.strong("direct share");
                ui.label("Publishes two targets for the system share sheet's suggestion row.");
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .add_enabled(idle, egui::Button::new("publish targets"))
                        .clicked()
                    {
                        self.on_set_targets();
                    }
                    if ui
                        .add_enabled(idle, egui::Button::new("clear targets"))
                        .clicked()
                    {
                        self.on_clear_targets();
                    }
                });

                ui.separator();
                ui.strong("received");
                self.received_ui(ui);

                ui.separator();
                ui.strong("transcript");
                let lines = lock(&self.shared.transcript).clone();
                for line in lines.iter().rev() {
                    ui.monospace(format!("{} -> {}", line.op, line.outcome));
                }
            });
        });
    }
}

/// Drive `call` to completion unless the UI cancels it first, which
/// drops the call's future and so cancels the share.
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
        Some(Err(err)) => format!("ERROR: {}", ShareError::from(err)),
        None => "cancelled — the future was dropped".to_owned(),
    }
}

fn summarize(share: &IncomingShare) -> String {
    let mut parts = Vec::new();
    if share.text.is_some() {
        parts.push("text".to_owned());
    }
    if share.url.is_some() {
        parts.push("link".to_owned());
    }
    if !share.files.is_empty() {
        parts.push(format!("{} file(s)", share.files.len()));
    }
    if let Some(subject) = &share.subject {
        parts.push(format!("subject \"{subject}\""));
    }
    if let Some(target) = &share.target_id {
        parts.push(format!("to target \"{target}\""));
    }
    if let Some(source) = &share.source_app {
        parts.push(format!("from {source}"));
    }
    if parts.is_empty() {
        "empty share".to_owned()
    } else {
        parts.join(", ")
    }
}

/// First line of a small text file, to show the copy is readable.
fn text_preview(path: &str, mime: Option<&str>) -> Option<String> {
    if !mime.is_some_and(|m| m.starts_with("text/")) {
        return None;
    }
    let content = std::fs::read_to_string(path).ok()?;
    let first = content.lines().find(|l| !l.trim().is_empty())?;
    Some(format!(
        "\u{201c}{}\u{201d}",
        first.chars().take(80).collect::<String>()
    ))
}

/// The demo never panics while holding these locks, but a poisoned
/// transcript is still worth rendering.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
