//! `org.freedesktop.Notifications` (spec 1.2) plus the KDE extensions
//! (`inline-reply`, `x-kde-*` hints) and, on Plasma, `kuiserver` job
//! views for notifications with progress.
//!
//! * Updates in place through `replaces_id`; a silent update adds
//!   `suppress-sound`.
//! * Capabilities and server information are read once and again after
//!   the server's bus name changes owner (restart), which also reports
//!   everything shown as dismissed: the new server has none of it.
//! * Signals are accepted only from the current owner of the name.
//! * The `value` progress hint goes only to servers known to draw it
//!   (dunst, mako, swaync, xfce4-notifyd); elsewhere progress is text.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, Weak};

use istmo_core::sync::lock;
use zbus::MatchRule;
use zbus::blocking::{Connection, MessageIterator};
use zbus::message::Type;
use zbus::zvariant::Value;

use super::{Owner, Picked, file_uri, kde_job, pick_image, platform, set_activation_token, spawn};
use crate::coalesce::Rate;
use crate::desktop::{Activation, Backend, BackendInfo, BackendSink, ShowMode, Shown};
use crate::render::{Input, RenderLimits};
use crate::{
    ActionKind, CallKind, Capabilities, Category, DismissReason, Feature, FeatureSupport,
    HintValue, Interruption, Notification, NotificationError, NotificationId, Sound, Style,
    Support,
};

const NAME: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";
/// The action key of the KDE inline-reply field.
const REPLY_KEY: &str = "inline-reply";
/// The action key of a click on the notification itself.
const DEFAULT_KEY: &str = "default";
/// Closes of ids `Notify` has not returned yet (the signal thread can
/// see them first).
const EARLY_CLOSES: usize = 16;
/// Buttons shown; the spec has no limit, servers show about three.
const MAX_ACTIONS: u32 = 3;

/// What the running server is.
#[derive(Debug)]
struct Server {
    caps: Vec<String>,
    /// Draws the `value` hint as a bar.
    value_hint: bool,
    /// Plasma with a job server: progress goes to job views.
    kde_jobs: bool,
    info: BackendInfo,
}

impl Server {
    fn has(&self, cap: &str) -> bool {
        self.caps.iter().any(|have| have == cap)
    }
}

#[derive(Debug)]
enum Entry {
    Note {
        server_id: u32,
        /// Rendered key of the action shown as the inline-reply field.
        reply_key: Option<String>,
        /// An action was invoked: the close that follows is no dismissal.
        activated: bool,
    },
    Job {
        path: String,
        /// Rendered key a cancel in the job view stands for.
        cancel_key: Option<String>,
    },
}

#[derive(Debug, Default)]
struct State {
    server: Option<Arc<Server>>,

    by_id: HashMap<NotificationId, Entry>,
    by_server: HashMap<u32, NotificationId>,
    by_job: HashMap<String, NotificationId>,
    early_closes: VecDeque<(u32, u32)>,
    /// The job-view cancel listener runs.
    job_listener: bool,
}

/// The freedesktop backend.
pub(super) struct Fdo {
    conn: Connection,
    app_id: String,
    sink: BackendSink,
    state: Mutex<State>,
    /// `show_job` spawns a listener holding the backend weakly.
    me: Weak<Self>,
    owner: Owner,
}

impl std::fmt::Debug for Fdo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Fdo")
            .field("app_id", &self.app_id)
            .finish_non_exhaustive()
    }
}

impl Fdo {
    pub(super) fn open(
        conn: Connection,
        app_id: String,
        sink: BackendSink,
    ) -> Result<Arc<Self>, NotificationError> {
        // Match rules go in before the first `Notify`.
        let signals = MessageIterator::for_match_rule(
            MatchRule::builder()
                .msg_type(Type::Signal)
                .interface(NAME)
                .and_then(|r| r.path(PATH))
                .map_err(platform)?
                .build(),
            &conn,
            Some(256),
        )
        .map_err(platform)?;
        let owners = zbus::blocking::fdo::DBusProxy::new(&conn)
            .and_then(|proxy| proxy.receive_name_owner_changed_with_args(&[(0, NAME)]))
            .map_err(platform)?;
        let fdo = Arc::new_cyclic(|me| Self {
            conn,
            app_id,
            sink,
            state: Mutex::default(),
            me: me.clone(),
            owner: Owner::new(NAME),
        });
        let weak = Arc::downgrade(&fdo);
        spawn("istmo-notif-fdo", move || {
            for message in signals {
                let Some(fdo) = weak.upgrade() else { return };
                if let Ok(message) = message {
                    fdo.signal(&message);
                }
            }
        })?;
        let weak = Arc::downgrade(&fdo);
        spawn("istmo-notif-owner", move || {
            for change in owners {
                let Some(fdo) = weak.upgrade() else { return };
                // A first owner (D-Bus activation by our `Notify`) is no
                // restart: only an owner that goes away loses state.
                if change.args().is_ok_and(|args| args.old_owner().is_none()) {
                    fdo.owner.forget();
                    continue;
                }
                fdo.server_restarted();
            }
        })?;
        Ok(fdo)
    }

