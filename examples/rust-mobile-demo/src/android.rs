//! Android entry point + egui view for the full-Rust demo.
//!
//! Layout:
//!
//! * [`android_main`] — invoked by `android-activity` after the NDK glue
//!   thread starts. Installs the tracing bridge, then hands the
//!   `AndroidApp` to eframe with the `wgpu` backend.
//! * [`DemoApp`] — `eframe::App` implementation. One button, one status
//!   line, one worker thread (`std::thread::spawn`) that runs the plugin
//!   chain via `pollster::block_on`.
//!
//! Kotlin side is expected to have called `IstmoRuntime.start()` in its
//! `MainActivity.onCreate` — the runtime is a `OnceLock` so double-init
//! is a soft error, and having Kotlin start it first means the pump is
//! draining before `android_main` fires.

use std::sync::{Arc, Mutex};

use android_activity::AndroidApp;
use eframe::egui;
use istmo::plugins::{
    NotificationImportance, NotificationRequest, NotificationsClient, PermissionsClient,
};
use istmo_plugins::google_sign_in::{OwnedSignInAccount, SignInClient, SignInConfig, SignInMode};

/// OAuth server client id for the demo. Real apps embed the id issued by
/// Google Cloud Console for the *backend* — the audience the id-token
/// must match. Kept as a placeholder here; replace before shipping.
const SERVER_CLIENT_ID: &str = "REPLACE_WITH_YOUR_SERVER_CLIENT_ID.apps.googleusercontent.com";

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

#[derive(Debug, Clone)]
enum Status {
    Idle,
    Working(String),
    Ok(String),
    Err(String),
}

impl Status {
    fn label(&self) -> &str {
        match self {
            Self::Idle => "Tap to sign in and receive a welcome notification.",
            Self::Working(msg) | Self::Ok(msg) | Self::Err(msg) => msg,
        }
    }
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
}

impl eframe::App for DemoApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let busy = matches!(&*self.status.lock().expect("status mutex"), Status::Working(_));
        let label = self.status.lock().expect("status mutex").label().to_owned();

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(48.0);
                ui.heading("istmo demo");
                ui.add_space(8.0);
                ui.label("Full Rust · egui · Google Sign-In · Notifications");
                ui.add_space(48.0);

                let button = egui::Button::new(egui::RichText::new("Sign in with Google").size(18.0))
                    .min_size(egui::vec2(240.0, 56.0));
                if ui.add_enabled(!busy, button).clicked() {
                    let status = self.status.clone();
                    let ctx = ctx.clone();
                    Self::set_status(&status, &ctx, Status::Working("Requesting…".to_owned()));
                    std::thread::spawn(move || {
                        let outcome = pollster::block_on(run_sign_in_flow(&status, &ctx));
                        let next = match outcome {
                            Ok(msg) => Status::Ok(msg),
                            Err(msg) => Status::Err(msg),
                        };
                        Self::set_status(&status, &ctx, next);
                    });
                }

                ui.add_space(24.0);
                ui.label(label);
            });
        });
    }
}

async fn run_sign_in_flow(
    status: &Arc<Mutex<Status>>,
    ctx: &egui::Context,
) -> Result<String, String> {
    DemoApp::set_status(status, ctx, Status::Working("Requesting notification permission…".to_owned()));
    let permissions = PermissionsClient::acquire().map_err(|e| format!("permissions: {e}"))?;
    let outcomes = permissions
        .request(vec![POST_NOTIFICATIONS.to_owned()])
        .await
        .map_err(|e| format!("permissions request: {e}"))?;
    log::info!("permission outcomes: {outcomes:?}");

    DemoApp::set_status(status, ctx, Status::Working("Signing in with Google…".to_owned()));
    let config = SignInConfig::builder(SERVER_CLIENT_ID)
        .scope("openid")
        .scope("email")
        .scope("profile")
        .build();
    let client = SignInClient::acquire_with(config)
        .await
        .map_err(|e| format!("sign_in acquire: {e}"))?;
    let account = client
        .sign_in_owned(SignInMode::Interactive)
        .await
        .map_err(|e| format!("sign_in: {e}"))?;
    log::info!("signed in as {} <{:?}>", account.id, account.email);

    DemoApp::set_status(status, ctx, Status::Working("Posting notification…".to_owned()));
    let notifications = NotificationsClient::acquire().map_err(|e| format!("notifications: {e}"))?;
    notifications
        .schedule(welcome_notification(&account))
        .await
        .map_err(|e| format!("notifications: {e}"))?;

    Ok(format!(
        "Welcome, {}",
        account.display_name.as_deref().unwrap_or(&account.id),
    ))
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
