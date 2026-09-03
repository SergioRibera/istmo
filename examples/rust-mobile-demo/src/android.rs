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
use istmo::NativeHandle;
use istmo::plugins::{
    NotificationError, NotificationImportance, NotificationRequest, NotificationsClient,
    PermissionsClient,
};
use istmo_plugins::admob::{
    AdError, AdMobClient, AdMobConfig, Banner, BannerRect, BannerRequest, InterstitialOutcome,
    RewardedOutcome,
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

// AdMob production ad units. Keep in sync with the app id registered
// under `com.google.android.gms.ads.APPLICATION_ID` in AndroidManifest.
const ADMOB_APP_ID: &str = "ca-app-pub-1842517361828817~4357161875";
const INTERSTITIAL_UNIT: &str = "ca-app-pub-1842517361828817/4741139402";
const BANNER_UNIT: &str = "ca-app-pub-1842517361828817/1751908784";
// Rewarded unit not provisioned yet — Google's canonical test rewarded
// unit stays until a real one lands. Mixing prod app id + a test ad
// unit id works (SDK does not enforce alignment) but keep an eye on
// AdMob policy strikes if this ships to production.
const REWARDED_UNIT: &str = "ca-app-pub-3940256099942544/5224354917";

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

/// Ephemeral ad message shown below the ad buttons — mirrors the sign-in
/// `Status` but scoped to ad actions so a rewarded-completion label
/// doesn't clobber the sign-in card headline.
#[derive(Debug, Clone, Default)]
enum AdStatus {
    #[default]
    Idle,
    Working(String),
    Ok(String),
    Err(String),
}

/// Whether the banner overlay is currently visible. When `true`, the
/// worker thread that opened it holds the handle alive (via the
/// [`DemoApp::banner_shown`] flag) so we don't need to keep the Rust
/// `NativeHandle<Banner>` in the App struct — the native side owns the
/// view lifetime and we command hide/show via the plugin.
struct DemoApp {
    status: Arc<Mutex<Status>>,
    ad_status: Arc<Mutex<AdStatus>>,
    /// Live banner handle. Present exactly when a banner is on-screen —
    /// the handle owns the native `AdView`'s lifetime. Toggle path
    /// takes it out to call `hide_banner_owned`.
    banner: Arc<Mutex<Option<NativeHandle<Banner>>>>,
}

impl DemoApp {
    fn new() -> Self {
        Self {
            status: Arc::new(Mutex::new(Status::Idle)),
            ad_status: Arc::new(Mutex::new(AdStatus::Idle)),
            banner: Arc::new(Mutex::new(None)),
        }
    }

    fn set_ad_status(status: &Arc<Mutex<AdStatus>>, ctx: &egui::Context, next: AdStatus) {
        {
            let mut guard = status.lock().expect("ad status mutex");
            *guard = next;
        }
        ctx.request_repaint();
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

    fn start_interstitial(&self, ctx: &egui::Context) {
        let status = self.ad_status.clone();
        let ctx = ctx.clone();
        Self::set_ad_status(
            &status,
            &ctx,
            AdStatus::Working("Loading interstitial…".into()),
        );
        std::thread::spawn(move || {
            let outcome = pollster::block_on(run_interstitial_flow(&status, &ctx));
            let next = match outcome {
                Ok(msg) => AdStatus::Ok(msg),
                Err(msg) => AdStatus::Err(msg),
            };
            Self::set_ad_status(&status, &ctx, next);
        });
    }

    fn start_rewarded(&self, ctx: &egui::Context) {
        let status = self.ad_status.clone();
        let ctx = ctx.clone();
        Self::set_ad_status(
            &status,
            &ctx,
            AdStatus::Working("Loading rewarded ad…".into()),
        );
        std::thread::spawn(move || {
            let outcome = pollster::block_on(run_rewarded_flow(&status, &ctx));
            let next = match outcome {
                Ok(msg) => AdStatus::Ok(msg),
                Err(msg) => AdStatus::Err(msg),
            };
            Self::set_ad_status(&status, &ctx, next);
        });
    }

    /// Toggle the banner overlay. When shown, we anchor it to the bottom
    /// of the current window (`ctx.screen_rect()`). Coordinates are
    /// physical pixels — `ctx.pixels_per_point()` translates from egui's
    /// logical units.
    fn start_toggle_banner(&self, ctx: &egui::Context) {
        let held = self.banner.lock().expect("banner mutex").take();
        let ad_status = self.ad_status.clone();
        let banner_slot = self.banner.clone();
        let ctx_clone = ctx.clone();

        if let Some(handle) = held {
            Self::set_ad_status(
                &ad_status,
                &ctx_clone,
                AdStatus::Working("Hiding banner…".into()),
            );
            std::thread::spawn(move || {
                let outcome = pollster::block_on(run_hide_banner(handle));
                let next = match outcome {
                    Ok(()) => AdStatus::Ok("Banner hidden.".into()),
                    Err(msg) => AdStatus::Err(msg),
                };
                Self::set_ad_status(&ad_status, &ctx_clone, next);
            });
        } else {
            // Anchor the banner at the bottom of the window, full width,
            // ~50 dp tall. Convert egui's logical-point rect into
            // physical pixels using the current pixels_per_point.
            let px = ctx.pixels_per_point();
            let screen = ctx.screen_rect();
            let banner_h_px = (50.0 * px) as u32;
            let width_px = (screen.width() * px) as u32;
            let x_px = 0u32;
            let y_px = ((screen.max.y * px) as u32).saturating_sub(banner_h_px);
            let rect = BannerRect {
                x: x_px,
                y: y_px,
                width: width_px,
                height: banner_h_px,
            };
            Self::set_ad_status(
                &ad_status,
                &ctx_clone,
                AdStatus::Working("Showing banner…".into()),
            );
            std::thread::spawn(move || {
                let outcome = pollster::block_on(run_show_banner(rect));
                let next = match outcome {
                    Ok(handle) => {
                        *banner_slot.lock().expect("banner mutex") = Some(handle);
                        AdStatus::Ok("Banner visible.".into())
                    }
                    Err(msg) => AdStatus::Err(msg),
                };
                Self::set_ad_status(&ad_status, &ctx_clone, next);
            });
        }
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

/// Vertical padding reserved above the header so the status bar does not
/// overlap our content. NativeActivity + egui draw edge-to-edge; the
/// Kotlin `WindowCompat.setDecorFitsSystemWindows(window, true)` helper
/// mitigates it on some devices, but MIUI (and other OEMs) still shove
/// content underneath the system bar. Reserving `28pt` matches the
/// stock Android status bar height across most modern phones.
const STATUS_BAR_INSET: f32 = 28.0;

/// Horizontal padding reserved on both sides of the content column so
/// cards do not clip against the left / right edges of the screen.
const HORIZONTAL_INSET: f32 = 12.0;

impl eframe::App for DemoApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Snapshot state under the mutex, then render — never hold the
        // lock across egui calls.
        let snapshot = self.status.lock().expect("status mutex").clone();

        egui::TopBottomPanel::top("hdr")
            .frame(
                egui::Frame::default()
                    .fill(ctx.style().visuals.panel_fill)
                    .inner_margin(egui::Margin {
                        left: HORIZONTAL_INSET,
                        right: HORIZONTAL_INSET,
                        top: STATUS_BAR_INSET + 8.0,
                        bottom: 10.0,
                    }),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.heading("istmo demo");
                    ui.add_space(8.0);
                    ui.label(egui::RichText::new("· Full Rust · egui").weak());
                });
            });

        egui::CentralPanel::default()
            .frame(
                egui::Frame::default()
                    .fill(ctx.style().visuals.panel_fill)
                    .inner_margin(egui::Margin::symmetric(HORIZONTAL_INSET, 12.0)),
            )
            .show(ctx, |ui| {
                // Cap content width to the panel width — every card
                // inside reads `ui.available_width()` when rendering,
                // and this bound prevents accidental horizontal
                // overflow when a very long label (like an id-token)
                // is dropped inside a `ui.horizontal` block.
                ui.set_max_width(ui.available_width());
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| match &snapshot {
                        Status::SignedIn(account) => {
                            self.render_signed_in(ui, ctx, account);
                        }
                        _ => {
                            self.render_signed_out(ui, ctx, &snapshot);
                        }
                    });
            });
    }
}

