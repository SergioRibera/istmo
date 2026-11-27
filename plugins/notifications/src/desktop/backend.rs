//! The contract between [`super::DesktopNotifications`] and a platform
//! backend (Linux fdo / portal, Windows toasts, macOS
//! `UNUserNotificationCenter`).
//!
//! The host does everything that is not the platform: validation,
//! channel defaults, [render fallbacks](crate::render), coalescing, the
//! in-process scheduler, the delivered table, image files, events,
//! responses. A backend only shows, replaces and removes what it is
//! handed, and reports back through its [`BackendSink`].
//!
//! Rules for implementations:
//!
//! * Methods are called from any thread, never with a host lock held,
//!   and may block (a D-Bus round trip, a `WinRT` call). Calls for one id
//!   ([`Backend::show`], [`Backend::remove`]) come one at a time, in the
//!   order the host issued them; calls for different ids may overlap.
//! * [`Backend::info`] is read once after the start and again after
//!   [`BackendSink::changed`]; the host caches it in between.
//! * [`BackendSink`] methods only queue the report and return: the host
//!   handles reports in order on its own report worker thread, never on
//!   the reporting thread. They may be called from any thread, from
//!   inside a backend method and from inside the start function; the
//!   handling may call the backend back (a snooze removes the
//!   notification) from the worker, so a backend dispatch thread that
//!   reports never waits on itself.
//! * Keys: a platform action carries the [`RenderedAction::key`] it was
//!   shown with; [`Activation::key`] hands it back unchanged.
//!
//! [`RenderedAction::key`]: crate::render::RenderedAction::key

use std::path::{Path, PathBuf};
use std::sync::Weak;

use crate::coalesce::Rate;
use crate::render::{RenderLimits, Rendered};
use crate::{
    AckToken, ActionId, Capabilities, ChannelId, ChannelSpec, DeliveredNotification, DismissReason,
    Feature, Notification, NotificationError, NotificationId, PermissionRequest, PermissionState,
    PermissionStatus,
};

/// What a backend reports about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendInfo {
    /// The wire report. The host adds its own simulations on top
    /// (scheduling while running, the delivered table, snooze, text
    /// progress, ...); report only what the platform itself does.
    pub capabilities: Capabilities,
    /// What the platform shows natively; the host simulates the rest.
    pub limits: RenderLimits,
    /// Posting rate for live updates.
    pub rate: Rate,
    /// [`Backend::delivered`] lists what is on screen; otherwise the host
    /// answers from its own table.
    pub lists_delivered: bool,
    /// The platform removes notifications after `timeout_ms` itself;
    /// otherwise the host removes them while it runs.
    pub timeouts: bool,
}

impl BackendInfo {
    /// A backend that only shows: three buttons, nothing else native.
    #[must_use]
    pub fn basic(backend: impl Into<String>) -> Self {
        let mut capabilities = Capabilities::none(backend);
        capabilities.max_actions = 3;
        capabilities.max_pending = None;
        capabilities.max_active = None;
        capabilities.features = [Feature::Show, Feature::Update, Feature::Actions]
            .into_iter()
            .map(|feature| crate::FeatureSupport {
                feature,
                support: crate::Support::Native,
            })
            .collect();
        Self {
            limits: RenderLimits::from_capabilities(&capabilities),
            capabilities,
            rate: Rate::LINUX,
            lists_delivered: false,
            timeouts: false,
        }
    }
}

/// Where an image of the shown notification is.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ImageSlot {
    /// [`Notification::icon`].
    Icon,
    /// The picture of [`crate::Style::Picture`].
    Picture,
    /// The avatar of the participant with this key (conversation user,
    /// message senders, the caller).
    Avatar(String),
    /// The image of message `n` of the conversation.
    Message(usize),
    /// The icon of an action.
    Action(ActionId),
}

/// Files of the images of a shown notification.
///
/// PNG / JPEG bytes are written to the plugin's cache directory (named
/// by content hash), [`crate::NotificationImage::File`] paths are used as
/// given. Platform names ([`crate::NotificationImage::Named`]) have no
/// file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImageFiles {
    pub(crate) entries: Vec<(ImageSlot, PathBuf)>,
}

impl ImageFiles {
    /// The file of `slot`.
    #[must_use]
    pub fn get(&self, slot: &ImageSlot) -> Option<&Path> {
        self.entries
            .iter()
            .find(|(have, _)| have == slot)
            .map(|(_, path)| path.as_path())
    }

    /// Whether it has no file.
    pub(crate) const fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Every slot and its file.
    pub fn iter(&self) -> impl Iterator<Item = (&ImageSlot, &Path)> {
        self.entries
            .iter()
            .map(|(slot, path)| (slot, path.as_path()))
    }
}

/// How a show relates to what is on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShowMode {
    /// Nothing with this id is shown.
    New,
    /// Replaces the one with this id in place.
    Replace {
        /// Alert (sound, banner) again; `false` = silent update.
        alert: bool,
    },
}

