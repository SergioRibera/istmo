//! `org.freedesktop.portal.Notification` v1 / v2.
//!
//! * Every button and the default action use one action name,
//!   `istmo-notification`; its target is a dictionary naming the
//!   notification, the rendered key, the user data and the URL, so an
//!   activation of a notification an earlier process posted still
//!   resolves ([`Carried`]).
//! * When the backend can own the app id on the bus, the action is
//!   exported (`app.istmo-notification`): the portal calls
//!   `org.freedesktop.Application.ActivateAction` on that name, D-Bus
//!   activating the app when it is not running (packaging writes the
//!   `.service` file and `DBusActivatable=true`). Otherwise (a
//!   `GApplication` already owns the name) the portal emits
//!   `ActionInvoked`, which reaches only a running process.
//! * v2: reply buttons carry the purpose `im.reply-with-text`, the
//!   plugin's call buttons `call.accept` / `call.decline` /
//!   `call.hang-up`, each only when `SupportedOptions` lists it;
//!   categories likewise; display hints and `silent` sound for silent
//!   updates.
//! * The portal has no closed signal: dismissals are not reported.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};

use istmo_core::sync::lock;
use zbus::MatchRule;
use zbus::blocking::{Connection, MessageIterator};
use zbus::fdo::{RequestNameFlags, RequestNameReply};
use zbus::message::Type;
use zbus::zvariant::{OwnedValue, StructureBuilder, Value};

use super::app_activation::Application;
use super::{Owner, Picked, pick_image, platform, set_activation_token, spawn};
use crate::coalesce::Rate;
use crate::desktop::{Activation, Backend, BackendInfo, BackendSink, Carried, ShowMode, Shown};
use crate::render::{Input, RenderLimits};
use crate::validate::MAX_IMAGE_BYTES;
use crate::{
    ActionId, CallKind, Capabilities, Category, Feature, FeatureSupport, Interruption, LockScreen,
    NotificationError, NotificationId, Sound, Style, Support,
};

const NAME: &str = "org.freedesktop.portal.Desktop";
const PATH: &str = "/org/freedesktop/portal/desktop";
const IFACE: &str = "org.freedesktop.portal.Notification";
const REGISTRY: &str = "org.freedesktop.host.portal.Registry";
/// The one action every button and the default action name.
pub(super) const ACTION: &str = "istmo-notification";
const REPLY_PURPOSE: &str = "im.reply-with-text";
/// Buttons shown; GNOME draws three.
const MAX_ACTIONS: u32 = 3;

// Keys of the action target dictionary.
const T_ID: &str = "istmo-id";
const T_KEY: &str = "istmo-key";
const T_DATA: &str = "istmo-user-data";
const T_URL: &str = "istmo-url";

/// Registers a host (unsandboxed) app id with the portal: it must be the
/// connection's first portal call. `false` when the portal is missing,
/// too old, or no `.desktop` file has that basename.
pub(super) fn register_host(conn: &Connection, app_id: &str) -> bool {
    let options: HashMap<&str, Value<'_>> = HashMap::new();
    conn.call_method(
        Some(NAME),
        PATH,
        Some(REGISTRY),
        "Register",
        &(app_id, options),
    )
    .inspect_err(|err| tracing::debug!(%err, "notifications: portal registry refused"))
    .is_ok()
}

/// What activations need, shared with the exported `Application` object.
#[derive(Debug)]
pub(super) struct Shared {
    sink: BackendSink,
    /// Ids posted by this process (anything else was posted by an
    /// earlier one: the activation started this process).
    posted: Mutex<Posted>,
}

/// Ids posted while this process has more than this many are forgotten
/// oldest first: the portal never tells when one left the screen.
const MAX_POSTED: usize = 512;

/// The ids this process posted, bounded by [`MAX_POSTED`].
#[derive(Debug, Default)]
struct Posted {
    order: VecDeque<NotificationId>,
    ids: HashSet<NotificationId>,
}

impl Posted {
    /// Records `id` (an update of a known id copies nothing).
    fn insert(&mut self, id: &NotificationId) {
        if self.ids.contains(id) {
            return;
        }
        if self.order.len() == MAX_POSTED
            && let Some(oldest) = self.order.pop_front()
        {
            self.ids.remove(&oldest);
        }
        self.ids.insert(id.clone());
        self.order.push_back(id.clone());
    }

