//! What the browser is handed for one notification: the options of
//! `showNotification` / `new Notification`, and the `data` the service
//! worker reads back on a click or a close.
//!
//! `data` carries a *record*: the notification's wire bytes with every
//! embedded image byte removed. A page loaded later rebuilds its table
//! from the records `getNotifications()` returns, so responses, updates
//! and snoozes of notifications an earlier page showed still work
//! (without their embedded images, unless an update sets them again).

use istmo_core::codec;

use super::state::{Post, ShowMode};
use crate::{Interruption, Notification, NotificationImage, Sound, Style};

/// Marker in `data.istmo`: the notification is the plugin's. Bumped if
/// the shape of `data` changes.
pub(crate) const DATA_VERSION: u32 = 1;

/// One notification as the browser gets it. Every field is plain data
/// (the JS objects are made from it on the page).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Payload {
    /// `tag`: the notification id (same tag = replace in place).
    pub(crate) tag: String,
    pub(crate) title: String,
    /// With every simulated line.
    pub(crate) body: Option<String>,
    pub(crate) icon: Option<String>,
    pub(crate) image: Option<String>,
    pub(crate) actions: Vec<PayloadAction>,
    pub(crate) require_interaction: bool,
    pub(crate) silent: bool,
    /// Alert again although the tag replaces one on screen.
    pub(crate) renotify: bool,
    pub(crate) timestamp: Option<i64>,
    pub(crate) data: Data,
}

/// One button.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PayloadAction {
    /// The rendered key; `event.action` hands it back.
    pub(crate) key: String,
    pub(crate) title: String,
    pub(crate) icon: Option<String>,
}

/// `notification.data`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Data {
    pub(crate) id: String,
    /// [`record`] of the notification.
    pub(crate) record: Vec<u8>,
    /// Keys of buttons that must not bring the app forward.
    pub(crate) background: Vec<String>,
    /// When it was first shown, Unix-epoch milliseconds.
    pub(crate) delivered_ms: i64,
    /// URL the worker opens when no window of the app is open; `None`
    /// = the registration's scope.
    pub(crate) open: Option<String>,
}

/// Image URLs already resolved for a post.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Urls {
    pub(crate) icon: Option<String>,
    pub(crate) image: Option<String>,
    /// Icon of each action, by action id.
    pub(crate) actions: Vec<(String, String)>,
}

/// The options flags the browser supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[expect(clippy::struct_excessive_bools, reason = "one flag per option")]
pub(crate) struct Options {
    pub(crate) require_interaction: bool,
    pub(crate) silent: bool,
    pub(crate) renotify: bool,
    pub(crate) timestamp: bool,
    pub(crate) image: bool,
}

/// The payload of `post`.
pub(crate) fn build(
    post: &Post,
    urls: Urls,
    options: Options,
    delivered_ms: i64,
    open: Option<&str>,
) -> Payload {
    let n = &post.notification;
    let quiet = n.sound == Some(Sound::Silent) || n.interruption == Some(Interruption::Passive);
    let (silent, renotify) = match post.mode {
        ShowMode::New => (quiet, false),
        ShowMode::Replace { alert: true } => (quiet, options.renotify),
        // `renotify: false` keeps a replace quiet; browsers without
        // `renotify` get `silent` as well.
        ShowMode::Replace { alert: false } => (quiet || !options.renotify, false),
    };
    let insistent = n.ongoing
        || matches!(
            n.interruption,
            Some(Interruption::Urgent | Interruption::Critical)
        );
    let action_icon = |key: &str| {
        let id = key.split_once('#').map_or(key, |(id, _)| id);
        urls.actions
            .iter()
            .find(|(action, _)| action == id)
            .map(|(_, url)| url.clone())
    };
    let actions = post
        .rendered
        .actions
        .iter()
        .map(|action| PayloadAction {
            key: action.key.clone(),
            title: action.title.clone(),
            icon: action_icon(&action.key),
        })
        .collect();
    let background = post
        .rendered
        .actions
        .iter()
        .filter(|action| !action.opens_app)
        .map(|action| action.key.clone())
        .collect();
    Payload {
        tag: n.id.value.clone(),
        title: n.title.clone(),
        body: post.rendered.body.clone(),
        icon: urls.icon,
        image: if options.image { urls.image } else { None },
        actions,
        require_interaction: insistent && options.require_interaction,
        silent: silent && options.silent,
        renotify,
        timestamp: if options.timestamp {
            n.timestamp_ms
                .or_else(|| n.chronometer.as_ref().map(|c| c.base_ms))
        } else {
            None
        },
        data: Data {
            id: n.id.value.clone(),
            record: record(n),
            background,
            delivered_ms,
            open: open.map(str::to_owned),
        },
    }
}

/// `image` without its bytes; files and names are kept.
fn keep(image: Option<&NotificationImage>) -> Option<NotificationImage> {
    match image {
        Some(NotificationImage::Png(_) | NotificationImage::Jpeg(_)) | None => None,
        Some(other) => Some(other.clone()),
    }
}