    /// The server, read once per owner.
    fn server(&self) -> Result<Arc<Server>, NotificationError> {
        if let Some(server) = &lock(&self.state).server {
            return Ok(Arc::clone(server));
        }
        let caps: Vec<String> = self
            .conn
            .call_method(Some(NAME), PATH, Some(NAME), "GetCapabilities", &())
            .and_then(|reply| reply.body().deserialize())
            .map_err(platform)?;
        let (name, _vendor, version, _spec): (String, String, String, String) = self
            .conn
            .call_method(Some(NAME), PATH, Some(NAME), "GetServerInformation", &())
            .and_then(|reply| reply.body().deserialize())
            .map_err(platform)?;
        let lower = name.to_ascii_lowercase();
        let value_hint = ["dunst", "mako", "swaync", "swaynotificationcenter", "xfce"]
            .iter()
            .any(|known| lower.contains(known));
        let kde_jobs = lower == "plasma" && kde_job::available(&self.conn);
        let server = Arc::new(Server {
            info: info(&caps, &name, &version, value_hint || kde_jobs),
            caps,
            value_hint,
            kde_jobs,
        });
        let mut state = lock(&self.state);
        // A restart while reading: keep what the other thread stored.
        Ok(Arc::clone(state.server.get_or_insert(server)))
    }

    /// The server's bus name changed owner: what it showed is gone.
    fn server_restarted(&self) {
        let gone: Vec<NotificationId> = {
            let mut state = lock(&self.state);
            state.server = None;
            self.owner.forget();
            state.by_server.clear();
            state.early_closes.clear();
            let notes: Vec<NotificationId> = state
                .by_id
                .iter()
                .filter(|(_, entry)| matches!(entry, Entry::Note { .. }))
                .map(|(id, _)| id.clone())
                .collect();
            for id in &notes {
                state.by_id.remove(id);
            }
            notes
        };
        for id in &gone {
            self.sink.dismissed(id, DismissReason::Other);
        }
        self.sink.changed();
    }

    fn signal(&self, message: &zbus::Message) {
        let header = message.header();
        let (Some(member), Some(sender)) = (header.member(), header.sender()) else {
            return;
        };
        if !self.owner.is(&self.conn, sender.as_str()) {
            return;
        }
        let body = message.body();
        match member.as_str() {
            "ActionInvoked" => {
                if let Ok((server_id, key)) = body.deserialize::<(u32, String)>() {
                    self.action_invoked(server_id, &key, None);
                }
            }
            "NotificationReplied" => {
                if let Ok((server_id, text)) = body.deserialize::<(u32, String)>() {
                    self.action_invoked(server_id, REPLY_KEY, Some(text));
                }
            }
            "ActivationToken" => {
                if let Ok((_, token)) = body.deserialize::<(u32, String)>() {
                    set_activation_token(token);
                }
            }
            "NotificationClosed" => {
                if let Ok((server_id, reason)) = body.deserialize::<(u32, u32)>() {
                    self.closed(server_id, reason);
                }
            }
            _ => {}
        }
    }

    fn action_invoked(&self, server_id: u32, key: &str, reply: Option<String>) {
        let activation = {
            let mut state = lock(&self.state);
            let Some(id) = state.by_server.get(&server_id).cloned() else {
                return;
            };
            let Some(Entry::Note {
                reply_key,
                activated,
                ..
            }) = state.by_id.get_mut(&id)
            else {
                return;
            };
            *activated = true;
            let key = match key {
                DEFAULT_KEY => None,
                REPLY_KEY => reply_key.clone(),
                other => Some(other.to_owned()),
            };
            Activation {
                id,
                key,
                reply,
                launched_app: false,
                carried: None,
            }
        };
        self.sink.activated(activation);
    }

