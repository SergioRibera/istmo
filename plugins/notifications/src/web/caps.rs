//! What the browser offers, as found by feature detection, and the
//! capability report and render limits that follow from it.

use crate::coalesce::Rate;
use crate::render::{Labels, RenderLimits};
use crate::{Capabilities, Feature, FeatureSupport, Support};

/// The live-update rate on the web (section 2 of the design).
pub(crate) const RATE: Rate = Rate::WEB;

/// What the browser offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "one flag per detected browser feature"
)]
pub(crate) struct Browser {
    /// `Notification` exists.
    pub(crate) api: bool,
    /// A service worker registration is active and shows notifications.
    pub(crate) service_worker: bool,
    /// `new Notification()` threw (Chrome on Android: mobile web needs
    /// the service worker even to show).
    pub(crate) page_refused: bool,
    /// `Notification.maxActions` (0 where `actions` is missing).
    pub(crate) max_actions: u32,
    /// `image` (Chromium).
    pub(crate) image: bool,
    /// `requireInteraction`.
    pub(crate) require_interaction: bool,
    /// `silent`.
    pub(crate) silent: bool,
    /// `renotify`.
    pub(crate) renotify: bool,
    /// `timestamp`.
    pub(crate) timestamp: bool,
}

impl Browser {
    /// Whether anything can be shown now.
    pub(crate) const fn can_show(&self) -> bool {
        self.api && (self.service_worker || !self.page_refused)
    }

    /// Action buttons shown: only service worker notifications have them.
    pub(crate) const fn buttons(&self) -> u32 {
        if self.can_show() && self.service_worker {
            self.max_actions
        } else {
            0
        }
    }

    /// `web:sw`, `web:page` or `web:none`.
    pub(crate) const fn backend(&self) -> &'static str {
        if !self.can_show() {
            "web:none"
        } else if self.service_worker {
            "web:sw"
        } else {
            "web:page"
        }
    }

    /// The report: the web column of the design's matrix, narrowed by
    /// what this browser has.
    pub(crate) fn capabilities(&self) -> Capabilities {
        if !self.can_show() {
            let mut caps = Capabilities::none(self.backend());
            if self.api {
                caps.features.push(FeatureSupport {
                    feature: Feature::PermissionRequest,
                    support: Support::Native,
                });
            }
            return caps;
        }
        let sw = self.service_worker;
        let actions = self.buttons() > 0;
        let on = |yes: bool, support: Support| {
            if yes { support } else { Support::Unsupported }
        };
        let features = [
            (Feature::Show, Support::Native),
            (Feature::Update, Support::Native),
            (Feature::SilentUpdate, Support::Native),
            (Feature::ProgressBar, Support::Simulated),
            (Feature::Schedule, Support::WhileRunning),
            (Feature::Repeat, Support::WhileRunning),
            (Feature::Missed, Support::Simulated),
            (Feature::Actions, on(actions, Support::Native)),
            (Feature::InlineReply, on(actions, Support::Simulated)),
            (Feature::Choices, on(actions, Support::Simulated)),
            (Feature::Snooze, on(actions, Support::WhileRunning)),
            (Feature::Avatar, Support::Native),
            (Feature::Picture, on(sw && self.image, Support::Native)),
            (Feature::Messaging, Support::Simulated),
            (Feature::CallStyle, Support::Simulated),
            (Feature::Channels, Support::Simulated),
            (
                Feature::Interruption,
                on(self.require_interaction || self.silent, Support::Simulated),
            ),
            (
                Feature::Ongoing,
                on(self.require_interaction, Support::Native),
            ),
            (Feature::Timeout, Support::WhileRunning),
            (Feature::Chronometer, on(self.timestamp, Support::Native)),
            (
                Feature::ListDelivered,
                if sw {
                    Support::Native
                } else {
                    Support::WhileRunning
                },
            ),
            (Feature::ListPending, Support::Simulated),
            (Feature::Dismissal, Support::Native),
            (Feature::TapWhenClosed, on(sw, Support::Native)),
            (Feature::BackgroundAction, on(actions, Support::Native)),
            (Feature::PermissionRequest, Support::Native),
        ];
        Capabilities {
            backend: self.backend().to_owned(),
            features: features
                .into_iter()
                .filter(|(_, support)| *support != Support::Unsupported)
                .map(|(feature, support)| FeatureSupport { feature, support })
                .collect(),
            max_actions: self.buttons(),
            max_pending: None,
            max_active: None,
        }
    }

    /// Nothing but the buttons is native; the rest is simulated.
    pub(crate) fn limits(&self, labels: &Labels) -> RenderLimits {
        RenderLimits {
            max_actions: self.buttons(),
            labels: labels.clone(),
            ..RenderLimits::default()
        }
    }
}

/// What the page tells about the browser it runs in, for
/// [`refuses_page_notifications`].
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct UserAgent<'a> {
    /// `navigator.userAgentData.mobile` (Chromium only).
    pub(crate) hint_mobile: Option<bool>,
    /// `navigator.userAgentData.platform` (Chromium only).
    pub(crate) hint_platform: Option<&'a str>,
    /// `navigator.userAgent`.
    pub(crate) text: &'a str,
    /// `'ontouchstart' in globalThis`.
    pub(crate) touch: bool,
}

