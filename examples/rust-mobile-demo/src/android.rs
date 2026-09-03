//! Android entry point + egui view for the full-Rust demo.
//!
//! Layout:
//!
//! * [`android_main`] — invoked by `android-activity` after the NDK glue
//!   thread starts. Installs the tracing bridge, then hands the
//!   `AndroidApp` to eframe with the `wgpu` backend.
//! * [`DemoApp`] — `eframe::App` implementation. Sign-in / sign-out
//!   button, account card with the fields Google returns, one worker
//!   thread (`std::thread::spawn`) per action.
//!
//! Kotlin side is expected to have called `IstmoRuntime.start()` in its
//! `MainActivity.onCreate` — the runtime is a `OnceLock` so double-init
//! is a soft error, and having Kotlin start it first means the pump is
//! draining before `android_main` fires.

use std::sync::{Arc, Mutex};

use android_activity::AndroidApp;
use eframe::egui;
use istmo::IstmoError;
use istmo::plugins::{
    NotificationError, NotificationImportance, NotificationRequest, NotificationsClient,
    PermissionsClient,
};
use istmo_plugins::google_sign_in::{
    OwnedSignInAccount, SignInClient, SignInConfig, SignInError, SignInMode,
};

/// OAuth server client id for the demo. Real apps embed the id issued by
/// Google Cloud Console for the *backend* — the audience the id-token
/// must match. Kept as a placeholder here; replace before shipping.
const SERVER_CLIENT_ID: &str =
    "848096709714-9le17umoe085dtcmrout03qdbcp8cpi7.apps.googleusercontent.com";

const POST_NOTIFICATIONS: &str = "android.permission.POST_NOTIFICATIONS";

/// NDK glue entry. `android-activity` provides the `ANativeActivity_onCreate`
/// bridge and spawns this function on a dedicated thread.
///
/// # Panics
/// Panics if the `wgpu` surface cannot be created — that indicates a
/// misconfigured device (no Vulkan support), which is not something the app
/// can recover from.
#[unsafe(no_mangle)]
pub fn android_main(app: AndroidApp) {
    android_logger::init_once(
        android_logger::Config::default().with_max_level(log::LevelFilter::Info),
    );
    log::info!("rust-mobile-demo android_main starting");

    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        event_loop_builder: Some(Box::new(move |builder| {
            use winit::platform::android::EventLoopBuilderExtAndroid;
            builder.with_android_app(app);
        })),
        ..Default::default()
    };
    if let Err(err) = eframe::run_native(
        "rust-mobile-demo",
        options,
        Box::new(|_cc| Ok(Box::new(DemoApp::new()))),
    ) {
        log::error!("eframe exited with error: {err:?}");
    }
}

/// UI-facing projection of `OwnedSignInAccount`. The Rust-side handle
/// (`NativeHandle<Credential>`) is intentionally dropped once we build
/// this — the native side keeps the credential registered under the
/// original id until the next `sign_out`.
#[derive(Debug, Clone)]
struct AccountView {
    id: String,
    email: Option<String>,
    display_name: Option<String>,
    photo_url: Option<String>,
    granted_scopes: Vec<String>,
    id_token_preview: String,
}

impl From<&OwnedSignInAccount> for AccountView {
    fn from(a: &OwnedSignInAccount) -> Self {
        // Trim the JWT to a preview — the full token can be thousands of
        // characters and belongs in a network call, not on screen.
        let id_token_preview = if a.id_token.len() > 42 {
            format!("{}…", &a.id_token[..42])
        } else {
            a.id_token.clone()
        };
        Self {
            id: a.id.clone(),
            email: a.email.clone(),
            display_name: a.display_name.clone(),
            photo_url: a.photo_url.clone(),
            granted_scopes: a.granted_scopes.clone(),
            id_token_preview,
        }
    }
}

#[derive(Debug, Clone)]
enum Status {
    Idle,
    Working(String),
    SignedIn(Arc<AccountView>),
    Err(String),
}

struct DemoApp {
    status: Arc<Mutex<Status>>,
}

impl DemoApp {
    fn new() -> Self {
        Self {
            status: Arc::new(Mutex::new(Status::Idle)),
        }
    }