    fn remove(&mut self, id: &NotificationId) {
        if self.ids.remove(id) {
            self.order.retain(|have| have != id);
        }
    }

    fn contains(&self, id: &NotificationId) -> bool {
        self.ids.contains(id)
    }
}

impl Shared {
    /// An activation with the portal's `parameter` array: the target,
    /// then (v2) platform data and the reply text, in whichever order the
    /// portal version uses.
    pub(super) fn activate(&self, parameter: &[OwnedValue]) {
        let mut target = None;
        let mut reply = None;
        for value in parameter {
            let value = unwrap_variant(value);
            if let Ok(dict) = HashMap::<String, OwnedValue>::try_from(value.clone()) {
                if dict.contains_key(T_ID) {
                    target = Some(dict);
                } else if let Some(token) = dict.get("activation-token").and_then(text) {
                    set_activation_token(token);
                }
            } else if let Some(text) = text(&value) {
                reply = Some(text);
            }
        }
        let Some(target) = target else {
            tracing::debug!("notifications: portal activation without an istmo target");
            return;
        };
        let Some(id) = target
            .get(T_ID)
            .and_then(text)
            .and_then(|id| NotificationId::try_from(id.as_str()).ok())
        else {
            return;
        };
        let launched_app = !lock(&self.posted).contains(&id);
        self.sink.activated(Activation {
            id,
            key: target.get(T_KEY).and_then(text),
            reply,
            launched_app,
            carried: Some(Carried {
                user_data: target.get(T_DATA).and_then(text),
                url: target.get(T_URL).and_then(text),
            }),
        });
    }

    /// `Activate` / `ActivateAction` platform data.
    pub(super) fn platform_data(data: &HashMap<String, OwnedValue>) {
        if let Some(token) = data.get("activation-token").and_then(text) {
            set_activation_token(token);
        }
    }
}

/// Strips `v` wrappers.
fn unwrap_variant(value: &OwnedValue) -> OwnedValue {
    let mut value: Value<'_> = (**value).try_clone().unwrap_or(Value::Bool(false));
    while let Value::Value(inner) = value {
        value = *inner;
    }
    OwnedValue::try_from(value).unwrap_or_else(|_| OwnedValue::from(false))
}

fn text(value: &OwnedValue) -> Option<String> {
    match &**value {
        Value::Str(s) => Some(s.as_str().to_owned()),
        Value::Value(inner) => match &**inner {
            Value::Str(s) => Some(s.as_str().to_owned()),
            _ => None,
        },
        _ => None,
    }
}

/// The notification portal backend.
#[derive(Debug)]
pub(super) struct Portal {
    conn: Connection,
    shared: Arc<Shared>,
    version: u32,
    categories: Vec<String>,
    purposes: Vec<String>,
    /// `app.` actions reach `org.freedesktop.Application` on our name.
    exported: bool,
    info: BackendInfo,
}

impl Portal {
    /// The portal backend, `None` when the portal has no Notification
    /// interface.
    pub(super) fn open(
        conn: Connection,
        app_id: &str,
        sink: BackendSink,
    ) -> Result<Option<Arc<dyn Backend>>, NotificationError> {
        let Some(version) = property::<u32>(&conn, "version") else {
            return Ok(None);
        };
        let (categories, purposes) = if version >= 2 {
            let options = property::<HashMap<String, OwnedValue>>(&conn, "SupportedOptions")
                .unwrap_or_default();
            let list = |key: &str| {
                options
                    .get(key)
                    .and_then(|v| Vec::<String>::try_from(unwrap_variant(v)).ok())
                    .unwrap_or_default()
            };
            (list("category"), list("button-purpose"))
        } else {
            (Vec::new(), Vec::new())
        };
        let shared = Arc::new(Shared {
            sink,
            posted: Mutex::default(),
        });
        let exported = export(&conn, app_id, &shared);
        let signals = MessageIterator::for_match_rule(
            MatchRule::builder()
                .msg_type(Type::Signal)
                .interface(IFACE)
                .and_then(|r| r.member("ActionInvoked"))
                .and_then(|r| r.path(PATH))
                .map_err(platform)?
                .build(),
            &conn,
            Some(64),
        )
        .map_err(platform)?;
        let owner = Owner::new(NAME);
        let listener = Arc::downgrade(&shared);
        let signal_conn = conn.clone();
        spawn("istmo-notif-portal", move || {
            for message in signals {
                let Some(shared) = listener.upgrade() else {
                    return;
                };
                let Ok(message) = message else { continue };
                let header = message.header();
                let Some(sender) = header.sender() else {
                    continue;
                };
                if !owner.is(&signal_conn, sender.as_str()) {
                    continue;
                }
                if let Ok((_id, action, parameter)) =
                    message
                        .body()
                        .deserialize::<(String, String, Vec<OwnedValue>)>()
                    && action == ACTION
                {
                    shared.activate(&parameter);
                }
            }
        })?;
        let reply = version >= 2 && purposes.iter().any(|p| p == REPLY_PURPOSE);
        let calls = version >= 2
            && categories.iter().any(|c| c == "call.incoming")
            && purposes.iter().any(|p| p == "call.accept");
        let info = info(version, exported, reply, calls);
        Ok(Some(Arc::new(Self {
            conn,
            shared,
            version,
            categories,
            purposes,
            exported,
            info,
        })))
    }

