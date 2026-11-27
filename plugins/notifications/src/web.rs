//! Web host boundary (not implemented yet).
//!
//! The web host shows notifications with the Notification API and the
//! service worker (`assets/web/istmo-notifications-sw.js`), schedules
//! in-process with [`crate::schedule::Engine`] behind a `setTimeout`
//! [`crate::schedule::Wakeup`], and keeps the store in IndexedDB. Until it
//! lands, web apps register [`crate::UnavailableNotifications`].
