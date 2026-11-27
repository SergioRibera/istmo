//! The Linux freedesktop backend against a fake notification server on a
//! private session bus.
//!
//! The parent test starts its own `dbus-daemon` and runs the real check
//! in a child copy of this binary whose `DBUS_SESSION_BUS_ADDRESS` points
//! at it (no process-wide `set_var`). Skipped when `dbus-daemon` is not
//! installed. `ISTMO_NOTIF_TEST_LIVE=1` also runs a smoke test against the
//! real session bus (a notification appears for a few seconds).

#![cfg(target_os = "linux")]
#![allow(
    missing_docs,
    reason = "the zbus interface macro emits undocumented methods for the fake servers"
)]

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use istmo_core::Runtime;
use istmo_notifications::DesktopNotifications;
use istmo_notifications::desktop::linux::{LinuxOptions, LinuxRoute, take_activation_token};
use istmo_notifications::{
    Action, DismissReason, Dismissal, Feature, Interruption, Notification, NotificationEvent,
    NotificationId, NotificationResponseStream, NotificationResponses, Notifications,
    NotificationsClient, NotificationsHost, Progress, ReplyOptions, Response, Sound, Support,
};
use zbus::blocking::Connection;
use zbus::interface;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::OwnedValue;

const CHILD: &str = "ISTMO_NOTIF_DBUS_CHILD";
const NAME: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";
const TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug)]
enum Call {
    Notify {
        replaces: u32,
        app: String,
        summary: String,
        body: String,
        actions: Vec<String>,
        hints: HashMap<String, OwnedValue>,
        expire: i32,
    },
    Close(u32),
}

struct Fake {
    name: &'static str,
    caps: Vec<String>,
    next: Mutex<u32>,
    calls: flume::Sender<Call>,
}

#[allow(
    clippy::too_many_arguments,
    clippy::needless_pass_by_value,
    clippy::unused_self,
    clippy::used_underscore_binding,
    clippy::missing_const_for_fn,
    reason = "the D-Bus method shapes of the spec"
)]
#[interface(name = "org.freedesktop.Notifications")]
impl Fake {
    fn get_capabilities(&self) -> Vec<String> {
        self.caps.clone()
    }

    fn get_server_information(&self) -> (String, String, String, String) {
        (
            self.name.to_owned(),
            "istmo".to_owned(),
            "1.9".to_owned(),
            "1.2".to_owned(),
        )
    }

    fn notify(
        &self,
        app_name: String,
        replaces_id: u32,
        app_icon: String,
        summary: String,
        body: String,
        actions: Vec<String>,
        hints: HashMap<String, OwnedValue>,
        expire_timeout: i32,
    ) -> u32 {
        drop(app_icon);
        let id = if replaces_id == 0 {
            let mut next = self.next.lock().unwrap();
            *next += 1;
            *next
        } else {
            replaces_id
        };
        let _sent = self.calls.send(Call::Notify {
            replaces: replaces_id,
            app: app_name,
            summary,
            body,
            actions,
            hints,
            expire: expire_timeout,
        });
        id
    }

    async fn close_notification(
        &self,
        id: u32,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) {
        let _sent = self.calls.send(Call::Close(id));
        let _ = Self::notification_closed(&emitter, id, 3).await;
    }

