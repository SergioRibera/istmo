//! First-party plugin surfaces bundled with [`istmo`](https://docs.rs/istmo).
//!
//! Plugins come in two shapes:
//!
//! - **Calls** — an `#[istmo::plugin]` trait with its generated client and
//!   host. The platform side implements the trait; Rust calls it.
//! - **Early-event streams** — the platform (or a Rust publisher) pushes
//!   snapshots or events onto an early-event channel and Rust reads them
//!   through [`LatestClient`] / [`EarlyStream`]. Desktop targets ship
//!   pure-Rust publishers (`*Publisher`) for most of these.
//!
//! # Modules
//!
//! Calls:
//!
//! - [`permissions`] — request and query runtime permissions.
//! - [`notifications`] — local user notifications.
//! - [`activity_results`] — Android `startActivityForResult` /
//!   `ActivityResultLauncher` equivalent.
//! - [`admob`] — Google `AdMob` banner / interstitial / rewarded ads, plus
//!   the [`BannerSlot`] driver.
//! - [`service`] — long-running background service control channel.
//! - [`worker`] — one-shot scheduled task hooks.
//! - [`task_scheduler`] — enqueue and cancel platform-scheduled tasks.
//!
//! Early-event streams:
//!
//! - [`lifecycle`] — app foreground / background / terminated events.
//! - [`deeplinks`] — inbound URL routing, including cold-start intents
//!   buffered as a queue.
//! - [`documents`] — files the OS asks the app to open, plus the desktop
//!   single-instance forwarding in `documents_publisher`.
//! - [`safe_area`] — safe-area insets published by the host UI layer.
//! - [`system_info`] — locale, time zone, formatting preferences, uptime
//!   and the device wall clock.
//! - [`device_info`] — model, OS, CPU, RAM and screen.
//! - [`capabilities`] — hardware feature flags.
//! - [`battery_state`] — charge level, charging and thermal state.
//! - [`network_state`] — reachability and link kind.
//! - [`storage_info`] — free and total space.
//! - [`theme_preference`] — light/dark and accessibility preferences.
//!
//! [`early`] holds the shared client, stream and publisher types.

#![doc(html_root_url = "https://docs.rs/istmo-plugins")]

pub mod activity_results;
pub mod admob;
pub mod battery_state;
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub mod battery_state_publisher;
pub mod capabilities;
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub mod capabilities_publisher;
pub mod deeplinks;
pub mod device_info;
pub mod documents;
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub mod documents_publisher;
pub mod early;
pub mod lifecycle;
pub mod network_state;
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub mod network_state_publisher;
pub mod notifications;
pub mod permissions;
pub mod safe_area;
pub mod service;
pub mod storage_info;
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub mod storage_info_publisher;
pub mod system_info;
pub mod task_scheduler;
pub mod theme_preference;
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub mod theme_preference_publisher;
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
pub use crate::battery_state::{
    BATTERY_STATE_CHANNEL, BATTERY_STATE_PLUGIN_ID, BatteryState, BatteryStateClient,
    BatteryStateStream, ChargingState, PowerSource, ThermalState,
};
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub use crate::battery_state_publisher::BatteryStatePublisher;
pub use crate::capabilities::{
    CAPABILITIES_CHANNEL, CAPABILITIES_PLUGIN_ID, Capabilities, CapabilitiesClient,
    CapabilitiesStream,
};
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub use crate::capabilities_publisher::CapabilitiesPublisher;
pub use crate::deeplinks::{
    DEEPLINKS_CHANNEL, DEEPLINKS_PLUGIN_ID, DEFAULT_DEEPLINKS_CAPACITY, DeepLink, DeepLinkStream,
    DeepLinks,
};
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub use crate::device_info::DeviceInfoPublisher;
pub use crate::device_info::{
    DEVICE_INFO_CHANNEL, DEVICE_INFO_PLUGIN_ID, DeviceInfo, DeviceInfoClient, DeviceInfoStream,
    FormFactor, ScreenInfo, rust_fallback as device_info_rust_fallback,
};
pub use crate::documents::{
    ActivationStream, DEFAULT_DOCUMENTS_CAPACITY, DOCUMENTS_ACTIVATION_CHANNEL, DOCUMENTS_CHANNEL,
    DOCUMENTS_PLUGIN_ID, DocumentLocation, DocumentOrigin, DocumentStream, Documents,
    InstanceActivation, OpenedDocument,
};
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub use crate::documents_publisher::{
    DocumentsError, DocumentsPublisher, InstanceKey, Startup, instance_key_from_build,
};
pub use crate::early::{EarlyStream, LatestClient, LatestPublisher, LatestValue};
pub use crate::lifecycle::{
    AppLifecycle, LIFECYCLE_CHANNEL, LIFECYCLE_PLUGIN_ID, LifecycleState, LifecycleStream,
};
pub use crate::network_state::{
    LinkKind, NETWORK_STATE_CHANNEL, NETWORK_STATE_PLUGIN_ID, NetworkState, NetworkStateClient,
    NetworkStateStream,
};
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub use crate::network_state_publisher::NetworkStatePublisher;
pub use crate::notifications::{
    NOTIFICATIONS_PLUGIN_ID, NotificationError, NotificationHandle, NotificationImportance,
    NotificationRequest, Notifications, NotificationsClient, NotificationsHost,
};
pub use crate::permissions::{
    PERMISSIONS_PLUGIN_ID, PermissionOutcome, PermissionStatus, Permissions, PermissionsClient,
    PermissionsHost,
};
#[cfg(feature = "winit-publisher")]
pub use crate::safe_area::SafeAreaPublisher;
pub use crate::safe_area::{
    EdgeInsets, SAFE_AREA_CHANNEL, SAFE_AREA_PLUGIN_ID, SafeArea, SafeAreaInsets, SafeAreaStream,
};
pub use crate::service::{
    NotificationSpec, SERVICE_CONTROL_PLUGIN_ID, ServiceContext, ServiceControl,
    ServiceControlClient, ServiceControlError, ServiceControlHost, StopNotifier, WakeLock,
    WakelockToken, stop_channel,
};
pub use crate::storage_info::{
    ExternalMedium, ExternalStorage, STORAGE_INFO_CHANNEL, STORAGE_INFO_PLUGIN_ID, StorageInfo,
    StorageInfoClient, StorageInfoStream,
};
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub use crate::storage_info_publisher::StorageInfoPublisher;
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub use crate::system_info::SystemInfoPublisher;
pub use crate::system_info::{
    CalendarSystem, Formats, FormatsStream, LayoutDirection, Locale, LocaleStream,
    MeasurementSystem, SYSTEM_INFO_FORMATS_CHANNEL, SYSTEM_INFO_LOCALE_CHANNEL,
    SYSTEM_INFO_PLUGIN_ID, SYSTEM_INFO_TIMEZONE_CHANNEL, SystemInfoClient, TemperatureUnit,
    TimeZone, TimeZoneStream,
};
pub use crate::task_scheduler::{
    ExistingWorkPolicy, TASK_SCHEDULER_PLUGIN_ID, TaskHandle, TaskRequest, TaskScheduler,
    TaskSchedulerClient, TaskSchedulerError, TaskSchedulerHost,
};
pub use crate::theme_preference::{
    Color, ColorScheme, THEME_PREFERENCE_CHANNEL, THEME_PREFERENCE_PLUGIN_ID, ThemePreference,
    ThemePreferenceClient, ThemePreferenceStream,
};
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub use crate::theme_preference_publisher::ThemePreferencePublisher;
pub use crate::worker::{Constraints, NetworkKind, TaskOutcome, WorkerContext};
