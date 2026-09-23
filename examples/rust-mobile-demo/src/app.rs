use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use eframe::egui;
use istmo::IstmoError;
#[cfg(any(
    target_os = "android",
    target_os = "ios",
    target_os = "tvos",
    target_os = "visionos",
))]
use istmo::plugins::safe_area::SafeAreaPublisher;
use istmo::plugins::{
    BannerSlot, EdgeInsets, NotificationError, NotificationImportance, NotificationRequest,
    NotificationsClient, PermissionsClient, SafeArea, SafeAreaInsets, SlotTarget,
    banner_rect_from_logical,
};
use istmo_google_sign_in::{
    OwnedSignInAccount, SignInClient, SignInConfig, SignInError, SignInMode,
};
use istmo_plugins::admob::{
    AdError, AdMobClient, AdMobConfig, InterstitialOutcome, RewardedOutcome,
};

const SERVER_CLIENT_ID: &str =
    "848096709714-9le17umoe085dtcmrout03qdbcp8cpi7.apps.googleusercontent.com";

const POST_NOTIFICATIONS: &str = "android.permission.POST_NOTIFICATIONS";

const ADMOB_APP_ID: &str = "ca-app-pub-1842517361828817~4357161875";

const INTERSTITIAL_UNIT: &str = "ca-app-pub-3940256099942544/1033173712";
const BANNER_UNIT: &str = "ca-app-pub-3940256099942544/6300978111";
const REWARDED_UNIT: &str = "ca-app-pub-3940256099942544/5224354917";

#[istmo::mobile_app]
pub(crate) fn main() {
    #[cfg(target_os = "android")]
    android_logger::init_once(
        android_logger::Config::default().with_max_level(log::LevelFilter::Info),
    );
    log::info!("rust-mobile-demo entry point running");

    #[cfg(target_os = "android")]
    let event_loop_builder: Option<
        Box<dyn FnOnce(&mut winit::event_loop::EventLoopBuilder<eframe::UserEvent>)>,
    > = {
        let app = istmo::android::android_app()
            .expect("android_main should have stashed the AndroidApp handle before entering main");
        Some(Box::new(move |builder| {
            use winit::platform::android::EventLoopBuilderExtAndroid;
            builder.with_android_app(app);
        }))
    };
    #[cfg(not(target_os = "android"))]
    let event_loop_builder: Option<
        Box<dyn FnOnce(&mut winit::event_loop::EventLoopBuilder<eframe::UserEvent>)>,
    > = None;

    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        event_loop_builder,
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

#[derive(Debug, Clone, Default)]
enum AdStatus {
    #[default]
    Idle,
    Working(String),
    Ok(String),
    Err(String),
}

struct DemoApp {
    status: Arc<Mutex<Status>>,
    ad_status: Arc<Mutex<AdStatus>>,

    admob: Arc<Mutex<Option<Arc<AdMobClient>>>>,

    admob_acquiring: Arc<AtomicBool>,

    banners: Vec<BannerSlot>,

    safe_area: Option<SafeArea>,

    #[cfg(any(
        target_os = "android",
        target_os = "ios",
        target_os = "tvos",
        target_os = "visionos",
    ))]
    safe_area_publisher: Option<SafeAreaPublisher>,

    feed: Vec<FeedItem>,
}