    fn supports_category(&self, category: &str) -> bool {
        self.categories.iter().any(|c| c == category)
    }

    fn supports_purpose(&self, purpose: &str) -> bool {
        self.purposes.iter().any(|p| p == purpose)
    }

    fn action_name(&self) -> String {
        if self.exported {
            format!("app.{ACTION}")
        } else {
            ACTION.to_owned()
        }
    }
}

/// Serves `org.freedesktop.Application` and owns `app_id` when the name
/// is free.
fn export(conn: &Connection, app_id: &str, shared: &Arc<Shared>) -> bool {
    let path = app_path(app_id);
    let served = conn
        .object_server()
        .at(path.as_str(), Application::new(Arc::clone(shared)))
        .unwrap_or(false);
    if !served {
        return false;
    }
    let owned = conn
        .request_name_with_flags(app_id, RequestNameFlags::DoNotQueue.into())
        .is_ok_and(|reply| reply == RequestNameReply::PrimaryOwner);
    if !owned {
        // Someone else (a GApplication) serves the app: plain actions.
        let _removed = conn.object_server().remove::<Application, _>(path.as_str());
    }
    owned
}

/// `org.example.App-1` -> `/org/example/App_1` (`GApplication`'s rule).
fn app_path(app_id: &str) -> String {
    let mut path = String::with_capacity(app_id.len() + 1);
    path.push('/');
    for c in app_id.chars() {
        path.push(match c {
            '.' => '/',
            '-' => '_',
            c => c,
        });
    }
    path
}

fn property<T: TryFrom<OwnedValue>>(conn: &Connection, name: &str) -> Option<T> {
    let reply = conn
        .call_method(
            Some(NAME),
            PATH,
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &(IFACE, name),
        )
        .ok()?;
    let value: OwnedValue = reply.body().deserialize().ok()?;
    T::try_from(unwrap_variant(&value)).ok()
}

fn info(version: u32, exported: bool, reply: bool, calls: bool) -> BackendInfo {
    let mut features = Vec::with_capacity(14);
    let mut add = |feature, support| features.push(FeatureSupport { feature, support });
    for feature in [
        Feature::Show,
        Feature::Update,
        Feature::SilentUpdate,
        Feature::Actions,
        Feature::Interruption,
        Feature::Avatar,
    ] {
        add(feature, Support::Native);
    }
    if version >= 2 {
        add(Feature::Ongoing, Support::Native);
        add(Feature::LockScreen, Support::Native);
    }
    if reply {
        add(Feature::InlineReply, Support::Native);
    }
    if calls {
        add(Feature::CallStyle, Support::Native);
    }
    if exported {
        add(Feature::TapWhenClosed, Support::Native);
        add(Feature::BackgroundAction, Support::Native);
    } else {
        add(Feature::BackgroundAction, Support::WhileRunning);
    }
    let mut capabilities = Capabilities::none(format!("portal:{version}"));
    capabilities.features = features;
    capabilities.max_actions = MAX_ACTIONS;
    capabilities.max_pending = None;
    capabilities.max_active = None;
    let mut limits = RenderLimits::from_capabilities(&capabilities);
    // The portal draws call buttons only from buttons with a purpose:
    // the plugin's call actions must still be rendered.
    limits.call = false;
    BackendInfo {
        limits,
        capabilities,
        rate: Rate::LINUX,
        lists_delivered: false,
        timeouts: false,
    }
}