impl DemoApp {
    fn render_signed_out(&self, ui: &mut egui::Ui, ctx: &egui::Context, status: &Status) {
        let busy = matches!(status, Status::Working(_));
        ui.vertical_centered(|ui| {
            ui.add_space(24.0);
            let button = egui::Button::new(egui::RichText::new("Sign in with Google").size(18.0))
                .min_size(egui::vec2(240.0, 56.0));
            if ui.add_enabled(!busy, button).clicked() {
                self.start_sign_in(ctx);
            }

            ui.add_space(16.0);
            match status {
                Status::Idle => {
                    ui.label(
                        egui::RichText::new("Tap to sign in and receive a welcome notification.")
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

    fn render_signed_in(&self, ui: &mut egui::Ui, ctx: &egui::Context, account: &AccountView) {
        let card_width = ui.available_width();
        egui::Frame::group(ui.style())
            .fill(ui.visuals().extreme_bg_color)
            .inner_margin(egui::Margin::same(14.0))
            .show(ui, |ui| {
                ui.set_max_width(card_width);
                let name = account
                    .display_name
                    .as_deref()
                    .unwrap_or_else(|| account.email.as_deref().unwrap_or(&account.id));
                ui.add(egui::Label::new(egui::RichText::new(name).heading()).truncate());
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
        self.render_ads_panel(ui, ctx);

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

    fn render_ads_panel(&self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let ad_snapshot = self.ad_status.lock().expect("ad status mutex").clone();
        let busy = matches!(&ad_snapshot, AdStatus::Working(_));
        let banner_shown = self.banner.lock().expect("banner mutex").is_some();
        let card_width = ui.available_width();

        egui::Frame::group(ui.style())
            .fill(ui.visuals().extreme_bg_color)
            .inner_margin(egui::Margin::same(12.0))
            .show(ui, |ui| {
                ui.set_max_width(card_width);
                ui.heading("Ads");
                ui.add_space(6.0);
                ui.label(egui::RichText::new("AdMob · test units").weak());
                ui.add_space(10.0);

                // Each button gets a fair share of the row minus the
                // spacing between them. Below ~600pt wide (typical
                // phone), that lands under the min_size (120), so
                // horizontal_wrapped naturally splits into two rows.
                let spacing = ui.spacing().item_spacing.x;
                let btn_w = ((card_width - spacing * 2.0) / 3.0).max(120.0);
                let btn_size = egui::vec2(btn_w, 40.0);

                ui.horizontal_wrapped(|ui| {
                    let interstitial = egui::Button::new("Interstitial").min_size(btn_size);
                    if ui.add_enabled(!busy, interstitial).clicked() {
                        self.start_interstitial(ctx);
                    }
                    let rewarded = egui::Button::new("Rewarded").min_size(btn_size);
                    if ui.add_enabled(!busy, rewarded).clicked() {
                        self.start_rewarded(ctx);
                    }
                    let banner_label = if banner_shown { "Hide banner" } else { "Banner" };
                    let banner = egui::Button::new(banner_label).min_size(btn_size);
                    if ui.add_enabled(!busy, banner).clicked() {
                        self.start_toggle_banner(ctx);
                    }
                });

                ui.add_space(10.0);
                match &ad_snapshot {
                    AdStatus::Idle => {
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new("Tap a button to try an ad.").weak(),
                            )
                            .wrap(),
                        );
                    }
                    AdStatus::Working(msg) => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.add(egui::Label::new(msg).wrap());
                        });
                    }
                    AdStatus::Ok(msg) => {
                        ui.add(egui::Label::new(egui::RichText::new(msg).strong()).wrap());
                    }
                    AdStatus::Err(msg) => {
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(msg)
                                    .color(egui::Color32::from_rgb(220, 90, 90)),
                            )
                            .wrap(),
                        );
                    }
                }
            });
    }
}

fn field(ui: &mut egui::Ui, key: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.add(
            egui::Label::new(egui::RichText::new(format!("{key}:")).strong().monospace())
                .truncate(),
        );
        ui.add_space(4.0);
        // Truncate over wrap for long values (URLs, JWTs). Wrapping
        // an id_token turns the card into ten lines of ugliness; a
        // trailing ellipsis reads as "there is more, tap to copy in
        // a future revision".
        ui.add(egui::Label::new(egui::RichText::new(value).monospace()).truncate());
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
    let notifications =
        NotificationsClient::acquire().map_err(|e| format!("notifications: {e}"))?;
    notifications
        .schedule(welcome_notification(&account))
        .await
        .map_err(|e| format!("notifications: {}", render_notification_error(&e)))?;

    Ok(view)
}

async fn run_interstitial_flow(
    status: &Arc<Mutex<AdStatus>>,
    ctx: &egui::Context,
) -> Result<String, String> {
    let client = acquire_admob().await?;
    DemoApp::set_ad_status(
        status,
        ctx,
        AdStatus::Working("Showing interstitial…".into()),
    );
    let ad = client
        .load_interstitial_owned(INTERSTITIAL_UNIT.to_owned())
        .await
        .map_err(|e| format!("load: {}", render_ad_error(&e)))?;
    let outcome = client
        .show_interstitial_owned(ad)
        .await
        .map_err(|e| format!("show: {}", render_ad_error(&e)))?;
    Ok(match outcome {
        InterstitialOutcome::Dismissed => "Interstitial dismissed by user.".to_owned(),
        InterstitialOutcome::FailedToShow => "Interstitial failed to show.".to_owned(),
    })
}

async fn run_rewarded_flow(
    status: &Arc<Mutex<AdStatus>>,
    ctx: &egui::Context,
) -> Result<String, String> {
    let client = acquire_admob().await?;
    DemoApp::set_ad_status(
        status,
        ctx,
        AdStatus::Working("Showing rewarded ad…".into()),
    );
    let ad = client
        .load_rewarded_owned(REWARDED_UNIT.to_owned())
        .await
        .map_err(|e| format!("load: {}", render_ad_error(&e)))?;
    let outcome: RewardedOutcome = client
        .show_rewarded_owned(ad)
        .await
        .map_err(|e| format!("show: {}", render_ad_error(&e)))?;
    Ok(if outcome.granted {
        format!(
            "Reward: {} × {}",
            outcome.reward_amount, outcome.reward_type,
        )
    } else {
        "Rewarded ad closed without reward.".to_owned()
    })
}

async fn run_show_banner(rect: BannerRect) -> Result<NativeHandle<Banner>, String> {
    let client = acquire_admob().await?;
    client
        .show_banner_owned(BannerRequest {
            ad_unit_id: BANNER_UNIT.to_owned(),
            rect,
        })
        .await
        .map_err(|e| format!("show_banner: {}", render_ad_error(&e)))
}

async fn run_hide_banner(handle: NativeHandle<Banner>) -> Result<(), String> {
    let client = acquire_admob().await?;
    client
        .hide_banner_owned(handle)
        .await
        .map_err(|e| format!("hide_banner: {}", render_ad_error(&e)))
}

async fn acquire_admob() -> Result<AdMobClient, String> {
    let config = AdMobConfig {
        app_id: ADMOB_APP_ID.to_owned(),
        test_device_ids: vec!["2902b757-08af-4ae3-b03e-b7e8a0104645".to_owned()],
        child_directed_treatment: false,
    };
    AdMobClient::acquire_with(config)
        .await
        .map_err(|e| format!("admob acquire: {}", render_ad_error(&e)))
}

fn render_ad_error(err: &IstmoError) -> String {
    if let IstmoError::PluginError { bytes } = err {
        return match istmo::codec::decode::<AdError>(bytes) {
            Ok((decoded, _)) => format!("{decoded}"),
            Err(codec_err) => format!(
                "undecodable domain error ({} bytes): {codec_err}",
                bytes.len(),
            ),
        };
    }
    err.to_string()
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
