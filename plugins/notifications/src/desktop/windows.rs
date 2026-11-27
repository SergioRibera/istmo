//! Windows backend: toasts through `Windows.UI.Notifications`.
//!
//! A COM activator handles taps and buttons when the app is closed,
//! registry-only registration serves unpackaged apps (no Start-menu
//! shortcut), and scheduled toasts deliver while the app is closed.
//!
//! The pure parts (toast XML, arguments, identities, the capability
//! report) compile everywhere so their tests run on any host; the
//! `WinRT` / COM / registry parts only on Windows.
//!
//! App wiring (see the crate README): call
//! [`DesktopNotifications::install`](crate::DesktopNotifications::install)
//! first thing in `main`, before the runtime starts, so COM finds the
//! activator within its 60 s window when the app was started by a toast.

#![cfg_attr(
    not(windows),
    allow(
        dead_code,
        reason = "the pure parts are used by the Windows backend; tests use them here"
    )
)]

mod args;
mod ident;
mod toast_xml;

#[cfg(windows)]
mod activator;
#[cfg(windows)]
mod notifier;
#[cfg(windows)]
mod registration;
#[cfg(windows)]
mod sys;

#[cfg(windows)]
pub(super) use notifier::start;
#[cfg(windows)]
pub use registration::{Installation, WindowsApp, app_args, launched_by_notification};

use crate::coalesce::Rate;
use crate::desktop::BackendInfo;
use crate::render::RenderLimits;
use crate::{Capabilities, Feature, FeatureSupport, Support};

/// Toast group of every toast this plugin shows (the tag is the
/// notification id: ids are at most 64 characters of `[A-Za-z0-9._:-]`,
/// the tag and group limit since build 15063, so no hashing is needed).
pub(crate) const GROUP: &str = "istmo";
/// Oldest supported build (Windows 10 1809).
pub(crate) const MIN_BUILD: u32 = 17_763;
/// Buttons a toast shows (context menu items count too; none are used).
pub(crate) const MAX_BUTTONS: u32 = 5;
/// Notifications an app keeps in the notification center.
pub(crate) const MAX_ACTIVE: u32 = 20;

/// What toasts show natively.
pub(crate) fn limits() -> RenderLimits {
    RenderLimits {
        max_actions: MAX_BUTTONS,
        subtitle: true,
        progress_bar: true,
        inline_reply: true,
        choices: true,
        big_text: false,
        inbox: false,
        messaging: false,
        call: true,
        labels: crate::render::Labels::default(),
    }
}

/// The capability report of build `build`; `activator` = the COM
/// activator is registered (taps and buttons reach a closed app).
pub(crate) fn info(build: u32, activator: bool) -> BackendInfo {
    use Support::{Native, Simulated, Unsupported, WhileRunning};
    let closed = if activator { Native } else { WhileRunning };
    let features = [
        (Feature::Show, Native),
        (Feature::Update, Native),
        (Feature::SilentUpdate, Native),
        (Feature::ProgressBar, Native),
        (Feature::LivePromotion, Unsupported),
        (Feature::Schedule, Native),
        (Feature::Repeat, Simulated),
        (Feature::Missed, Native),
        (Feature::Actions, Native),
        (Feature::InlineReply, Native),
        (Feature::Choices, Native),
        (Feature::Snooze, Simulated),
        (Feature::Avatar, Native),
        (Feature::Picture, Native),
        (Feature::Messaging, Simulated),
        (Feature::CallStyle, Native),
        (Feature::Groups, Native),
        (Feature::Channels, Simulated),
        (Feature::CustomSound, Native),
        (Feature::Interruption, Simulated),
        (Feature::Ongoing, Simulated),
        (Feature::Timeout, Native),
        (Feature::LockScreen, Unsupported),
        (Feature::Chronometer, Unsupported),
        (Feature::ListDelivered, Native),
        (Feature::ListPending, Simulated),
        (Feature::Dismissal, WhileRunning),
        (Feature::TapWhenClosed, closed),
        (Feature::BackgroundAction, closed),
        (Feature::PermissionRequest, Unsupported),
        (Feature::ExactAlarms, Unsupported),
        (Feature::DoNotDisturb, Unsupported),
        (Feature::OpenSettings, Native),
    ];
    let mut capabilities = Capabilities::none(format!("windows:{build}"));
    capabilities.features = features
        .into_iter()
        .map(|(feature, support)| FeatureSupport { feature, support })
        .collect();
    capabilities.max_actions = MAX_BUTTONS;
    capabilities.max_pending = None;
    capabilities.max_active = Some(MAX_ACTIVE);
    BackendInfo {
        capabilities,
        limits: limits(),
        rate: Rate::WINDOWS,
        lists_delivered: true,
        timeouts: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_matches_the_limits() {
        let info = info(22_631, true);
        assert_eq!(info.capabilities.backend, "windows:22631");
        assert_eq!(info.capabilities.max_actions, info.limits.max_actions);
        assert_eq!(
            info.capabilities.support(Feature::TapWhenClosed),
            Support::Native
        );
        assert_eq!(
            super::info(22_631, false)
                .capabilities
                .support(Feature::TapWhenClosed),
            Support::WhileRunning
        );
        // Native features agree with what the renderer is told.
        assert_eq!(
            RenderLimits::from_capabilities(&info.capabilities).inline_reply,
            info.limits.inline_reply
        );
        assert_eq!(
            RenderLimits::from_capabilities(&info.capabilities).choices,
            info.limits.choices
        );
    }
}