/// The action target: which notification, which key, what it carries.
fn target<'a>(
    id: &'a NotificationId,
    key: Option<&'a str>,
    user_data: Option<&'a str>,
    url: Option<&'a str>,
) -> Value<'a> {
    let mut dict: HashMap<&str, Value<'a>> = HashMap::with_capacity(4);
    dict.insert(T_ID, Value::from(id.as_str()));
    if let Some(key) = key {
        dict.insert(T_KEY, Value::from(key));
    }
    if let Some(data) = user_data {
        dict.insert(T_DATA, Value::from(data));
    }
    if let Some(url) = url {
        dict.insert(T_URL, Value::from(url));
    }
    Value::from(dict)
}

/// A serialized `GIcon`: `(sv)`.
fn icon(kind: &str, value: Value<'_>) -> Value<'static> {
    let structure = StructureBuilder::new()
        .add_field(kind.to_owned())
        .append_field(Value::Value(Box::new(value)))
        .build()
        .map(|s| Value::from(s).try_into_owned().map(Value::from));
    match structure {
        Ok(Ok(value)) => value,
        _ => Value::from(""),
    }
}

impl Backend for Portal {
    fn info(&self) -> BackendInfo {
        self.info.clone()
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one portal key after another; splitting scatters the mapping table"
    )]
    fn show(&self, shown: &Shown<'_>) -> Result<(), NotificationError> {
        let n = shown.notification;
        let v2 = self.version >= 2;
        let action = self.action_name();
        let mut dict: HashMap<&str, Value<'_>> = HashMap::with_capacity(12);
        dict.insert("title", Value::from(n.title.as_str()));
        if let Some(body) = &shown.rendered.body {
            dict.insert("body", Value::from(body.as_str()));
        }
        if let Some(level) = n.interruption {
            dict.insert("priority", Value::from(priority(level)));
        }
        match pick_image(n, shown.images, false) {
            Some(Picked::File(path)) => {
                if let Some(bytes) = read_image(path) {
                    dict.insert("icon", icon("bytes", Value::from(bytes)));
                } else {
                    tracing::debug!(path = %path.display(), "notifications: icon not read");
                }
            }
            Some(Picked::Named(name)) => {
                dict.insert("icon", icon("themed", Value::from(vec![name.to_owned()])));
            }
            None => {}
        }
        let user_data = n.user_data.as_deref();
        dict.insert("default-action", Value::from(action.as_str()));
        dict.insert(
            "default-action-target",
            target(&n.id, None, user_data, n.url.as_deref()),
        );

        let mut reply_button = false;
        let mut buttons: Vec<HashMap<&str, Value<'_>>> =
            Vec::with_capacity(shown.rendered.actions.len());
        for rendered in &shown.rendered.actions {
            let original = n
                .actions
                .iter()
                .find(|a| a.id.as_str() == rendered.key.split('#').next().unwrap_or_default());
            let url = original.and_then(|a| a.url.as_deref()).or(n.url.as_deref());
            let mut button: HashMap<&str, Value<'_>> = HashMap::with_capacity(4);
            button.insert("label", Value::from(rendered.title.as_str()));
            button.insert("action", Value::from(action.as_str()));
            button.insert(
                "target",
                target(&n.id, Some(rendered.key.as_str()), user_data, url),
            );
            if v2 {
                let purpose = match rendered.key.as_str() {
                    _ if rendered.input == Input::Reply => Some(REPLY_PURPOSE),
                    ActionId::CALL_ANSWER => Some("call.accept"),
                    ActionId::CALL_DECLINE => Some("call.decline"),
                    ActionId::CALL_HANG_UP => Some("call.hang-up"),
                    _ => None,
                }
                .filter(|p| self.supports_purpose(p));
                if let Some(purpose) = purpose {
                    reply_button |= purpose == REPLY_PURPOSE;
                    button.insert("purpose", Value::from(purpose));
                }
            }
            buttons.push(button);
        }
        if !buttons.is_empty() {
            dict.insert("buttons", Value::from(buttons));
        }

        if v2 {
            let category = match (&n.style, n.category) {
                (Style::Call(call), _) => Some(match call.kind {
                    CallKind::Ongoing => "call.ongoing",
                    CallKind::Incoming | CallKind::Screening => "call.incoming",
                }),
                (Style::Messaging(_), _) | (_, Some(Category::Message)) => Some("im.received"),
                _ if reply_button => Some("im.received"),
                (_, Some(Category::Alarm)) => Some("alarm.ringing"),
                _ => None,
            }
            .filter(|c| self.supports_category(c));
            if let Some(category) = category {
                dict.insert("category", Value::from(category));
            }
            let mut hints: Vec<&str> = Vec::with_capacity(3);
            if n.ongoing {
                hints.push("persistent");
            } else if n.timeout_ms.is_some() {
                hints.push("transient");
            }
            match n.lock_screen {
                Some(LockScreen::Hidden) => hints.push("hide-on-lock-screen"),
                Some(LockScreen::HideContent) => hints.push("hide-content-on-lock-screen"),
                _ => {}
            }
            if shown.mode == (ShowMode::Replace { alert: true }) {
                hints.push("show-as-new");
            }
            if !hints.is_empty() {
                dict.insert("display-hint", Value::from(hints));
            }
            let silent = matches!(n.sound, Some(Sound::Silent))
                || shown.mode == (ShowMode::Replace { alert: false });
            if silent {
                dict.insert("sound", Value::from("silent"));
            } else if matches!(n.sound, Some(Sound::Default)) {
                dict.insert("sound", Value::from("default"));
            }
        }
        self.conn
            .call_method(
                Some(NAME),
                PATH,
                Some(IFACE),
                "AddNotification",
                &(n.id.as_str(), dict),
            )
            .map_err(platform)?;
        lock(&self.shared.posted).insert(&n.id);

        Ok(())
    }

    fn remove(&self, id: &NotificationId) -> Result<(), NotificationError> {
        lock(&self.shared.posted).remove(id);
        self.conn
            .call_method(
                Some(NAME),
                PATH,
                Some(IFACE),
                "RemoveNotification",
                &(id.as_str(),),
            )
            .map(drop)
            .map_err(platform)
    }
}