    fn closed(&self, server_id: u32, reason: u32) {
        let gone = {
            let mut state = lock(&self.state);
            if let Some(id) = state.by_server.remove(&server_id) {
                let activated = match state.by_id.remove(&id) {
                    Some(Entry::Note { activated, .. }) => activated,
                    Some(job @ Entry::Job { .. }) => {
                        // A job replaced the note meanwhile.
                        state.by_id.insert(id, job);
                        return;
                    }
                    None => false,
                };
                Some((id, activated))
            } else {
                if state.early_closes.len() == EARLY_CLOSES {
                    state.early_closes.pop_front();
                }
                state.early_closes.push_back((server_id, reason));
                None
            }
        };
        if let Some((id, activated)) = gone {
            self.sink.dismissed(&id, close_reason(reason, activated));
        }
    }

    /// A cancel in a job view.
    fn job_cancelled(&self, path: &str) {
        let (id, key) = {
            let mut state = lock(&self.state);
            let Some(id) = state.by_job.get(path).cloned() else {
                return;
            };
            let Some(Entry::Job { cancel_key, .. }) = state.by_id.get(&id) else {
                return;
            };
            let key = cancel_key.clone();
            if key.is_none() {
                state.by_job.remove(path);
                state.by_id.remove(&id);
            }
            (id, key)
        };
        if key.is_some() {
            // The app answers the cancel action and ends the job itself.
            self.sink.activated(Activation {
                id,
                key,
                reply: None,
                launched_app: false,
                carried: None,
            });
        } else {
            if let Err(err) = kde_job::terminate(&self.conn, path) {
                tracing::debug!(%err, "notifications: job view not ended");
            }
            self.sink.dismissed(&id, DismissReason::User);
        }
    }

    fn listen_jobs(self: &Arc<Self>) -> Result<(), NotificationError> {
        if std::mem::replace(&mut lock(&self.state).job_listener, true) {
            return Ok(());
        }
        let cancels = MessageIterator::for_match_rule(
            MatchRule::builder()
                .msg_type(Type::Signal)
                .interface(kde_job::VIEW_IFACE)
                .and_then(|r| r.member("cancelRequested"))
                .map_err(platform)?
                .build(),
            &self.conn,
            Some(64),
        )
        .map_err(platform)?;
        let weak: Weak<Self> = Arc::downgrade(self);
        spawn("istmo-notif-kjob", move || {
            for message in cancels {
                let Some(fdo) = weak.upgrade() else { return };
                if let Ok(message) = message
                    && let Some(path) = message.header().path()
                {
                    fdo.job_cancelled(path.as_str());
                }
            }
        })
    }

    fn close_note(&self, server_id: u32) -> Result<(), NotificationError> {
        self.conn
            .call_method(
                Some(NAME),
                PATH,
                Some(NAME),
                "CloseNotification",
                &(server_id,),
            )
            .map(drop)
            .map_err(platform)
    }

    fn show_job(self: &Arc<Self>, shown: &Shown<'_>) -> Result<(), NotificationError> {
        let n = shown.notification;
        let progress = n.progress.as_ref();
        let label = progress.and_then(|p| p.label.as_deref());
        let props = kde_job::JobProps {
            title: &n.title,
            info: label.or(shown.rendered.body.as_deref()),
            percent: progress.and_then(|p| p.value).map(percent).map(u32::from),
        };
        let cancel_key = shown
            .rendered
            .actions
            .iter()
            .find(|a| a.destructive)
            .map(|a| a.key.clone());
        let previous = {
            let mut state = lock(&self.state);
            match state.by_id.get_mut(&n.id) {
                Some(Entry::Job {
                    path,
                    cancel_key: key,
                }) => {
                    key.clone_from(&cancel_key);
                    Some(Ok(path.clone()))
                }
                Some(Entry::Note { server_id, .. }) => {
                    let server_id = *server_id;
                    state.by_server.remove(&server_id);
                    state.by_id.remove(&n.id);
                    Some(Err(server_id))
                }
                None => None,
            }
        };
        match previous {
            Some(Ok(path)) => return kde_job::update(&self.conn, &path, &props).map_err(platform),
            Some(Err(server_id)) => self.close_note(server_id)?,
            None => {}
        }
        self.listen_jobs()?;
        let path = kde_job::request(&self.conn, &self.app_id, &props).map_err(platform)?;
        let mut state = lock(&self.state);
        state.by_job.insert(path.clone(), n.id.clone());
        state
            .by_id
            .insert(n.id.clone(), Entry::Job { path, cancel_key });
        Ok(())
    }
}

