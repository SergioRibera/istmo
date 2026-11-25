//! A small browser on `istmo-webview`.
//!
//! - Address bar, history buttons, title / URL / progress from the
//!   view's telemetry lane.
//! - "Local page": `app://localhost/` served from `assets/web/`, with a
//!   bridge the page uses to ask Rust for the time and receive messages.
//! - "Sign in with Microsoft": the system browser and a loopback
//!   redirect through `istmo-web-auth` (needs `ISTMO_DEMO_MS_CLIENT_ID`,
//!   an app registration with a "Mobile and desktop" redirect of
//!   `http://localhost`).
//!
//! The view sits inside this window when the engine can embed (X11,
//! macOS, Windows) and opens a window of its own otherwise (Wayland).
//!
//! `cargo run -p webview-demo`

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use eframe::egui;
use istmo_core::{Runtime, RuntimeConfig, RuntimeInit};
use istmo_web_auth::desktop::DesktopWebAuth;
use istmo_web_auth::{
    CallbackSpec, LoopbackCallback, SignInOptions, WebAuthClient, WebAuthClientExt, WebAuthHost,
};
use istmo_webview::desktop::{DesktopOptions, DesktopWebViewFactory};
use istmo_webview::{
    AssetsSource, BridgeConfig, ControlEvent, CustomScheme, HistoryNav, HistoryState,
    OutgoingMessage, SchemeSource, TelemetryEvent, TelemetryKind, ViewEvent, ViewEvents, ViewFrame,
    ViewHandle, WebViewClient, WebViewClientExt, WebViewHost, WebViewInitConfig, WebViewSpec,
};
use istmo_window::{WindowId, WindowRegistry};

const LOCAL_PAGE: &str = "app://localhost/index.html";
const LOCAL_ORIGIN: &str = "app://localhost";
const WINDOW: WindowId = WindowId(1);
/// What the toolbar shows; the spec must enable it too.
const TELEMETRY: [TelemetryKind; 4] = [
    TelemetryKind::Title,
    TelemetryKind::Url,
    TelemetryKind::Progress,
    TelemetryKind::History,
];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    // `cargo run` reads `assets/` from this crate, not from istmo-core's.
    istmo_core::assets::set_app_assets(
        option_env!("ISTMO_APP_ASSETS_DIR"),
        env!("CARGO_MANIFEST_DIR"),
    );
    let RuntimeInit { runtime, outbound } = Runtime::init(RuntimeConfig::inline())?;
    // Every host is local: nothing goes out, but keep the queue drained.
    std::thread::Builder::new()
        .name("demo-outbound".into())
        .spawn(move || while outbound.recv().is_ok() {})?;
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1100.0, 760.0]),
        ..eframe::NativeOptions::default()
    };
    eframe::run_native(
        "istmo webview demo",
        options,
        Box::new(move |cc| {
            WindowRegistry::global().register(WINDOW, cc)?;
            runtime.register_host(WebViewHost::new(DesktopWebViewFactory::new(
                &runtime,
                DesktopOptions::default(),
            )));
            runtime.register_host(WebAuthHost::new(DesktopWebAuth::new(&runtime)));
            Ok(Box::new(Browser::new(&runtime, &cc.egui_ctx)?))
        }),
    )?;
    Ok(())
}

/// What a sign-in reported, for the status line.
type SignInResult = Result<String, String>;

struct Browser {
    view: ViewHandle,
    events: ViewEvents,
    telemetry: istmo_core::TypedStream<TelemetryEvent, istmo_webview::WebViewError>,
    auth: Arc<WebAuthClient>,
    embedded: bool,
    backend: String,
    address: String,
    title: String,
    progress: f32,
    history: HistoryState,
    status: String,
    last_frame: Option<ViewFrame>,
    sign_in: Option<flume::Receiver<SignInResult>>,
    /// `ISTMO_DEMO_SELFTEST`: report once the local page is up, then quit.
    selftest: bool,
}

impl std::fmt::Debug for Browser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Browser")
            .field("address", &self.address)
            .finish_non_exhaustive()
    }
}