    #[zbus(signal)]
    async fn notification_closed(
        emitter: &SignalEmitter<'_>,
        id: u32,
        reason: u32,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn action_invoked(emitter: &SignalEmitter<'_>, id: u32, key: &str) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn activation_token(
        emitter: &SignalEmitter<'_>,
        id: u32,
        token: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn notification_replied(
        emitter: &SignalEmitter<'_>,
        id: u32,
        text: &str,
    ) -> zbus::Result<()>;
}

fn fake_server(name: &'static str, caps: &[&str]) -> (Connection, flume::Receiver<Call>) {
    let (tx, rx) = flume::unbounded();
    let fake = Fake {
        name,
        caps: caps.iter().map(|c| (*c).to_owned()).collect(),
        next: Mutex::new(0),
        calls: tx,
    };
    let conn = zbus::blocking::connection::Builder::session()
        .unwrap()
        .serve_at(PATH, fake)
        .unwrap()
        .name(NAME)
        .unwrap()
        .build()
        .unwrap();
    (conn, rx)
}

fn emit(conn: &Connection, f: impl FnOnce(&SignalEmitter<'_>) -> zbus::Result<()>) {
    let iface = conn.object_server().interface::<_, Fake>(PATH).unwrap();
    f(iface.signal_emitter()).unwrap();
}

fn run_child(test: &str) {
    let Ok(mut daemon) = Command::new("dbus-daemon")
        .args(["--session", "--nofork", "--nopidfile", "--print-address=1"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        eprintln!("dbus-daemon not installed: {test} skipped");
        return;
    };
    let mut address = String::new();
    let stdout = daemon.stdout.take().expect("piped stdout");
    BufReader::new(stdout)
        .read_line(&mut address)
        .expect("bus address");
    let status = Command::new(std::env::current_exe().expect("test binary"))
        .args(["--exact", test, "--nocapture"])
        .env(CHILD, "1")
        .env("DBUS_SESSION_BUS_ADDRESS", address.trim())
        .env_remove("FLATPAK_ID")
        .status()
        .expect("run child");
    let _ = daemon.kill();
    let _ = daemon.wait();
    assert!(status.success(), "child failed: {status}");
}

struct Setup {
    host: DesktopNotifications,
    client: NotificationsClient,
    responses: NotificationResponseStream,
    _rt: Arc<Runtime>,
    dir: std::path::PathBuf,
}

impl Drop for Setup {
    fn drop(&mut self) {
        let _cleanup = std::fs::remove_dir_all(&self.dir);
    }
}

fn setup(options: LinuxOptions) -> Setup {
    let dir = std::env::temp_dir().join(format!("istmo-notif-linux-{}", std::process::id()));
    let host = DesktopNotifications::builder(options.start())
        .without_store()
        .images_in(dir.join("images"))
        .build();
    let rt = Runtime::mock()
        .host(NotificationsHost::new(host.clone()))
        .runtime;
    host.attach(&rt);
    let client = NotificationsClient::from_runtime(&rt).unwrap();
    let responses = NotificationResponses::from_runtime(&rt).unwrap().stream();
    Setup {
        host,
        client,
        responses,
        _rt: rt,
        dir,
    }
}

fn next_call(calls: &flume::Receiver<Call>) -> Call {
    calls.recv_timeout(TIMEOUT).expect("a call to the server")
}

fn next_response(stream: &NotificationResponseStream) -> istmo_notifications::NotificationResponse {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if let Some(response) = stream.try_recv() {
            return response.unwrap();
        }
        assert!(Instant::now() < deadline, "no response");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn next_dismissal(events: &flume::Receiver<NotificationEvent>) -> Dismissal {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if let NotificationEvent::Dismissed(dismissal) =
            events.recv_timeout(left).expect("a dismissal")
        {
            return dismissal;
        }
    }
}

fn int(hints: &HashMap<String, OwnedValue>, key: &str) -> Option<i64> {
    let value = hints.get(key)?;
    u8::try_from(value)
        .map(i64::from)
        .or_else(|_| i32::try_from(value).map(i64::from))
        .ok()
}

fn text(hints: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    hints
        .get(key)
        .and_then(|v| String::try_from(v.clone()).ok())
}

fn boolean(hints: &HashMap<String, OwnedValue>, key: &str) -> Option<bool> {
    hints.get(key).and_then(|v| bool::try_from(v).ok())
}

fn id(value: &str) -> NotificationId {
    NotificationId::try_from(value).unwrap()
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one scenario against one server, step by step"
)]
fn fdo_backend_against_a_fake_server() {
    if std::env::var_os(CHILD).is_none() {
        return run_child("fdo_backend_against_a_fake_server");
    }
    let (server, calls) = fake_server(
        "mako",
        &[
            "actions",
            "body",
            "body-markup",
            "inline-reply",
            "persistence",
        ],
    );
    let s = setup(
        LinuxOptions::new()
            .with_app_id("dev.istmo.notif_test")
            .with_route(LinuxRoute::Server),
    );
    let events = s.host.events();

    // Capabilities follow the server.
    let caps = pollster::block_on(s.client.capabilities()).unwrap();
    assert_eq!(caps.backend, "fdo:mako 1.9");
    assert_eq!(caps.support(Feature::ProgressBar), Support::Native);
    assert_eq!(caps.support(Feature::InlineReply), Support::Native);
    assert_eq!(caps.support(Feature::Actions), Support::Native);

    // Show: summary, escaped body with the progress label, actions with
    // the reply field, hints.
    let mut n =
        Notification::new("dl", "Report")
            .unwrap()
            .with_body("a <b> & c")
            .with_progress(Progress::fraction(0.5).with_label("Downloading"))
            .with_sound(Sound::Silent)
            .with_action(Action::reply("reply", "Reply").unwrap().with_reply_options(
                ReplyOptions {
                    placeholder: Some("Say something".to_owned()),
                    submit_title: Some("Send".to_owned()),
                    suggestions: Vec::new(),
                },
            ))
            .with_action(Action::button("pause", "Pause").unwrap());
    n.interruption = Some(Interruption::Urgent);
    pollster::block_on(s.client.show_checked(n)).unwrap();
    let Call::Notify {
        replaces,
        app,
        summary,
        body,
        actions,
        hints,
        expire,
    } = next_call(&calls)
    else {
        panic!("expected Notify");
    };
    assert_eq!(replaces, 0);
    assert_eq!(app, "dev.istmo.notif_test");
    assert_eq!(summary, "Report");
    assert_eq!(body, "a &lt;b&gt; &amp; c\nDownloading");
    assert_eq!(
        actions,
        ["default", "", "inline-reply", "Reply", "pause", "Pause"]
    );
    assert_eq!(int(&hints, "urgency"), Some(2));
    assert_eq!(int(&hints, "value"), Some(50));
    assert_eq!(boolean(&hints, "suppress-sound"), Some(true));
    assert_eq!(
        text(&hints, "desktop-entry").as_deref(),
        Some("dev.istmo.notif_test")
    );
    assert_eq!(
        text(&hints, "x-kde-reply-placeholder-text").as_deref(),
        Some("Say something")
    );
    assert_eq!(
        text(&hints, "x-kde-reply-submit-button-text").as_deref(),
        Some("Send")
    );
    assert_eq!(expire, -1);

    // A progress update replaces in place, silently.
    pollster::block_on(
        s.client
            .set_progress(id("dl"), Some(Progress::fraction(0.75))),
    )
    .unwrap();
    let Call::Notify {
        replaces, hints, ..
    } = next_call(&calls)
    else {
        panic!("expected Notify");
    };
    assert_eq!(replaces, 1);
    assert_eq!(int(&hints, "value"), Some(75));
    assert_eq!(boolean(&hints, "suppress-sound"), Some(true));

    // A button, with its activation token.
    emit(&server, |e| {
        zbus::block_on(Fake::activation_token(e, 1, "token-1"))?;
        zbus::block_on(Fake::action_invoked(e, 1, "pause"))
    });
    let response = next_response(&s.responses);
    assert_eq!(response.id, id("dl"));
    assert_eq!(response.kind(), Response::Action("pause"));
    assert_eq!(take_activation_token().as_deref(), Some("token-1"));

    // The inline reply.
    emit(&server, |e| {
        zbus::block_on(Fake::notification_replied(e, 1, "on my way"))
    });
    let response = next_response(&s.responses);
    assert_eq!(response.kind(), Response::Reply("on my way"));

    // The default action is the notification itself.
    emit(&server, |e| {
        zbus::block_on(Fake::action_invoked(e, 1, "default"))
    });
    assert_eq!(next_response(&s.responses).kind(), Response::Open);

    // The close after an action is no user dismissal.
    emit(&server, |e| {
        zbus::block_on(Fake::notification_closed(e, 1, 2))
    });
    assert_eq!(
        next_dismissal(&events),
        Dismissal {
            id: id("dl"),
            reason: DismissReason::Other
        }
    );

    // A second notification: dismissed by the user, then one that times out.
    pollster::block_on(s.client.show_checked(Notification::new("b", "B").unwrap())).unwrap();
    let Call::Notify { replaces: 0, .. } = next_call(&calls) else {
        panic!("expected a new Notify");
    };
    emit(&server, |e| {
        zbus::block_on(Fake::notification_closed(e, 2, 2))
    });
    assert_eq!(next_dismissal(&events).reason, DismissReason::User);
    pollster::block_on(
        s.client.show_checked(
            Notification::new("c", "C")
                .unwrap()
                .with_timeout(Duration::from_secs(4)),
        ),
    )
    .unwrap();
    let Call::Notify { expire, hints, .. } = next_call(&calls) else {
        panic!("expected Notify");
    };
    assert_eq!(expire, 4000);
    assert_eq!(boolean(&hints, "transient"), Some(true));
    emit(&server, |e| {
        zbus::block_on(Fake::notification_closed(e, 3, 1))
    });
    assert_eq!(next_dismissal(&events).reason, DismissReason::Timeout);

    // Cancel closes it on the server.
    pollster::block_on(s.client.show_checked(Notification::new("d", "D").unwrap())).unwrap();
    let _notify = next_call(&calls);
    pollster::block_on(s.client.cancel(id("d"))).unwrap();
    assert!(matches!(next_call(&calls), Call::Close(4)));

    // Signals from anyone but the server are ignored.
    pollster::block_on(s.client.show_checked(Notification::new("e", "E").unwrap())).unwrap();
    let _notify = next_call(&calls);
    let (spoof, _spoof_calls) = {
        let (tx, rx) = flume::unbounded();
        let conn = zbus::blocking::connection::Builder::session()
            .unwrap()
            .serve_at(
                PATH,
                Fake {
                    name: "spoof",
                    caps: Vec::new(),
                    next: Mutex::new(0),
                    calls: tx,
                },
            )
            .unwrap()
            .build()
            .unwrap();
        (conn, rx)
    };
    emit(&spoof, |e| {
        zbus::block_on(Fake::action_invoked(e, 5, "default"))
    });
    std::thread::sleep(Duration::from_millis(200));
    assert!(s.responses.try_recv().is_none(), "spoofed signal accepted");

    // The server restarts with other capabilities: what it showed is
    // gone, the new capabilities are read.
    drop(server);
    assert_eq!(
        next_dismissal(&events),
        Dismissal {
            id: id("e"),
            reason: DismissReason::Other
        }
    );
    // The host reads the info again once the backend sees the new owner
    // (a bus signal, then a queued report).
    let (_server, calls) = fake_server("Plain", &["body"]);
    let deadline = Instant::now() + TIMEOUT;
    let caps = loop {
        s.host.settle();
        let caps = pollster::block_on(s.client.capabilities()).unwrap();
        if caps.backend != "fdo" {
            break caps;
        }
        assert!(Instant::now() < deadline, "info not read again");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(caps.backend, "fdo:Plain 1.9");
    assert_eq!(caps.support(Feature::InlineReply), Support::Unsupported);
    assert_eq!(caps.support(Feature::Actions), Support::Unsupported);
    // No `value` support: progress is text; no actions.
    pollster::block_on(
        s.client.show_checked(
            Notification::new("f", "F")
                .unwrap()
                .with_progress(Progress::fraction(0.25)),
        ),
    )
    .unwrap();
    let Call::Notify {
        body,
        actions,
        hints,
        ..
    } = next_call(&calls)
    else {
        panic!("expected Notify");
    };
    assert_eq!(body, "25%");
    assert!(actions.is_empty());
    assert_eq!(int(&hints, "value"), None);
}

/// A fake `org.kde.JobViewServer`: hands out view paths.
struct FakeJobs {
    next: Mutex<u32>,
    views: flume::Sender<String>,
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "the D-Bus method shape of JobViewServerV2"
)]
#[interface(name = "org.kde.JobViewServerV2")]
impl FakeJobs {
    #[zbus(name = "requestView")]
    fn request_view(
        &self,
        desktop_entry: String,
        capabilities: i32,
        hints: HashMap<String, OwnedValue>,
    ) -> zbus::zvariant::OwnedObjectPath {
        drop((desktop_entry, capabilities, hints));
        let number = {
            let mut next = self.next.lock().unwrap();
            *next += 1;
            *next
        };
        let path = format!("/JobView/{number}");

        let _sent = self.views.send(path.clone());
        zbus::zvariant::OwnedObjectPath::try_from(path).unwrap()
    }
}

fn fake_jobs() -> (Connection, flume::Receiver<String>) {
    let (tx, rx) = flume::unbounded();
    let conn = zbus::blocking::connection::Builder::session()
        .unwrap()
        .serve_at(
            "/JobViewServer",
            FakeJobs {
                next: Mutex::new(0),
                views: tx,
            },
        )
        .unwrap()
        .name("org.kde.JobViewServer")
        .unwrap()
        .build()
        .unwrap();
    (conn, rx)
}

fn cancel_requested(conn: &Connection, path: &str) {
    conn.emit_signal(
        None::<&str>,
        path,
        "org.kde.JobViewV3",
        "cancelRequested",
        &(),
    )
    .unwrap();
}

#[test]
fn kde_job_views_follow_their_server() {
    if std::env::var_os(CHILD).is_none() {
        return run_child("kde_job_views_follow_their_server");
    }
    let (_server, _calls) = fake_server("Plasma", &["actions", "body"]);
    let (jobs, views) = fake_jobs();
    let s = setup(
        LinuxOptions::new()
            .with_app_id("dev.istmo.notif_test")
            .with_route(LinuxRoute::Server),
    );
    let events = s.host.events();
    let progress = |name: &str| {
        Notification::new(name, "Copying")
            .unwrap()
            .with_progress(Progress::fraction(0.5))
    };
    pollster::block_on(s.client.show_checked(progress("j1"))).unwrap();
    let view = views.recv_timeout(TIMEOUT).expect("a job view");
    // A cancel from anyone but the job server is ignored.
    let spoofer = Connection::session().unwrap();
    cancel_requested(&spoofer, &view);
    std::thread::sleep(Duration::from_millis(300));
    s.host.settle();
    assert!(
        !events
            .try_iter()
            .any(|e| matches!(e, NotificationEvent::Dismissed(_))),
        "a spoofed cancel ended the job"
    );
    cancel_requested(&jobs, &view);
    let dismissal = next_dismissal(&events);
    assert_eq!(dismissal.id, id("j1"));
    assert_eq!(dismissal.reason, DismissReason::User);

    // The job server goes away: its views are gone, the host hears so.
    pollster::block_on(s.client.show_checked(progress("j2"))).unwrap();
    let _second = views.recv_timeout(TIMEOUT).expect("a second job view");
    drop(jobs);
    let dismissal = next_dismissal(&events);
    assert_eq!(dismissal.id, id("j2"));
    assert_eq!(dismissal.reason, DismissReason::Other);
}

#[test]
fn live_session_smoke() {
    // `1`: the automatic route; `portal`: the notification portal.
    let route = match std::env::var("ISTMO_NOTIF_TEST_LIVE").as_deref() {
        Ok("portal") => LinuxRoute::Portal,
        Ok(_) => LinuxRoute::Auto,
        Err(_) => return,
    };
    let s = setup(
        LinuxOptions::new()
            .with_app_id("dev.istmo.notif_live")
            .with_route(route),
    );
    let caps = pollster::block_on(s.client.capabilities()).unwrap();
    eprintln!("live backend: {} {:?}", caps.backend, caps.features);
    let n = Notification::new("live", "istmo notifications")
        .unwrap()
        .with_body("Linux backend smoke test")
        .with_progress(Progress::fraction(0.1).with_label("Working"))
        .with_action(Action::button("ok", "OK").unwrap());
    pollster::block_on(s.client.show_checked(n)).unwrap();
    for step in 2..=9_u8 {
        std::thread::sleep(Duration::from_millis(300));
        pollster::block_on(
            s.client
                .set_progress(id("live"), Some(Progress::fraction(f32::from(step) / 10.0))),
        )
        .unwrap();
    }
    std::thread::sleep(Duration::from_secs(1));
    pollster::block_on(s.client.cancel(id("live"))).unwrap();
}

const PORTAL: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";

type Dict = HashMap<String, OwnedValue>;

struct FakePortal {
    calls: flume::Sender<(String, Option<Dict>)>,
}

#[allow(
    clippy::needless_pass_by_value,
    clippy::unused_self,
    clippy::used_underscore_binding,
    clippy::missing_const_for_fn,
    reason = "the D-Bus method shapes of the portal"
)]
#[interface(name = "org.freedesktop.portal.Notification")]
impl FakePortal {
    fn add_notification(&self, id: String, notification: Dict) {
        let _sent = self.calls.send((id, Some(notification)));
    }