impl Backend for Fdo {
    fn info(&self) -> BackendInfo {
        match self.server() {
            Ok(server) => server.info.clone(),
            Err(err) => {
                tracing::debug!(%err, "notifications: no notification server");
                let mut info = BackendInfo::basic("fdo");
                info.capabilities.max_actions = 0;
                info.limits.max_actions = 0;
                info
            }
        }
    }

    fn show(&self, shown: &Shown<'_>) -> Result<(), NotificationError> {
        let server = self.server()?;
        let n = shown.notification;
        if server.kde_jobs && n.progress.is_some() {
            // `show_job` needs the Arc for its listener thread.
            let Some(this) = self.me.upgrade() else {
                return Err(NotificationError::Unsupported(Feature::Show));
            };
            return this.show_job(shown);
        }
        let (replaces, job) = {
            let mut state = lock(&self.state);
            match state.by_id.get(&n.id) {
                Some(Entry::Note { server_id, .. }) => (*server_id, None),
                Some(Entry::Job { path, .. }) => {
                    let path = path.clone();
                    state.by_job.remove(&path);
                    state.by_id.remove(&n.id);
                    (0, Some(path))
                }
                None => (0, None),
            }
        };
        if let Some(path) = job
            && let Err(err) = kde_job::terminate(&self.conn, &path)
        {
            tracing::debug!(%err, "notifications: job view not ended");
        }
        let post = notify_args(&server, &self.app_id, shown);
        let server_id: u32 = self
            .conn
            .call_method(
                Some(NAME),
                PATH,
                Some(NAME),
                "Notify",
                &(
                    self.app_id.as_str(),
                    replaces,
                    "",
                    n.title.as_str(),
                    post.body.as_str(),
                    &post.actions,
                    &post.hints,
                    post.expire,
                ),
            )
            .and_then(|reply| reply.body().deserialize())
            .map_err(platform)?;
        let early = {
            let mut state = lock(&self.state);
            if replaces != 0 && replaces != server_id {
                state.by_server.remove(&replaces);
            }
            state.by_server.insert(server_id, n.id.clone());
            state.by_id.insert(
                n.id.clone(),
                Entry::Note {
                    server_id,
                    reply_key: post.reply_key,
                    activated: false,
                },
            );
            let early = state
                .early_closes
                .iter()
                .position(|(seen, _)| *seen == server_id)
                .and_then(|at| state.early_closes.remove(at));
            if early.is_some() {
                state.by_server.remove(&server_id);
                state.by_id.remove(&n.id);
            }
            early
        };
        if let Some((_, reason)) = early {
            self.sink.dismissed(&n.id, close_reason(reason, false));
        }
        Ok(())
    }

    fn remove(&self, id: &NotificationId) -> Result<(), NotificationError> {
        let entry = {
            let mut state = lock(&self.state);
            let entry = state.by_id.remove(id);
            match &entry {
                Some(Entry::Note { server_id, .. }) => {
                    state.by_server.remove(server_id);
                }
                Some(Entry::Job { path, .. }) => {
                    state.by_job.remove(path);
                }
                None => {}
            }
            entry
        };
        match entry {
            Some(Entry::Note { server_id, .. }) => self.close_note(server_id),
            Some(Entry::Job { path, .. }) => {
                kde_job::terminate(&self.conn, &path).map_err(platform)
            }
            None => Ok(()),
        }
    }
}

/// `Notify` arguments besides app name, replaces id, icon and summary.
struct Post<'a> {
    body: String,
    actions: Vec<&'a str>,
    hints: HashMap<&'a str, Value<'a>>,
    expire: i32,
    reply_key: Option<String>,
}