impl Browser {
    fn new(
        runtime: &Arc<Runtime>,
        ctx: &egui::Context,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let config = WebViewInitConfig {
            schemes: vec![CustomScheme {
                name: "app".into(),
                source: SchemeSource::Assets(AssetsSource { dir: "web".into() }),
            }],
            ..WebViewInitConfig::default()
        };
        let client = Arc::new(pollster::block_on(WebViewClient::from_runtime_with(
            runtime, config,
        ))?);
        let caps = pollster::block_on(client.capabilities())?;
        let spec = if caps.embedded {
            WebViewSpec::embedded(WINDOW.0)
        } else {
            WebViewSpec::window("istmo browser", 1000, 700)
        };
        let view = pollster::block_on(
            client.open_view(
                spec.with_bridge(BridgeConfig::new(["demo"]).allow_origin(LOCAL_ORIGIN))
                    .with_telemetry(TELEMETRY),
            ),
        )?;
        let events = view.control().ok_or("no control lane")?;
        let telemetry = view.telemetry(TELEMETRY)?;
        pollster::block_on(view.load_url(LOCAL_PAGE))?;
        // Engine events arrive between frames: keep egui ticking.
        ctx.request_repaint_after(std::time::Duration::from_millis(50));
        Ok(Self {
            view,
            events,
            telemetry,
            auth: Arc::new(WebAuthClient::from_runtime(runtime)?),
            embedded: caps.embedded,
            backend: format!("{} {}", caps.backend, caps.engine_version),
            address: LOCAL_PAGE.into(),
            title: String::new(),
            progress: 1.0,
            history: HistoryState {
                can_go_back: false,
                can_go_forward: false,
            },
            status: String::new(),
            last_frame: None,
            sign_in: None,
            selftest: std::env::var_os("ISTMO_DEMO_SELFTEST").is_some(),
        })
    }

    /// Drain what the engine reported since the last frame.
    fn poll(&mut self) {
        while let Some(item) = self.telemetry.try_recv() {
            let event = match item {
                Ok(istmo_core::StreamItem::Event(event)) => event,
                Ok(istmo_core::StreamItem::Failed(err)) => {
                    self.status = format!("telemetry stopped: {err}");
                    break;
                }
                _ => break,
            };
            match event {
                TelemetryEvent::Title(title) => self.title = title,
                TelemetryEvent::Url(url) => self.address = url,
                TelemetryEvent::Progress(p) => self.progress = p,
                TelemetryEvent::History(h) => self.history = h,
                _ => {}
            }
        }
        while let Some(event) = self.events.try_next() {
            match event {
                ViewEvent::Invoke(call) => {
                    let answer = if call.message().data == b"time" {
                        let secs = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .map_or(0, |d| d.as_secs());
                        Ok(format!("{secs} s since the epoch, from Rust").into_bytes())
                    } else {
                        Err("unknown request".to_owned())
                    };
                    if let Err(err) = pollster::block_on(call.reply(answer)) {
                        self.status = format!("reply failed: {err}");
                    }
                }
                ViewEvent::Other(ControlEvent::NavigationFailed(f)) => {
                    self.status = format!("could not load {}: {}", f.url, f.description);
                }
                ViewEvent::Other(ControlEvent::CloseRequested) => {
                    self.status = "the page asked to close".into();
                }
                ViewEvent::Decision(decision) => {
                    // A browser would prompt; the demo declines.
                    pollster::block_on(decision.deny()).ok();
                }
                _ => {}
            }
        }
        if let Some(rx) = &self.sign_in
            && let Ok(result) = rx.try_recv()
        {
            self.status = match result {
                Ok(text) | Err(text) => text,
            };
            self.sign_in = None;
        }
    }

    fn go(&mut self) {
        let mut url = self.address.trim().to_owned();
        if !url.contains("://") {
            url = format!("https://{url}");
        }
        if let Err(err) = pollster::block_on(self.view.load_url(url)) {
            self.status = format!("load failed: {err}");
        }
    }

    fn navigate(&mut self, nav: HistoryNav) {
        if let Err(err) = pollster::block_on(self.view.navigate_history(nav)) {
            self.status = format!("navigation failed: {err}");
        }
    }

    fn send_to_page(&mut self) {
        let message = OutgoingMessage {
            channel: "demo".into(),
            data: b"hello from the Rust side".to_vec(),
            expected_origin: LOCAL_ORIGIN.into(),
        };
        if let Err(err) = pollster::block_on(self.view.post_message(message)) {
            self.status = format!("not delivered: {err} (open the local page first)");
        }
    }

