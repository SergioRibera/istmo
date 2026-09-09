//! Core plugins bundled with the istmo framework.
//!
//! Milestone M3 lands four capabilities that every mobile app needs and that
//! every downstream plugin will lean on:
//!
//! * [`permissions`] — runtime permission checks and requests.
//! * [`lifecycle`] — foreground / background / low-memory transitions,
//!   surfaced through the runtime's [`LatestValueSlot`] so late subscribers
//!   immediately observe the current state.
//! * [`deeplinks`] — deep-link URIs, buffered through the runtime's
//!   [`PreMainQueue`] so links delivered before the app is ready are not
//!   dropped.
//! * [`activity_results`] — launch an intent (`Intent`, `UIActivityViewController`)
//!   and await its result.
//!
//! The plugin id namespace used on the wire is `istmo.<name>`; native backends
//! register handlers under those ids.
//!
//! [`LatestValueSlot`]: istmo_core::early_events::LatestValueSlot
//! [`PreMainQueue`]: istmo_core::early_events::PreMainQueue

pub mod activity_results;
pub mod admob;
pub mod deeplinks;
pub mod google_sign_in;
pub mod lifecycle;
pub mod notifications;
pub mod permissions;
pub mod safe_area;
pub mod service;
pub mod task_scheduler;
pub mod worker;

pub use crate::activity_results::{
    ACTIVITY_RESULTS_PLUGIN_ID, ActivityLaunchError, ActivityOutcome, ActivityResult,
    ActivityResults, ActivityResultsClient, ActivityResultsHost, ExtraValue, IntentRequest,
};
pub use crate::admob::{
    ADMOB_PLUGIN_ID, AdError, AdMob, AdMobClient, AdMobConfig, AdMobHost, Banner, BannerRect,
    BannerRequest, Interstitial, InterstitialOutcome, Rewarded, RewardedOutcome,
};
pub use crate::admob::slot::{
    BannerSlot, BoxFuture as BannerSlotBoxFuture, SlotStatus, SlotTarget, SpawnFn as BannerSlotSpawnFn,
    banner_rect_from_logical,
};
pub use crate::deeplinks::{DEEPLINKS_CHANNEL, DeepLink, DeepLinkStream, DeepLinks};
pub use crate::google_sign_in::{
    Credential, GOOGLE_SIGN_IN_PLUGIN_ID, OwnedSignInAccount, SignIn, SignInAccount, SignInClient,
    SignInConfig, SignInConfigBuilder, SignInError, SignInHost, SignInMode,
};
pub use crate::lifecycle::{AppLifecycle, LIFECYCLE_CHANNEL, LifecycleState, LifecycleStream};
pub use crate::notifications::{
    NOTIFICATIONS_PLUGIN_ID, NotificationError, NotificationHandle, NotificationImportance,
    NotificationRequest, Notifications, NotificationsClient, NotificationsHost,
};
pub use crate::permissions::{
    PERMISSIONS_PLUGIN_ID, PermissionOutcome, PermissionStatus, Permissions, PermissionsClient,
    PermissionsHost,
};
pub use crate::safe_area::{EdgeInsets, SAFE_AREA_CHANNEL, SafeArea, SafeAreaInsets, SafeAreaStream};
pub use crate::service::{
    NotificationSpec, SERVICE_CONTROL_PLUGIN_ID, ServiceContext, ServiceControl,
    ServiceControlClient, ServiceControlError, ServiceControlHost, StopNotifier, WakeLock,
    WakelockToken, stop_channel,
};
pub use crate::task_scheduler::{
    ExistingWorkPolicy, TASK_SCHEDULER_PLUGIN_ID, TaskHandle, TaskRequest, TaskScheduler,
    TaskSchedulerClient, TaskSchedulerError, TaskSchedulerHost,
};
pub use crate::worker::{Constraints, NetworkKind, TaskOutcome, WorkerContext};
