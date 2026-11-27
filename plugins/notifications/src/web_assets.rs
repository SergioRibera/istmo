//! The web host's service worker, for build scripts and dev servers that
//! serve it (every target, so a native `build.rs` can write it out).
//!
//! Contract with whoever serves it:
//!
//! * Serve [`SERVICE_WORKER_JS`] as [`SERVICE_WORKER_FILE`] next to the
//!   app's page (same origin, `Content-Type: text/javascript`), so its
//!   default scope covers the page. [`crate::WebNotifications`] registers
//!   it at that relative URL unless configured otherwise.
//! * Apps with their own service worker add
//!   `importScripts("istmo-notifications-sw.js")` to it instead and
//!   configure the host with `ServiceWorker::Existing`.
//! * The script and the page share the `IndexedDB` database [`DB_NAME`]
//!   (version [`DB_VERSION`]): object store [`SCHEDULE_STORE`] (one
//!   record per scheduled notification, keyed by its id, each a
//!   one-entry store in the core store format) and [`QUEUE_STORE`]
//!   (clicks and closes the worker queued). Both sides create both
//!   stores on upgrade and close their connection on `versionchange`.

/// File name the host registers by default.
pub const SERVICE_WORKER_FILE: &str = "istmo-notifications-sw.js";

/// The script (plain JavaScript, no dependencies).
pub const SERVICE_WORKER_JS: &str = include_str!("../web/istmo-notifications-sw.js");

/// `IndexedDB` database shared by the page and the worker.
pub const DB_NAME: &str = "istmo-notifications";

/// Version of [`DB_NAME`]; the worker script uses the same.
pub const DB_VERSION: u32 = 2;

/// Object store of the scheduled notifications.
pub const SCHEDULE_STORE: &str = "scheduled";

/// Object store of the worker's queue of clicks and closes.
pub const QUEUE_STORE: &str = "responses";

/// `BroadcastChannel` the page's tabs coordinate on (schedule changes,
/// the leader lease where Web Locks are missing).
pub const CHANNEL_NAME: &str = "istmo-notifications";

/// Web Lock held by the tab that runs the schedule.
pub const LOCK_NAME: &str = "istmo-notifications/scheduler";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_script_names_the_same_database() {
        assert!(SERVICE_WORKER_JS.contains(&format!("const DB_NAME = \"{DB_NAME}\";")));
        assert!(SERVICE_WORKER_JS.contains(&format!("const DB_VERSION = {DB_VERSION};")));
        assert!(SERVICE_WORKER_JS.contains(&format!("const SCHEDULE = \"{SCHEDULE_STORE}\";")));
        assert!(SERVICE_WORKER_JS.contains(&format!("const QUEUE = \"{QUEUE_STORE}\";")));
        assert!(SERVICE_WORKER_JS.contains("const DATA_VERSION = 1;"));
    }
}