const fn priority(level: Interruption) -> &'static str {
    match level {
        Interruption::Passive => "low",
        Interruption::Normal => "normal",
        Interruption::High => "high",
        Interruption::Urgent | Interruption::Critical => "urgent",
    }
}

/// The image bytes, refused above the plugin's image cap.
fn read_image(path: &std::path::Path) -> Option<Vec<u8>> {
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() > MAX_IMAGE_BYTES as u64 {
        return None;
    }
    std::fs::read(path).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn posted_ids_are_bounded_oldest_first() {
        let id = |i: usize| NotificationId::try_from(format!("n{i}")).unwrap();
        let mut posted = Posted::default();
        for i in 0..MAX_POSTED + 10 {
            posted.insert(&id(i));
            posted.insert(&id(i));
        }
        assert_eq!(posted.order.len(), MAX_POSTED);
        assert_eq!(posted.ids.len(), MAX_POSTED);
        assert!(!posted.contains(&id(9)));
        assert!(posted.contains(&id(10)));
        posted.remove(&id(10));
        assert!(!posted.contains(&id(10)));
        assert_eq!(posted.order.len(), MAX_POSTED - 1);
    }

    #[test]
    fn app_paths() {
        assert_eq!(app_path("org.example.App-1"), "/org/example/App_1");
    }

    #[test]
    fn info_reports_exported_actions() {
        let info = info(2, true, true, false);
        assert_eq!(info.capabilities.backend, "portal:2");
        assert_eq!(
            info.capabilities.support(Feature::TapWhenClosed),
            Support::Native
        );
        assert!(info.limits.inline_reply);
        assert!(!info.limits.call);
        let v1 = super::info(1, false, false, false);
        assert_eq!(
            v1.capabilities.support(Feature::Ongoing),
            Support::Unsupported
        );
        assert_eq!(
            v1.capabilities.support(Feature::BackgroundAction),
            Support::WhileRunning
        );
    }
}
