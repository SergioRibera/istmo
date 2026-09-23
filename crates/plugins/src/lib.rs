//! First-party plugin surfaces bundled with [`istmo`](https://docs.rs/istmo).
//!
//! Each submodule exposes a plugin trait, its generated client, and any
//! supporting message / config types. All plugins here are wire-only:
//! the Rust side ships the trait, and the platform side provides the
//! implementation.
//!
//! # Modules
//!
//! - [`lifecycle`] — app foreground / background / terminated events
//!   published as an early-event stream.
//! - [`deeplinks`] — inbound URL routing, including cold-start intents
//!   buffered as an early-event queue.
//! - [`permissions`] — request and query runtime permissions.
//! - [`notifications`] — local user notifications.
//! - [`activity_results`] — Android `startActivityForResult` /
//!   `ActivityResultLauncher` equivalent.
//! - [`safe_area`] — safe-area insets published by the host UI layer.
//! - [`admob`] — Google AdMob banner / interstitial / rewarded ads.
//! - [`service`] — long-running background service control channel.
//! - [`worker`] — one-shot scheduled task hooks.
//! - [`task_scheduler`] — enqueue and cancel platform-scheduled tasks.

#![doc(html_root_url = "https://docs.rs/istmo-plugins")]

pub mod activity_results;
pub mod admob;
pub mod deeplinks;
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
pub use crate::admob::slot::{
    BannerSlot, BoxFuture as BannerSlotBoxFuture, SlotStatus, SlotTarget,
    SpawnFn as BannerSlotSpawnFn, banner_rect_from_logical,
};
pub use crate::admob::{
    ADMOB_PLUGIN_ID, AdError, AdMob, AdMobClient, AdMobConfig, AdMobHost, Banner, BannerRect,
    BannerRequest, Interstitial, InterstitialOutcome, Rewarded, RewardedOutcome,
};
pub use crate::deeplinks::{DEEPLINKS_CHANNEL, DeepLink, DeepLinkStream, DeepLinks};
pub use crate::lifecycle::{AppLifecycle, LIFECYCLE_CHANNEL, LifecycleState, LifecycleStream};
pub use crate::notifications::{
    NOTIFICATIONS_PLUGIN_ID, NotificationError, NotificationHandle, NotificationImportance,
    NotificationRequest, Notifications, NotificationsClient, NotificationsHost,
};
pub use crate::permissions::{
    PERMISSIONS_PLUGIN_ID, PermissionOutcome, PermissionStatus, Permissions, PermissionsClient,
    PermissionsHost,
};
pub use crate::safe_area::{
    EdgeInsets, SAFE_AREA_CHANNEL, SafeArea, SafeAreaInsets, SafeAreaStream,
};
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