    fn set_status(status: &Arc<Mutex<Status>>, ctx: &egui::Context, next: Status) {
        {
            let mut guard = status.lock().expect("status mutex");
            *guard = next;
        }
        ctx.request_repaint();
    }

    fn start_sign_in(&self, ctx: &egui::Context) {
        let status = self.status.clone();
        let ctx = ctx.clone();
        Self::set_status(&status, &ctx, Status::Working("Requesting…".to_owned()));
        std::thread::spawn(move || {
            let outcome = pollster::block_on(run_sign_in_flow(&status, &ctx));
            let next = match outcome {
                Ok(account) => Status::SignedIn(Arc::new(account)),
                Err(msg) => Status::Err(msg),
            };
            Self::set_status(&status, &ctx, next);
        });
    }

    fn start_sign_out(&self, ctx: &egui::Context) {
        let status = self.status.clone();
        let ctx = ctx.clone();
        Self::set_status(&status, &ctx, Status::Working("Signing out…".to_owned()));
        std::thread::spawn(move || {
            let outcome = pollster::block_on(run_sign_out_flow());
            let next = match outcome {
                Ok(()) => Status::Idle,
                Err(msg) => Status::Err(msg),
            };
            Self::set_status(&status, &ctx, next);
        });
    }
}

impl eframe::App for DemoApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Snapshot state under the mutex, then render — never hold the
        // lock across egui calls.
        let snapshot = self.status.lock().expect("status mutex").clone();

        egui::TopBottomPanel::top("hdr").show(ctx, |ui| {
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                ui.add_space(16.0);
                ui.heading("istmo demo");
                ui.add_space(8.0);
                ui.label(egui::RichText::new("· Full Rust · egui").weak());
            });
            ui.add_space(12.0);
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(16.0);
            match &snapshot {
                Status::SignedIn(account) => {
                    self.render_signed_in(ui, ctx, account);
                }
                _ => {
                    self.render_signed_out(ui, ctx, &snapshot);
                }
            }
        });
    }
}

impl DemoApp {
    fn render_signed_out(&self, ui: &mut egui::Ui, ctx: &egui::Context, status: &Status) {
        let busy = matches!(status, Status::Working(_));
        ui.vertical_centered(|ui| {
            ui.add_space(24.0);
            let button = egui::Button::new(
                egui::RichText::new("Sign in with Google").size(18.0),
            )
            .min_size(egui::vec2(240.0, 56.0));
            if ui.add_enabled(!busy, button).clicked() {
                self.start_sign_in(ctx);
            }

            ui.add_space(16.0);
            match status {
                Status::Idle => {
                    ui.label(
                        egui::RichText::new(
                            "Tap to sign in and receive a welcome notification.",
                        )
                        .weak(),
                    );
                }
                Status::Working(msg) => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(msg);
                    });
                }
                Status::Err(msg) => {
                    ui.colored_label(egui::Color32::from_rgb(220, 90, 90), msg);
                }
                Status::SignedIn(_) => {}
            }
        });
    }

    fn render_signed_in(
        &self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        account: &AccountView,
    ) {
        egui::Frame::group(ui.style())
            .fill(ui.visuals().extreme_bg_color)
            .inner_margin(egui::Margin::same(16.0))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let name = account
                        .display_name
                        .as_deref()
                        .unwrap_or_else(|| account.email.as_deref().unwrap_or(&account.id));
                    ui.heading(name);
                });
                ui.add_space(8.0);
                field(ui, "id", &account.id);
                if let Some(email) = &account.email {
                    field(ui, "email", email);
                }
                if let Some(display) = &account.display_name {
                    field(ui, "name", display);
                }
                if let Some(url) = &account.photo_url {
                    field(ui, "avatar", url);
                }
                field(ui, "scopes", &account.granted_scopes.join(", "));
                field(ui, "id_token", &account.id_token_preview);
            });

        ui.add_space(16.0);
        ui.vertical_centered(|ui| {
            if ui
                .add(
                    egui::Button::new(egui::RichText::new("Sign out").size(16.0))
                        .min_size(egui::vec2(180.0, 44.0)),
                )
                .clicked()
            {
                self.start_sign_out(ctx);
            }
        });
    }
}

fn field(ui: &mut egui::Ui, key: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(format!("{key}:"))
                .strong()
                .monospace(),
        );
        ui.add_space(4.0);
        ui.label(egui::RichText::new(value).monospace());
    });
    ui.add_space(2.0);
}