impl DemoApp {
    fn new() -> Self {
        Self {
            status: Arc::new(Mutex::new(Status::Idle)),
            ad_status: Arc::new(Mutex::new(AdStatus::Idle)),
            admob: Arc::new(Mutex::new(None)),
            admob_acquiring: Arc::new(AtomicBool::new(false)),
            banners: Vec::new(),
            safe_area: None,
            #[cfg(any(
                target_os = "android",
                target_os = "ios",
                target_os = "tvos",
                target_os = "visionos",
            ))]
            safe_area_publisher: None,
            feed: build_feed(60),
        }
    }

    fn ensure_admob(&self, ctx: &egui::Context) -> Option<Arc<AdMobClient>> {
        {
            let guard = self.admob.lock().expect("admob mutex");
            if let Some(c) = guard.as_ref() {
                return Some(c.clone());
            }
        }

        if self
            .admob_acquiring
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return None;
        }
        let admob = self.admob.clone();
        let latch = self.admob_acquiring.clone();
        let ctx_clone = ctx.clone();
        std::thread::spawn(move || {
            let cfg = AdMobConfig {
                app_id: ADMOB_APP_ID.to_owned(),
                test_device_ids: vec!["2902b757-08af-4ae3-b03e-b7e8a0104645".to_owned()],
                child_directed_treatment: false,
            };
            match pollster::block_on(AdMobClient::acquire_with(cfg)) {
                Ok(client) => {
                    let mut guard = admob.lock().expect("admob mutex");
                    *guard = Some(Arc::new(client));
                    ctx_clone.request_repaint();
                }
                Err(err) => {
                    log::warn!("admob acquire failed: {err}");

                    latch.store(false, Ordering::SeqCst);
                }
            }
        });
        None
    }

    fn ensure_safe_area(&mut self, ctx: &egui::Context) -> Option<SafeAreaInsets> {
        if self.safe_area.is_none() {
            match SafeArea::acquire() {
                Ok(sa) => {
                    let stream = sa.stream();
                    let ctx_clone = ctx.clone();
                    std::thread::spawn(move || {
                        while let Ok(_insets) = stream.recv() {
                            ctx_clone.request_repaint();
                        }
                    });
                    self.safe_area = Some(sa);
                }
                Err(err) => {
                    log::debug!("safe_area not ready: {err}");
                    return None;
                }
            }
        }
        self.safe_area
            .as_ref()
            .and_then(|s| s.current().ok().flatten())
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

const FALLBACK_TOP_INSET_PT: f32 = 24.0;
const FALLBACK_BOTTOM_INSET_PT: f32 = 0.0;

const HORIZONTAL_INSET: f32 = 12.0;

fn safe_area_margin(insets: Option<SafeAreaInsets>) -> egui::Margin {
    let padding: EdgeInsets = insets
        .map(SafeAreaInsets::view_padding)
        .unwrap_or(EdgeInsets {
            top: FALLBACK_TOP_INSET_PT,
            right: 0.0,
            bottom: FALLBACK_BOTTOM_INSET_PT,
            left: 0.0,
        });
    egui::Margin {
        top: padding.top,
        right: padding.right,
        bottom: padding.bottom,
        left: padding.left,
    }
}

impl eframe::App for DemoApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        #[cfg(any(
            target_os = "android",
            target_os = "ios",
            target_os = "tvos",
            target_os = "visionos",
        ))]
        {
            if self.safe_area_publisher.is_none() {
                if let Ok(rt) = istmo::Runtime::global() {
                    if let Ok(p) = SafeAreaPublisher::install(&rt) {
                        self.safe_area_publisher = Some(p);
                    }
                }
            }
            if let Some(publisher) = self.safe_area_publisher.as_mut() {
                publisher.tick();
            }
        }

        let snapshot = self.status.lock().expect("status mutex").clone();
        let insets = self.ensure_safe_area(ctx);
        let safe = safe_area_margin(insets);

        egui::TopBottomPanel::top("hdr")
            .frame(
                egui::Frame::default()
                    .fill(ctx.style().visuals.panel_fill)
                    .inner_margin(egui::Margin {
                        left: HORIZONTAL_INSET + safe.left,
                        right: HORIZONTAL_INSET + safe.right,
                        top: safe.top + 8.0,
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
                    .inner_margin(egui::Margin {
                        left: HORIZONTAL_INSET + safe.left,
                        right: HORIZONTAL_INSET + safe.right,
                        top: 12.0,
                        bottom: 12.0 + safe.bottom,
                    }),
            )
            .show(ctx, |ui| {
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

    fn render_signed_in(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, account: &AccountView) {
        let admob = self.ensure_admob(ctx);

        card(ui, |ui| render_account(ui, account));
        ui.add_space(12.0);

        self.render_ads_actions_card(ui, ctx);
        ui.add_space(12.0);

        const BANNER_EVERY: usize = 5;
        let px = ctx.pixels_per_point();
        let clip = ui.clip_rect();
        let feed_len = self.feed.len();
        for i in 0..feed_len {
            {
                let item = &self.feed[i];
                card(ui, |ui| render_feed_item(ui, item));
            }
            ui.add_space(8.0);
            if (i + 1) % BANNER_EVERY == 0 {
                let banner_idx = i / BANNER_EVERY;
                self.render_banner_row(ui, banner_idx, admob.as_ref(), clip, px);
                ui.add_space(8.0);
            }
        }

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
        ui.add_space(24.0);
    }

    fn render_ads_actions_card(&self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let ad_snapshot = self.ad_status.lock().expect("ad status mutex").clone();
        let busy = matches!(&ad_snapshot, AdStatus::Working(_));

        card(ui, |ui| {
            ui.heading("Ads");
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new("Interstitial + rewarded · banners auto-appear in the feed")
                    .weak(),
            );
            ui.add_space(10.0);

            let inner_width = ui.available_width();
            let spacing = ui.spacing().item_spacing.x;
            let per_btn = ((inner_width - spacing) / 2.0).max(96.0);
            let btn_size = egui::vec2(per_btn, 40.0);

            ui.horizontal_wrapped(|ui| {
                let interstitial = egui::Button::new("Interstitial").min_size(btn_size);
                if ui.add_enabled(!busy, interstitial).clicked() {
                    self.start_interstitial(ctx);
                }
                let rewarded = egui::Button::new("Rewarded").min_size(btn_size);
                if ui.add_enabled(!busy, rewarded).clicked() {
                    self.start_rewarded(ctx);
                }
            });

            ui.add_space(10.0);
            match &ad_snapshot {
                AdStatus::Idle => {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new("Scroll down to see banners appear inline.").weak(),
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
                            egui::RichText::new(msg).color(egui::Color32::from_rgb(220, 90, 90)),
                        )
                        .wrap(),
                    );
                }
            }
        });
    }

    fn render_banner_row(
        &mut self,
        ui: &mut egui::Ui,
        banner_idx: usize,
        admob: Option<&Arc<AdMobClient>>,
        clip: egui::Rect,
        px: f32,
    ) {
        let width = ui.available_width();
        let size = egui::vec2(width, 60.0);
        let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());

        let visuals = ui.visuals().clone();
        ui.painter()
            .rect_filled(rect, 8.0, visuals.extreme_bg_color);
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "· ad ·",
            egui::TextStyle::Small.resolve(ui.style()),
            visuals.weak_text_color(),
        );

        let Some(client) = admob else {
            return;
        };
        while self.banners.len() <= banner_idx {
            self.banners
                .push(BannerSlot::new(BANNER_UNIT, client.clone()));
        }

        let target = if clip.intersects(rect) {
            let br =
                banner_rect_from_logical(rect.min.x, rect.min.y, rect.width(), rect.height(), px);
            SlotTarget::Show(br)
        } else {
            SlotTarget::Hide
        };
        self.banners[banner_idx].sync(target);

        if let Some(err) = self.banners[banner_idx].last_error() {
            let text = format!("banner failed: {err}");
            let font = egui::TextStyle::Small.resolve(ui.style());
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                text,
                font,
                egui::Color32::from_rgb(220, 90, 90),
            );
        }
    }
}