#[expect(
    clippy::too_many_lines,
    reason = "one hint after another; splitting scatters the mapping table"
)]
fn notify_args<'a>(server: &Server, app_id: &'a str, shown: &Shown<'a>) -> Post<'a> {
    let n = shown.notification;
    let mut hints: HashMap<&'a str, Value<'a>> = HashMap::new();
    let mut actions = Vec::new();
    let mut reply_key = None;
    if server.has("actions") {
        actions.extend([DEFAULT_KEY, ""]);
        let inline = server.has(REPLY_KEY);
        for action in &shown.rendered.actions {
            if action.input == Input::Reply && inline && reply_key.is_none() {
                reply_key = Some(action.key.clone());
                actions.extend([REPLY_KEY, action.title.as_str()]);
                let options = n.actions.iter().find_map(|a| match &a.kind {
                    ActionKind::Reply(options) if a.id.as_str() == action.key => Some(options),
                    _ => None,
                });
                if let Some(options) = options {
                    if let Some(text) = &options.placeholder {
                        hints.insert("x-kde-reply-placeholder-text", Value::from(text.as_str()));
                    }
                    if let Some(text) = &options.submit_title {
                        hints.insert("x-kde-reply-submit-button-text", Value::from(text.as_str()));
                    }
                }
            } else {
                actions.extend([action.key.as_str(), action.title.as_str()]);
            }
        }
    }

    let mut body = shown.rendered.body.clone().unwrap_or_default();
    if server.value_hint
        && let Some(progress) = &n.progress
    {
        if let Some(value) = progress.value {
            hints.insert("value", Value::I32(i32::from(percent(value))));
        }
        // The bar replaced the text line; the label stays readable.
        if let Some(label) = progress.label.as_deref().filter(|l| !l.is_empty()) {
            if !body.is_empty() {
                body.push('\n');
            }
            body.push_str(label);
        }
    }
    if server.has("body-markup") {
        body = escape_markup(&body);
    }

    if let Some(level) = n.interruption {
        hints.insert("urgency", Value::U8(urgency(level)));
    }
    if let Some(category) = category(n) {
        hints.insert("category", Value::from(category));
    }
    hints.insert("desktop-entry", Value::from(app_id));
    match pick_image(n, shown.images, true) {
        Some(Picked::File(path)) => {
            let uri = file_uri(path);
            if server.has("x-kde-urls") && matches!(n.style, Style::Picture(_)) {
                hints.insert("x-kde-urls", Value::from(vec![uri.clone()]));
            }
            hints.insert("image-path", Value::from(uri));
        }
        Some(Picked::Named(name)) => {
            hints.insert("image-path", Value::from(name.to_owned()));
        }
        None => {}
    }
    let silent = matches!(n.sound, Some(Sound::Silent))
        || matches!(shown.mode, ShowMode::Replace { alert: false });
    if silent {
        hints.insert("suppress-sound", Value::from(true));
    } else if let Some(Sound::Named(name)) = &n.sound {
        if name.contains('/') {
            hints.insert("sound-file", Value::from(name.as_str()));
        } else {
            hints.insert("sound-name", Value::from(name.as_str()));
        }
    }
    if n.ongoing {
        hints.insert("resident", Value::from(true));
    } else if n.timeout_ms.is_some() {
        // It goes away on its own; no point keeping it in the history.
        hints.insert("transient", Value::from(true));
    }
    for hint in &n.linux_hints {
        let value = match &hint.value {
            HintValue::Bool(b) => Value::from(*b),
            HintValue::Number(v) => Value::from(*v),
            HintValue::Text(text) => Value::from(text.as_str()),
            HintValue::Bytes(bytes) => Value::from(bytes.clone()),
        };
        hints.insert(hint.key.as_str(), value);
    }
    let expire = if n.ongoing {
        0
    } else {
        n.timeout_ms
            .map_or(-1, |ms| i32::try_from(ms).unwrap_or(i32::MAX))
    };
    Post {
        body,
        actions,
        hints,
        expire,
        reply_key,
    }
}