async fn run_sign_in_flow(
    status: &Arc<Mutex<Status>>,
    ctx: &egui::Context,
) -> Result<AccountView, String> {
    DemoApp::set_status(
        status,
        ctx,
        Status::Working("Requesting notification permission…".to_owned()),
    );
    let permissions = PermissionsClient::acquire().map_err(|e| format!("permissions: {e}"))?;
    let outcomes = permissions
        .request(vec![POST_NOTIFICATIONS.to_owned()])
        .await
        .map_err(|e| format!("permissions request: {e}"))?;
    log::info!("permission outcomes: {outcomes:?}");

    if SERVER_CLIENT_ID.starts_with("REPLACE_") {
        return Err(
            "SERVER_CLIENT_ID is still the placeholder — edit src/android.rs, \
             plug in the OAuth client id issued to your backend in Google Cloud Console, \
             then rebuild."
                .to_owned(),
        );
    }

    DemoApp::set_status(
        status,
        ctx,
        Status::Working("Signing in with Google…".to_owned()),
    );
    let config = SignInConfig::builder(SERVER_CLIENT_ID)
        .scope("openid")
        .scope("email")
        .scope("profile")
        .build();
    let client = SignInClient::acquire_with(config)
        .await
        .map_err(|e| format!("sign_in acquire: {}", render_sign_in_error(&e)))?;
    let account = client
        .sign_in_owned(SignInMode::Interactive)
        .await
        .map_err(|e| format!("sign_in: {}", render_sign_in_error(&e)))?;
    log::info!(
        "signed in id={} email={:?} display={:?} scopes={:?}",
        account.id,
        account.email,
        account.display_name,
        account.granted_scopes,
    );
    let view = AccountView::from(&account);

    DemoApp::set_status(
        status,
        ctx,
        Status::Working("Posting notification…".to_owned()),
    );
    let notifications = NotificationsClient::acquire().map_err(|e| format!("notifications: {e}"))?;
    notifications
        .schedule(welcome_notification(&account))
        .await
        .map_err(|e| format!("notifications: {}", render_notification_error(&e)))?;

    Ok(view)
}

async fn run_sign_out_flow() -> Result<(), String> {
    let config = SignInConfig::builder(SERVER_CLIENT_ID)
        .scope("openid")
        .scope("email")
        .scope("profile")
        .build();
    let client = SignInClient::acquire_with(config)
        .await
        .map_err(|e| format!("sign_out acquire: {}", render_sign_in_error(&e)))?;
    client
        .sign_out()
        .await
        .map_err(|e| format!("sign_out: {}", render_sign_in_error(&e)))?;
    log::info!("signed out");
    Ok(())
}

/// Turn an `IstmoError::PluginError { bytes }` from the sign-in plugin into
/// a human-readable string. Every other `IstmoError` variant falls back to
/// its `Display` impl. Transport / infra errors are already actionable
/// without extra decoding.
fn render_sign_in_error(err: &IstmoError) -> String {
    if let IstmoError::PluginError { bytes } = err {
        return match istmo::codec::decode::<SignInError>(bytes) {
            Ok((decoded, _)) => format!("{decoded}"),
            Err(codec_err) => format!(
                "undecodable domain error ({} bytes): {codec_err}",
                bytes.len(),
            ),
        };
    }
    err.to_string()
}

fn render_notification_error(err: &IstmoError) -> String {
    if let IstmoError::PluginError { bytes } = err {
        return match istmo::codec::decode::<NotificationError>(bytes) {
            Ok((decoded, _)) => format!("{decoded}"),
            Err(codec_err) => format!(
                "undecodable domain error ({} bytes): {codec_err}",
                bytes.len(),
            ),
        };
    }
    err.to_string()
}

fn welcome_notification(account: &OwnedSignInAccount) -> NotificationRequest {
    let who = account
        .display_name
        .as_deref()
        .or(account.email.as_deref())
        .unwrap_or(&account.id);
    NotificationRequest {
        title: "Signed in".to_owned(),
        body: format!("Welcome back, {who}!"),
        channel_id: "auth".to_owned(),
        importance: NotificationImportance::Default,
        delay_seconds: None,
        tag: Some("sign-in-welcome".to_owned()),
    }
}