    fn remove_notification(&self, id: String) {
        let _sent = self.calls.send((id, None));
    }

    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        2
    }

    #[zbus(property, name = "SupportedOptions")]
    fn supported_options(&self) -> Dict {
        let list = |items: &[&str]| {
            OwnedValue::try_from(zbus::zvariant::Value::from(
                items.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(),
            ))
            .unwrap()
        };
        HashMap::from([
            (
                "category".to_owned(),
                list(&["im.received", "call.incoming"]),
            ),
            ("button-purpose".to_owned(), list(&["im.reply-with-text"])),
        ])
    }

    #[zbus(signal)]
    async fn action_invoked(
        emitter: &SignalEmitter<'_>,
        id: &str,
        action: &str,
        parameter: Vec<OwnedValue>,
    ) -> zbus::Result<()>;
}

fn dict_of(value: &OwnedValue) -> Dict {
    Dict::try_from(value.try_clone().unwrap()).unwrap()
}

#[test]
fn portal_backend_against_a_fake_portal() {
    if std::env::var_os(CHILD).is_none() {
        return run_child("portal_backend_against_a_fake_portal");
    }
    let (tx, calls) = flume::unbounded();
    let portal = zbus::blocking::connection::Builder::session()
        .unwrap()
        .serve_at(PORTAL_PATH, FakePortal { calls: tx })
        .unwrap()
        .name(PORTAL)
        .unwrap()
        .build()
        .unwrap();
    let app_id = "dev.istmo.portal_test";
    let s = setup(
        LinuxOptions::new()
            .with_app_id(app_id)
            .with_route(LinuxRoute::Portal),
    );
    let caps = pollster::block_on(s.client.capabilities()).unwrap();
    assert_eq!(caps.backend, "portal:2");
    assert_eq!(caps.support(Feature::InlineReply), Support::Native);
    assert_eq!(caps.support(Feature::TapWhenClosed), Support::Native);

    let n = Notification::new("chat", "Ana")
        .unwrap()
        .with_body("Lunch?")
        .with_user_data("thread=7")
        .with_action(Action::reply("reply", "Reply").unwrap())
        .with_action(Action::button("later", "Later").unwrap());
    pollster::block_on(s.client.show_checked(n)).unwrap();
    let (id_sent, dict) = calls.recv_timeout(TIMEOUT).unwrap();
    let dict = dict.expect("AddNotification");
    assert_eq!(id_sent, "chat");
    assert_eq!(
        String::try_from(dict["title"].try_clone().unwrap()).unwrap(),
        "Ana"
    );
    assert_eq!(
        String::try_from(dict["default-action"].try_clone().unwrap()).unwrap(),
        "app.istmo-notification"
    );
    assert_eq!(
        String::try_from(dict["category"].try_clone().unwrap()).unwrap(),
        "im.received"
    );
    let buttons = Vec::<Dict>::try_from(dict["buttons"].try_clone().unwrap()).expect("buttons");
    assert_eq!(buttons.len(), 2);
    assert_eq!(
        String::try_from(buttons[0]["purpose"].try_clone().unwrap()).unwrap(),
        "im.reply-with-text"
    );
    assert!(!buttons[1].contains_key("purpose"));
    let target = buttons[0]["target"].try_clone().unwrap();
    assert_eq!(
        String::try_from(dict_of(&target)["istmo-user-data"].try_clone().unwrap()).unwrap(),
        "thread=7"
    );

    // An exported action: the portal calls ActivateAction on the app id.
    let observer = Connection::session().unwrap();
    let platform: HashMap<&str, zbus::zvariant::Value<'_>> =
        HashMap::from([("activation-token", zbus::zvariant::Value::from("tok-2"))]);
    observer
        .call_method(
            Some(app_id),
            "/dev/istmo/portal_test",
            Some("org.freedesktop.Application"),
            "ActivateAction",
            &(
                "istmo-notification",
                vec![
                    target,
                    OwnedValue::from(zbus::zvariant::Str::from("see you")),
                ],
                platform,
            ),
        )
        .unwrap();
    let response = next_response(&s.responses);
    assert_eq!(response.kind(), Response::Reply("see you"));
    assert_eq!(response.user_data.as_deref(), Some("thread=7"));
    assert!(!response.launched_app);
    assert_eq!(take_activation_token().as_deref(), Some("tok-2"));

    // A non-exported activation (signal): the default action.
    let default_target = dict["default-action-target"].try_clone().unwrap();
    let iface = portal
        .object_server()
        .interface::<_, FakePortal>(PORTAL_PATH)
        .unwrap();
    zbus::block_on(FakePortal::action_invoked(
        iface.signal_emitter(),
        "chat",
        "istmo-notification",
        vec![default_target],
    ))
    .unwrap();
    assert_eq!(next_response(&s.responses).kind(), Response::Open);

    pollster::block_on(s.client.cancel(id("chat"))).unwrap();
    assert_eq!(
        calls.recv_timeout(TIMEOUT).unwrap(),
        ("chat".to_owned(), None)
    );
}