/// `n` without embedded image bytes.
pub(crate) fn strip(n: &Notification) -> Notification {
    let mut out = n.clone();
    out.icon = keep(n.icon.as_ref());
    for action in &mut out.actions {
        action.icon = keep(action.icon.as_ref());
    }
    match &mut out.style {
        Style::Picture(NotificationImage::Png(_) | NotificationImage::Jpeg(_)) => {
            out.style = Style::Plain;
        }
        Style::Messaging(conversation) => {
            conversation.user.avatar = keep(conversation.user.avatar.as_ref());
            for message in &mut conversation.messages {
                message.image = keep(message.image.as_ref());
                if let Some(sender) = &mut message.sender {
                    sender.avatar = keep(sender.avatar.as_ref());
                }
            }
        }
        Style::Call(call) => call.caller.avatar = keep(call.caller.avatar.as_ref()),
        _ => {}
    }
    out
}

/// The record of `n`: wire bytes of `n` after [`strip`]; empty when it does
/// not encode (the page then answers from `id` alone).
pub(crate) fn record(n: &Notification) -> Vec<u8> {
    codec::encode(&strip(n)).unwrap_or_default()
}

/// The notification in a record.
pub(crate) fn read_record(bytes: &[u8]) -> Option<Notification> {
    if bytes.is_empty() {
        return None;
    }
    codec::decode::<Notification>(bytes).ok().map(|(n, _)| n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::{RenderLimits, render};
    use crate::{Action, Chronometer, NotificationImage};

    fn post(n: Notification, mode: ShowMode, max_actions: u32) -> Post {
        let limits = RenderLimits {
            max_actions,
            ..RenderLimits::default()
        };
        Post {
            rendered: render(&n, &limits),
            notification: n,
            mode,
            fresh_images: true,
        }
    }

    const ALL: Options = Options {
        require_interaction: true,
        silent: true,
        renotify: true,
        timestamp: true,
        image: true,
    };

    #[test]
    fn buttons_keep_their_keys_and_background_flag() {
        let n = Notification::new("m:1", "Hi")
            .unwrap()
            .with_body("there")
            .with_action(Action::button("read", "Mark read").unwrap().background())
            .with_action(Action::button("open", "Open").unwrap());
        let p = build(&post(n, ShowMode::New, 2), Urls::default(), ALL, 7, None);
        assert_eq!(p.tag, "m:1");
        assert_eq!(p.body.as_deref(), Some("there"));
        let keys: Vec<_> = p.actions.iter().map(|a| a.key.as_str()).collect();
        assert_eq!(keys, ["read", "open"]);
        assert_eq!(p.data.background, ["read"]);
        assert_eq!(p.data.delivered_ms, 7);
        assert!(!p.renotify && !p.silent);
    }

    #[test]
    fn updates_alert_only_when_asked() {
        let n = Notification::new("u", "t").unwrap();
        let alert = build(
            &post(n.clone(), ShowMode::Replace { alert: true }, 0),
            Urls::default(),
            ALL,
            0,
            None,
        );
        assert!(alert.renotify && !alert.silent);
        let quiet = build(
            &post(n.clone(), ShowMode::Replace { alert: false }, 0),
            Urls::default(),
            ALL,
            0,
            None,
        );
        assert!(!quiet.renotify && !quiet.silent);
        let no_renotify = Options {
            renotify: false,
            ..ALL
        };
        let firefox = build(
            &post(n, ShowMode::Replace { alert: false }, 0),
            Urls::default(),
            no_renotify,
            0,
            None,
        );
        assert!(firefox.silent && !firefox.renotify);
    }

    #[test]
    fn interruption_maps_to_options_the_browser_has() {
        let urgent = Notification::new("a", "t")
            .unwrap()
            .with_interruption(Interruption::Urgent)
            .with_chronometer(Chronometer {
                base_ms: 42,
                count_down: false,
            });
        let p = build(
            &post(urgent.clone(), ShowMode::New, 0),
            Urls::default(),
            ALL,
            0,
            None,
        );
        assert!(p.require_interaction);
        assert_eq!(p.timestamp, Some(42));
        let bare = build(
            &post(urgent, ShowMode::New, 0),
            Urls::default(),
            Options::default(),
            0,
            None,
        );
        assert!(!bare.require_interaction);
        assert_eq!(bare.timestamp, None);
        let passive = Notification::new("b", "t")
            .unwrap()
            .with_interruption(Interruption::Passive);
        assert!(
            build(
                &post(passive, ShowMode::New, 0),
                Urls::default(),
                ALL,
                0,
                None
            )
            .silent
        );
    }

    #[test]
    fn records_drop_image_bytes_and_round_trip() {
        let n = Notification::new("r", "t")
            .unwrap()
            .with_icon(NotificationImage::png(vec![0; 1024]))
            .with_style(Style::Picture(NotificationImage::jpeg(vec![1; 64])))
            .with_user_data("ud");
        let bytes = record(&n);
        assert!(bytes.len() < 256);
        let back = read_record(&bytes).unwrap();
        assert_eq!(back.icon, None);
        assert_eq!(back.style, Style::Plain);
        assert_eq!(back.user_data.as_deref(), Some("ud"));
        let file = Notification::new("f", "t")
            .unwrap()
            .with_icon(NotificationImage::file("/a.png"));
        assert_eq!(read_record(&record(&file)).unwrap().icon, file.icon);
        assert_eq!(read_record(&[]), None);
        assert_eq!(read_record(&[0xff; 3]), None);
    }
}
