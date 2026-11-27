//! Web host: [`WebNotifications`] over the Notification API and the
//! plugin's service worker.
//!
//! * Shows through `ServiceWorkerRegistration.showNotification` when a
//!   service worker is active (buttons, pictures, taps while the page is
//!   closed, `getNotifications()` listing), else through
//!   `new Notification()` on the page (no buttons; Chrome on Android
//!   refuses it: mobile web needs the worker even to show).
//! * Same `tag` replaces in place; `renotify` alerts again, its absence
//!   (or `silent` where `renotify` is missing) keeps updates quiet.
//! * Schedules while the page runs: the core [`Engine`](crate::schedule::Engine)
//!   behind one `setTimeout` [`Wakeup`](crate::schedule::Wakeup), entries in
//!   `IndexedDB` in the core store format, [`crate::MissedPolicy`] applied
//!   when the page loads.
//! * Reply and choices are simulated as buttons that open the app
//!   ([`crate::NotificationResponse::input_requested`]); there is no
//!   in-page prompt.
//!
//! The service worker script (`web/istmo-notifications-sw.js` in the
//! crate, also [`crate::web_assets::SERVICE_WORKER_JS`]) must be served
//! by the app; see `ServiceWorker`.

mod caps;
mod images;
mod payload;
mod state;

#[cfg(target_arch = "wasm32")]
mod host;
#[cfg(target_arch = "wasm32")]
mod idb;
#[cfg(target_arch = "wasm32")]
mod js;
#[cfg(target_arch = "wasm32")]
mod timer;

#[cfg(target_arch = "wasm32")]
pub use host::{ServiceWorker, WebNotifications, WebNotificationsBuilder};