fn render_account(ui: &mut egui::Ui, account: &AccountView) {
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
    if let Some(url) = &account.photo_url {
        field(ui, "avatar", url);
    }
    field(ui, "scopes", &account.granted_scopes.join(", "));
    field(ui, "id_token", &account.id_token_preview);
}

fn render_feed_item(ui: &mut egui::Ui, item: &FeedItem) {
    ui.horizontal(|ui| {
        ui.add(egui::Label::new(egui::RichText::new(&item.author).strong().monospace()).truncate());
        ui.add_space(6.0);
        ui.add(
            egui::Label::new(
                egui::RichText::new(format!("· {}m", item.minutes_ago))
                    .weak()
                    .monospace(),
            )
            .truncate(),
        );
    });
    ui.add_space(4.0);
    ui.add(egui::Label::new(egui::RichText::new(&item.title).size(15.0).strong()).wrap());
    ui.add_space(4.0);
    ui.add(egui::Label::new(&item.body).wrap());
}

#[derive(Debug, Clone)]
struct FeedItem {
    author: String,
    minutes_ago: u32,
    title: String,
    body: String,
}

fn build_feed(count: usize) -> Vec<FeedItem> {
    const AUTHORS: [&str; 8] = [
        "@sonia", "@mateo", "@brian", "@ana", "@leo", "@nina", "@omar", "@zoe",
    ];
    const TITLES: [&str; 8] = [
        "istmo hits M6 — full-Rust mobile is real",
        "wgpu on Android finally landed adaptive banners",
        "why we replaced Flutter with 400 lines of Rust",
        "shipping a workspace-scale Cargo build",
        "no more `serde` on the wire — bincode 2 is enough",
        "safe-area insets across every OEM",
        "PopupWindow: the one Android trick nobody remembered",
        "AdMob adaptive height without a layout jump",
    ];
    const BODIES: [&str; 8] = [
        "One trait per plugin, one struct per host, and the frame protocol carries the rest.",
        "Turns out `AdSize.getCurrentOrientationAnchoredAdaptiveBannerAdSize` is a whole sentence.",
        "The runtime crate is 900 lines. The client-side ergonomics come from macros.",
        "NativeHandle<T> plus a drop cascade removes an entire class of leaks.",
        "If your codec doesn't fit in Kotlin as a hundred lines, your codec is wrong.",
        "Kotlin publishes `WindowInsetsCompat` deltas over an early-event channel.",
        "SurfaceFlinger composites the popup as its own layer above the wgpu present.",
        "Fixing the y-axis meant treating banners as first-class citizens in the feed.",
    ];
    (0..count)
        .map(|i| FeedItem {
            author: AUTHORS[i % AUTHORS.len()].to_owned(),
            minutes_ago: (i as u32 * 11 % 240) + 1,
            title: TITLES[(i * 3 + 1) % TITLES.len()].to_owned(),
            body: BODIES[(i * 5 + 2) % BODIES.len()].to_owned(),
        })
        .collect()
}

fn card(ui: &mut egui::Ui, contents: impl FnOnce(&mut egui::Ui)) {
    let width = ui.available_width();
    ui.allocate_ui_with_layout(
        egui::vec2(width, 0.0),
        egui::Layout::top_down(egui::Align::Min),
        |ui| {
            egui::Frame::group(ui.style())
                .fill(ui.visuals().extreme_bg_color)
                .inner_margin(egui::Margin::same(12.0))
                .show(ui, contents);
        },
    );
}

fn field(ui: &mut egui::Ui, key: &str, value: &str) {
    const KEY_COL_WIDTH: f32 = 78.0;
    ui.horizontal(|ui| {
        ui.add_sized(
            egui::vec2(KEY_COL_WIDTH, 0.0),
            egui::Label::new(egui::RichText::new(format!("{key}:")).strong().monospace()),
        );

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