/// Whether `new Notification()` will throw: Chromium on Android (Chrome,
/// Edge, Samsung Internet) only shows through a service worker. Known
/// up front from User-Agent Client Hints, else from a touch Android
/// Chromium user agent; Firefox for Android shows page notifications.
/// The host still falls back on the `TypeError` when this misses.
pub(crate) fn refuses_page_notifications(agent: &UserAgent<'_>) -> bool {
    if let (Some(mobile), Some(name)) = (agent.hint_mobile, agent.hint_platform) {
        return mobile && name.eq_ignore_ascii_case("android");
    }
    let ua = agent.text;
    agent.touch && ua.contains("Android") && ua.contains("Chrome/") && !ua.contains("Firefox/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chrome() -> Browser {
        Browser {
            api: true,
            service_worker: true,
            page_refused: false,
            max_actions: 2,
            image: true,
            require_interaction: true,
            silent: true,
            renotify: true,
            timestamp: true,
        }
    }

    #[test]
    fn chromium_with_a_worker_has_buttons_and_pictures() {
        let caps = chrome().capabilities();
        assert_eq!(caps.backend, "web:sw");
        assert_eq!(caps.max_actions, 2);
        assert_eq!(caps.support(Feature::Actions), Support::Native);
        assert_eq!(caps.support(Feature::Picture), Support::Native);
        assert_eq!(caps.support(Feature::InlineReply), Support::Simulated);
        assert_eq!(caps.support(Feature::TapWhenClosed), Support::Native);
        assert_eq!(caps.support(Feature::ListDelivered), Support::Native);
        assert_eq!(caps.support(Feature::Schedule), Support::WhileRunning);
        assert_eq!(caps.support(Feature::Groups), Support::Unsupported);
        assert_eq!(caps.support(Feature::CustomSound), Support::Unsupported);
        assert_eq!(caps.support(Feature::OpenSettings), Support::Unsupported);
        assert_eq!(chrome().limits(&Labels::default()).max_actions, 2);
    }

    #[test]
    fn firefox_and_safari_lack_actions_and_images() {
        let firefox = Browser {
            max_actions: 0,
            image: false,
            renotify: false,
            timestamp: false,
            ..chrome()
        };
        let caps = firefox.capabilities();
        assert_eq!(caps.max_actions, 0);
        for feature in [
            Feature::Actions,
            Feature::InlineReply,
            Feature::Choices,
            Feature::Snooze,
            Feature::Picture,
            Feature::BackgroundAction,
            Feature::Chronometer,
        ] {
            assert_eq!(caps.support(feature), Support::Unsupported, "{feature}");
        }
        assert_eq!(caps.support(Feature::Show), Support::Native);
    }

    #[test]
    fn the_page_alone_has_no_buttons_and_no_cold_start() {
        let page = Browser {
            service_worker: false,
            ..chrome()
        };
        let caps = page.capabilities();
        assert_eq!(caps.backend, "web:page");
        assert_eq!(caps.max_actions, 0);
        assert_eq!(caps.support(Feature::TapWhenClosed), Support::Unsupported);
        assert_eq!(caps.support(Feature::ListDelivered), Support::WhileRunning);
        assert_eq!(caps.support(Feature::Picture), Support::Unsupported);
    }

    #[test]
    fn a_refused_page_without_a_worker_shows_nothing() {
        let android = Browser {
            service_worker: false,
            page_refused: true,
            ..chrome()
        };
        assert!(!android.can_show());
        let caps = android.capabilities();
        assert_eq!(caps.backend, "web:none");
        assert_eq!(caps.support(Feature::Show), Support::Unsupported);
        assert_eq!(caps.support(Feature::PermissionRequest), Support::Native);
        assert!(
            !Browser::default()
                .capabilities()
                .has(Feature::PermissionRequest)
        );
    }

    #[test]
    fn chromium_on_android_is_known_to_refuse_page_notifications() {
        let hints = |mobile, platform| UserAgent {
            hint_mobile: Some(mobile),
            hint_platform: Some(platform),
            ..UserAgent::default()
        };
        assert!(refuses_page_notifications(&hints(true, "Android")));
        assert!(!refuses_page_notifications(&hints(false, "Android")));
        assert!(!refuses_page_notifications(&hints(false, "Windows")));
        let chrome_android = "Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 \
                              (KHTML, like Gecko) Chrome/130.0 Mobile Safari/537.36";
        let firefox_android =
            "Mozilla/5.0 (Android 14; Mobile; rv:130.0) Gecko/130.0 Firefox/130.0";
        let by_ua = |text, touch| UserAgent {
            text,
            touch,
            ..UserAgent::default()
        };
        assert!(refuses_page_notifications(&by_ua(chrome_android, true)));
        assert!(!refuses_page_notifications(&by_ua(chrome_android, false)));
        assert!(!refuses_page_notifications(&by_ua(firefox_android, true)));
    }
}