/// What the server does natively.
fn info(caps: &[String], name: &str, version: &str, progress_bar: bool) -> BackendInfo {
    let has = |cap: &str| caps.iter().any(|have| have == cap);
    let actions = has("actions");
    let mut features = Vec::with_capacity(14);
    let mut add = |feature, support| features.push(FeatureSupport { feature, support });
    for feature in [
        Feature::Show,
        Feature::Update,
        Feature::SilentUpdate,
        Feature::Interruption,
        Feature::Ongoing,
        Feature::Avatar,
        Feature::Picture,
    ] {
        add(feature, Support::Native);
    }
    if has("sound") {
        add(Feature::CustomSound, Support::Native);
    }
    if actions {
        add(Feature::Actions, Support::Native);
        add(Feature::BackgroundAction, Support::WhileRunning);
        if has(REPLY_KEY) {
            add(Feature::InlineReply, Support::Native);
        }
    }
    if progress_bar {
        add(Feature::ProgressBar, Support::Native);
    }
    add(Feature::Dismissal, Support::WhileRunning);
    let backend = if version.is_empty() {
        format!("fdo:{name}")
    } else {
        format!("fdo:{name} {version}")
    };
    let mut capabilities = Capabilities::none(backend);
    capabilities.features = features;
    capabilities.max_actions = if actions { MAX_ACTIONS } else { 0 };
    capabilities.max_pending = None;
    capabilities.max_active = None;
    BackendInfo {
        limits: RenderLimits::from_capabilities(&capabilities),
        capabilities,
        rate: Rate::LINUX,
        lists_delivered: false,
        timeouts: false,
    }
}

/// `NotificationClosed` reasons: 1 expired, 2 dismissed, 3 closed by
/// `CloseNotification`, 4 undefined. After an action the close is the
/// server tidying up, not a dismissal.
const fn close_reason(reason: u32, activated: bool) -> DismissReason {
    if activated {
        return DismissReason::Other;
    }
    match reason {
        1 => DismissReason::Timeout,
        2 => DismissReason::User,
        3 => DismissReason::App,
        _ => DismissReason::Other,
    }
}

const fn urgency(level: Interruption) -> u8 {
    match level {
        Interruption::Passive => 0,
        Interruption::Normal | Interruption::High => 1,
        Interruption::Urgent | Interruption::Critical => 2,
    }
}

/// The spec's category of `n`, when one fits.
fn category(n: &Notification) -> Option<&'static str> {
    match &n.style {
        Style::Call(call) => {
            return Some(match call.kind {
                CallKind::Ongoing => "call",
                CallKind::Incoming | CallKind::Screening => "call.incoming",
            });
        }
        Style::Messaging(_) => return Some("im.received"),
        _ => {}
    }
    match n.category? {
        Category::Message => Some("im.received"),
        Category::Call => Some("call"),
        Category::Transfer => Some("transfer"),
        _ => None,
    }
}

/// `0..=1` as a whole percent, truncated (99.9 % is not done).
fn percent(value: f32) -> u8 {
    let value = if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    };
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "value is clamped to 0..=1, so the product fits in u8"
    )]
    let percent = (value * 100.0).floor() as u8;
    percent
}

/// Body markup is a small XML subset: plain text must escape `&<>`.
fn escape_markup(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_reasons() {
        assert_eq!(close_reason(1, false), DismissReason::Timeout);
        assert_eq!(close_reason(2, false), DismissReason::User);
        assert_eq!(close_reason(3, false), DismissReason::App);
        assert_eq!(close_reason(2, true), DismissReason::Other);
        assert_eq!(close_reason(9, false), DismissReason::Other);
    }

    #[test]
    fn markup_and_percent() {
        assert_eq!(escape_markup("a<b>&c"), "a&lt;b&gt;&amp;c");
        assert_eq!(percent(0.999), 99);
        assert_eq!(percent(f32::NAN), 0);
        assert_eq!(percent(2.0), 100);
    }

    #[test]
    fn info_follows_capabilities() {
        let caps = ["actions", "body", "inline-reply"].map(str::to_owned);
        let info = info(&caps, "Plasma", "6.2", true);
        assert_eq!(info.capabilities.backend, "fdo:Plasma 6.2");
        assert!(info.limits.inline_reply);
        assert!(info.limits.progress_bar);
        assert_eq!(info.limits.max_actions, 3);
        let bare = super::info(&[], "x", "1", false);
        assert_eq!(bare.limits.max_actions, 0);
        assert_eq!(
            bare.capabilities.support(Feature::Actions),
            Support::Unsupported
        );
    }
}