/// One notification to put on screen.
#[derive(Debug, Clone, Copy)]
pub struct Shown<'a> {
    /// Validated, with the channel's defaults merged in.
    pub notification: &'a Notification,
    /// Body with simulated lines, the buttons to show.
    pub rendered: &'a Rendered,
    /// Files of its images.
    pub images: &'a ImageFiles,
    /// New or replacing.
    pub mode: ShowMode,
}

/// A platform backend of [`super::DesktopNotifications`].
pub trait Backend: Send + Sync + 'static {
    /// What it does. Read once after the start and again after
    /// [`BackendSink::changed`].
    fn info(&self) -> BackendInfo;

    /// The permission state. Default: nothing to ask for.
    fn permission(&self) -> PermissionStatus {
        PermissionStatus::of(PermissionState::NotRequired)
    }

    /// Ask the user. Default: the current state.
    fn request_permission(
        &self,
        _request: &PermissionRequest,
    ) -> Result<PermissionStatus, NotificationError> {
        Ok(self.permission())
    }

    /// Open the system settings. Default: unsupported.
    fn open_settings(&self, _channel: Option<&ChannelId>) -> Result<(), NotificationError> {
        Err(NotificationError::Unsupported(Feature::OpenSettings))
    }

    /// Channels changed (the host merges their defaults itself).
    /// Default: nothing to do.
    fn set_channels(&self, _channels: &[ChannelSpec]) -> Result<(), NotificationError> {
        Ok(())
    }

    /// Put `shown` on screen, or replace the one with its id.
    fn show(&self, shown: &Shown<'_>) -> Result<(), NotificationError>;

    /// Take `id` off the screen. Unknown ids are not an error.
    fn remove(&self, id: &NotificationId) -> Result<(), NotificationError>;

    /// What is on screen, when the platform can list it
    /// ([`BackendInfo::lists_delivered`]). Default: none.
    fn delivered(&self) -> Result<Vec<DeliveredNotification>, NotificationError> {
        Err(NotificationError::Unsupported(Feature::ListDelivered))
    }

    /// The app finished a background response: release whatever keeps
    /// the process alive for it. Default: nothing to release.
    fn complete(&self, _ack: AckToken) {}
}

/// A platform activation: a tap, a button, a reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Activation {
    /// Which notification.
    pub id: NotificationId,
    /// The [`RenderedAction::key`](crate::render::RenderedAction::key) of
    /// the button; `None` for the notification itself.
    pub key: Option<String>,
    /// Text the platform handed over (inline reply, selected choice).
    pub reply: Option<String>,
    /// This activation started the process.
    pub launched_app: bool,
    /// User data and URL carried by the platform activation itself, used
    /// when the host has no record of the notification (shown by an
    /// earlier process).
    pub carried: Option<Carried>,
}

/// Fields a platform activation carries with it (Windows toast
/// arguments, portal targets).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Carried {
    /// [`Notification::user_data`].
    pub user_data: Option<String>,
    /// The URL of the action, else of the notification.
    pub url: Option<String>,
}

/// How a backend reports back to its host.
///
/// Cheap to clone; does nothing once the host is gone. Every method queues the report and returns at
/// once (see the module docs); [`super::DesktopNotifications::settle`]
/// waits until what was reported is handled.
#[derive(Clone)]
pub struct BackendSink {
    pub(crate) host: Weak<super::Inner>,
}

impl std::fmt::Debug for BackendSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BackendSink")
            .field("host", &(self.host.strong_count() > 0))
            .finish()
    }
}

impl BackendSink {
    /// A sink bound to no host (tests of a backend on its own).
    #[must_use]
    pub const fn detached() -> Self {
        Self { host: Weak::new() }
    }

    /// The user activated a notification.
    pub fn activated(&self, activation: Activation) {
        self.report(super::Report::Activated(activation));
    }

    /// A notification left the screen without a tap.
    pub fn dismissed(&self, id: &NotificationId, reason: DismissReason) {
        self.report(super::Report::Dismissed(id.clone(), reason));
    }

    /// The platform failed to show a notification after accepting it.
    pub fn failed(&self, id: Option<&NotificationId>, message: &str) {
        self.report(super::Report::Failed(id.cloned(), message.to_owned()));
    }

    /// What the backend reports changed (notification server restarted,
    /// permission changed): the host re-reads [`Backend::info`].
    pub fn changed(&self) {
        self.report(super::Report::Changed);
    }

    fn report(&self, report: super::Report) {
        if let Some(host) = self.host.upgrade() {
            host.report(report);
        }
    }
}

/// A backend that shows nothing and accepts everything: notifications
/// live only in the host's table. For tests and headless runs.
#[derive(Debug, Clone, Copy, Default)]
pub struct NullBackend;

impl Backend for NullBackend {
    fn info(&self) -> BackendInfo {
        BackendInfo::basic("null")
    }

    fn show(&self, _shown: &Shown<'_>) -> Result<(), NotificationError> {
        Ok(())
    }

    fn remove(&self, _id: &NotificationId) -> Result<(), NotificationError> {
        Ok(())
    }
}