    /// Authorization-code flow with PKCE, on its own thread: the token
    /// exchange is the app's business and is left out.
    fn start_sign_in(&mut self) {
        let Some(client_id) = std::env::var("ISTMO_DEMO_MS_CLIENT_ID")
            .ok()
            .filter(|s| !s.is_empty())
        else {
            self.status = "set ISTMO_DEMO_MS_CLIENT_ID to an Entra app registration id".into();
            return;
        };
        let (tx, rx) = flume::bounded(1);
        let auth = Arc::clone(&self.auth);
        let spawned = std::thread::Builder::new().name("demo-sign-in".into()).spawn(move || {
            let result = pollster::block_on(async {
                let flow = auth
                    .begin(CallbackSpec::Loopback(LoopbackCallback { path: "/callback".into() }))
                    .await
                    .map_err(|e| e.to_string())?;
                let url = format!(
                    "https://login.microsoftonline.com/common/oauth2/v2.0/authorize?client_id={client_id}&response_type=code&response_mode=query&scope=openid%20profile%20offline_access&redirect_uri={}&state={}&nonce={}&code_challenge={}&code_challenge_method={}",
                    flow.redirect_uri_encoded(),
                    flow.state(),
                    flow.nonce(),
                    flow.pkce().challenge(),
                    flow.pkce().method(),
                );
                let callback = flow.complete(url, SignInOptions::default()).await.map_err(|e| e.to_string())?;
                Ok(callback.param("code").map_or_else(
                    || "the callback carried no code".into(),
                    |code| format!("signed in: authorization code of {} chars, ready to exchange", code.len()),
                ))
            });
            drop(tx.send(result));
        });
        match spawned {
            Ok(_) => {
                self.status = "finish signing in in your browser".into();
                self.sign_in = Some(rx);
            }
            Err(err) => self.status = format!("no thread for sign-in: {err}"),
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui
                .add_enabled(self.history.can_go_back, egui::Button::new("Back"))
                .clicked()
            {
                self.navigate(HistoryNav::Back);
            }
            if ui
                .add_enabled(self.history.can_go_forward, egui::Button::new("Forward"))
                .clicked()
            {
                self.navigate(HistoryNav::Forward);
            }
            if ui.button("Reload").clicked() {
                self.navigate(HistoryNav::Reload);
            }
            let field = ui.add(
                egui::TextEdit::singleline(&mut self.address)
                    .desired_width(ui.available_width() - 330.0),
            );
            if field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                self.go();
            }
            if ui.button("Go").clicked() {
                self.go();
            }
            if ui.button("Local page").clicked() {
                self.address = LOCAL_PAGE.into();
                self.go();
            }
            if ui.button("Send to page").clicked() {
                self.send_to_page();
            }
            if ui
                .add_enabled(
                    self.sign_in.is_none(),
                    egui::Button::new("Sign in with Microsoft"),
                )
                .clicked()
            {
                self.start_sign_in();
            }
        });
        if self.progress < 1.0 {
            ui.add(egui::ProgressBar::new(self.progress).desired_height(3.0));
        }
    }

    /// Keep the native view over `rect` (physical pixels of this window).
    fn place(&mut self, rect: egui::Rect, pixels_per_point: f32) {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "on-screen pixel sizes"
        )]
        let frame = ViewFrame {
            x: (rect.min.x * pixels_per_point).round() as i32,
            y: (rect.min.y * pixels_per_point).round() as i32,
            width: (rect.width() * pixels_per_point).round().max(0.0) as u32,
            height: (rect.height() * pixels_per_point).round().max(0.0) as u32,
            visible: true,
        };
        if self.last_frame != Some(frame) {
            // Coalesced in the presenter: no call per frame.
            self.view.presenter().sync(frame);
            self.last_frame = Some(frame);
        }
    }
}

impl eframe::App for Browser {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll();
        if self.selftest && self.title == "Local page" {
            self.selftest = false;
            let text = pollster::block_on(self.view.eval(istmo_webview::ScriptCall {
                source: "document.querySelector('h1').textContent".into(),
                world: istmo_webview::ScriptWorld::Page,
                expected_origin: LOCAL_ORIGIN.into(),
            }));
            println!(
                "SELFTEST backend={} embedded={} title={:?} h1={text:?}",
                self.backend, self.embedded, self.title
            );
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| self.toolbar(ui));
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(&self.backend).weak());
                ui.separator();
                ui.label(&self.title);
                ui.separator();
                ui.label(&self.status);
            });
        });
        egui::CentralPanel::default()
            .frame(egui::Frame::none())
            .show(ctx, |ui| {
                let rect = ui.max_rect();
                if self.embedded {
                    self.place(rect, ctx.pixels_per_point());
                } else {
                    ui.centered_and_justified(|ui| {
                        ui.label("This engine cannot embed here: the page is in its own window.");
                    });
                }
            });
        ctx.request_repaint_after(std::time::Duration::from_millis(50));
    }
}
