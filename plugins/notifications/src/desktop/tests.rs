//! The desktop host end to end: client calls through a runtime, a
//! recording backend, a manual timer and a fake clock.

use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use istmo_core::Runtime;
use istmo_plugins::{DEEPLINKS_CHANNEL, DEFAULT_DEEPLINKS_CAPACITY, DeepLink};

use super::*;
use crate::coalesce::Rate;
use crate::{
    Action, ChannelSpec, Conversation, Interruption, NotificationImage, NotificationResponses,
    NotificationsClient, NotificationsHost, Participant, Response,
};

#[derive(Debug, Clone, PartialEq)]
struct ShowCall {
    id: String,
    mode: ShowMode,
    body: Option<String>,
    keys: Vec<String>,
    interruption: Option<Interruption>,
    icon: Option<PathBuf>,
}

#[derive(Debug)]
struct Recorder {
    info: Mutex<BackendInfo>,
    shows: Mutex<Vec<ShowCall>>,
    removes: Mutex<Vec<String>>,
    completed: Mutex<Vec<AckToken>>,
}

impl Recorder {
    fn new(rate: Rate) -> Arc<Self> {
        let mut info = BackendInfo::basic("recorder");
        info.rate = rate;
        Arc::new(Self {
            info: Mutex::new(info),
            shows: Mutex::default(),
            removes: Mutex::default(),
            completed: Mutex::default(),
        })
    }

    fn shows(&self) -> Vec<ShowCall> {
        std::mem::take(&mut *lock(&self.shows))
    }

    fn removes(&self) -> Vec<String> {
        std::mem::take(&mut *lock(&self.removes))
    }
}

impl Backend for Recorder {
    fn info(&self) -> BackendInfo {
        lock(&self.info).clone()
    }

    fn show(&self, shown: &Shown<'_>) -> Result<(), NotificationError> {
        lock(&self.shows).push(ShowCall {
            id: shown.notification.id.value.clone(),
            mode: shown.mode,
            body: shown.rendered.body.clone(),
            keys: shown
                .rendered
                .actions
                .iter()
                .map(|a| a.key.clone())
                .collect(),
            interruption: shown.notification.interruption,
            icon: shown.images.get(&ImageSlot::Icon).map(Path::to_path_buf),
        });
        Ok(())
    }

    fn remove(&self, id: &NotificationId) -> Result<(), NotificationError> {
        lock(&self.removes).push(id.value.clone());
        Ok(())
    }

    fn complete(&self, ack: AckToken) {
        lock(&self.completed).push(ack);
    }
}

/// A clock per test: tests run in parallel and the host calls the clock
/// from worker threads.
macro_rules! clock {
    ($start:expr) => {{
        static NOW: AtomicI64 = AtomicI64::new($start);
        fn now() -> i64 {
            NOW.load(Ordering::SeqCst)
        }
        (&NOW, now as fn() -> i64)
    }};
}

struct Setup {
    host: DesktopNotifications,
    backend: Arc<Recorder>,
    client: NotificationsClient,
    rt: Arc<Runtime>,
    dir: PathBuf,
}

impl Drop for Setup {
    fn drop(&mut self) {
        let _cleanup = std::fs::remove_dir_all(&self.dir);
    }
}

fn temp(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("istmo-notif-host-{name}-{}", std::process::id()))
}

fn setup_with(name: &str, rate: Rate, clock: fn() -> i64, store: bool) -> Setup {
    let dir = temp(name);
    let backend = Recorder::new(rate);
    let mut builder = DesktopNotifications::with_backend(backend.clone())
        .images_in(dir.join("images"))
        .with_clock(clock)
        .manual_timer()
        .with_channels(vec![ChannelSpec {
            id: ChannelId::try_from("chat").unwrap(),
            name: "Chat".to_owned(),
            description: None,
            importance: Interruption::High,
            sound: None,
            vibration: None,
            light_color: None,
            show_badge: true,
            lock_screen: None,
            bypass_dnd: false,
            group: None,
        }]);
    builder = if store {
        builder.store_at(dir.join("schedule.bin"))
    } else {
        builder.without_store()
    };
    let host = builder.build();
    let rt = Runtime::mock()
        .host(NotificationsHost::new(host.clone()))
        .runtime;
    host.attach(&rt);
    let client = NotificationsClient::from_runtime(&rt).unwrap();
    Setup {
        host,
        backend,
        client,
        rt,
        dir,
    }
}

fn n(id: &str) -> Notification {
    Notification::new(id, "Title").unwrap()
}

#[test]
fn show_renders_merges_the_channel_and_lists_it() {
    let (_now, clock) = clock!(1_000);
    let s = setup_with("show", Rate::LINUX, clock, false);
    let events = s.host.events();
    let notification = n("dl")
        .with_body("report.pdf")
        .with_channel("chat")
        .unwrap()
        .with_progress(Progress::fraction(0.5))
        .with_icon(NotificationImage::png(vec![1, 2, 3]))
        .with_action(Action::reply("reply", "Reply").unwrap());
    pollster::block_on(s.client.show_checked(notification)).unwrap();
    let shows = s.backend.shows();
    assert_eq!(shows.len(), 1);
    let show = &shows[0];
    assert_eq!(show.mode, ShowMode::New);
    assert_eq!(show.body.as_deref(), Some("report.pdf\n50%"));
    assert_eq!(show.keys, ["reply"]);
    assert_eq!(show.interruption, Some(Interruption::High));
    let icon = show.icon.clone().unwrap();
    assert_eq!(std::fs::read(&icon).unwrap(), [1, 2, 3]);
    assert_eq!(
        events.try_recv().unwrap(),
        NotificationEvent::Delivered(NotificationId::try_from("dl").unwrap())
    );
    let delivered = pollster::block_on(s.client.delivered()).unwrap();
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].delivered_ms, Some(1_000));
    // Unknown channels are refused.
    let err = pollster::block_on(s.client.show_checked(n("x").with_channel("nope").unwrap()))
        .unwrap_err();
    assert_eq!(err, NotificationError::UnknownChannel("nope".to_owned()));
    // Cancel takes it off the screen and removes its image file.
    pollster::block_on(s.client.cancel(NotificationId::try_from("dl").unwrap())).unwrap();
    assert_eq!(s.backend.removes(), ["dl"]);
    assert!(!icon.exists());
    assert!(pollster::block_on(s.client.delivered()).unwrap().is_empty());
}

#[test]
fn live_updates_are_coalesced_to_the_backend_rate() {
    let (now, clock) = clock!(0);
    let s = setup_with("coalesce", Rate::new(1, 1), clock, false);
    let id = NotificationId::try_from("dl").unwrap();
    pollster::block_on(s.client.show_checked(n("dl"))).unwrap();
    for value in [0.1, 0.2, 0.3] {
        pollster::block_on(
            s.client
                .set_progress(id.clone(), Some(Progress::fraction(value))),
        )
        .unwrap();
    }
    // The show spent the only token: every update waits, the latest wins.
    assert_eq!(s.backend.shows().len(), 1);
    assert_eq!(s.host.armed(), Some(1_000));
    now.store(1_000, Ordering::SeqCst);
    assert_eq!(s.host.tick(), None);
    let shows = s.backend.shows();
    assert_eq!(shows.len(), 1);
    assert_eq!(shows[0].mode, ShowMode::Replace { alert: false });
    assert_eq!(shows[0].body.as_deref(), Some("30%"));
    // An alerting update goes at once even without a token.
    let patch = NotificationPatch {
        body: Some("done".to_owned()),
        ..NotificationPatch::default()
    }
    .alerting();
    pollster::block_on(s.client.update(id, patch)).unwrap();
    let shows = s.backend.shows();
    assert_eq!(shows[0].mode, ShowMode::Replace { alert: true });
    assert_eq!(shows[0].body.as_deref(), Some("done\n30%"));
}

#[test]
fn dismissed_notifications_drop_silent_updates() {
    let (_now, clock) = clock!(0);
    let s = setup_with("dismiss", Rate::LINUX, clock, false);
    let id = NotificationId::try_from("dl").unwrap();
    let events = s.host.events();
    pollster::block_on(s.client.show_checked(n("dl"))).unwrap();
    s.backend.shows();
    s.host.sink().dismissed(&id, DismissReason::User);
    pollster::block_on(
        s.client
            .set_progress(id.clone(), Some(Progress::fraction(0.5))),
    )
    .unwrap();
    assert!(s.backend.shows().is_empty());
    let patch = NotificationPatch::default().alerting();
    pollster::block_on(s.client.update(id.clone(), patch)).unwrap();
    let shows = s.backend.shows();
    assert_eq!(shows[0].mode, ShowMode::New);
    assert_eq!(shows[0].body.as_deref(), Some("50%"));
    let mut got = events.try_iter();
    assert!(got.any(|event| event
        == NotificationEvent::Dismissed(Dismissal {
            id: id.clone(),
            reason: DismissReason::User,
        })));
}

#[test]
fn activations_publish_responses_deep_links_and_acks() {
    let (_now, clock) = clock!(0);
    let s = setup_with("respond", Rate::LINUX, clock, false);
    let responses = NotificationResponses::from_runtime(&s.rt).unwrap().stream();
    let links =
        s.rt.early_events()
            .queue(DEEPLINKS_CHANNEL, DEFAULT_DEEPLINKS_CAPACITY)
            .subscribe();
    let chat = n("chat:1")
        .with_user_data("42")
        .with_url("app://chat/1")
        .with_action(Action::button("read", "Mark read").unwrap().background())
        .with_action(Action::select("rsvp", "RSVP", ["Yes", "No"]).unwrap());
    pollster::block_on(s.client.show_checked(chat)).unwrap();
    let id = NotificationId::try_from("chat:1").unwrap();
    s.host.sink().activated(Activation {
        id: id.clone(),
        key: Some("read".to_owned()),
        reply: None,
        launched_app: false,
        carried: None,
    });
    let response = responses.try_recv().unwrap().unwrap();
    assert_eq!(response.kind(), Response::Action("read"));
    assert_eq!(response.user_data.as_deref(), Some("42"));
    let ack = response.ack.unwrap();
    let (link, _) = istmo_core::codec::decode::<DeepLink>(&links.try_recv().unwrap()).unwrap();
    assert_eq!(link.uri, "app://chat/1");
    assert_eq!(link.source.as_deref(), Some("notification:chat:1:read"));
    pollster::block_on(s.client.complete(ack)).unwrap();
    assert_eq!(*lock(&s.backend.completed), [ack]);
    assert!(pollster::block_on(s.client.complete(ack)).is_err());
    // A simulated choice answers with the choice as the reply.
    s.host.sink().activated(Activation {
        id,
        key: Some("rsvp#0".to_owned()),
        reply: None,
        launched_app: false,
        carried: None,
    });
    let response = responses.try_recv().unwrap().unwrap();
    assert_eq!(response.kind(), Response::Reply("Yes"));
    assert_eq!(response.ack, None);
    // Shown by an earlier process: the carried fields.
    s.host.sink().activated(Activation {
        id: NotificationId::try_from("old").unwrap(),
        key: None,
        reply: None,
        launched_app: true,
        carried: Some(Carried {
            user_data: Some("u".to_owned()),
            url: None,
        }),
    });
    let response = responses.try_recv().unwrap().unwrap();
    assert_eq!(response.kind(), Response::Open);
    assert!(response.launched_app);
    assert_eq!(response.user_data.as_deref(), Some("u"));
}

#[test]
fn snooze_hides_and_shows_again() {
    let (now, clock) = clock!(0);
    let s = setup_with("snooze", Rate::LINUX, clock, false);
    let events = s.host.events();
    let reminder = n("r").with_action(
        Action::snooze("later", "Later", std::time::Duration::from_secs(600)).unwrap(),
    );
    pollster::block_on(s.client.show_checked(reminder)).unwrap();
    s.backend.shows();
    let id = NotificationId::try_from("r").unwrap();
    s.host.sink().activated(Activation {
        id: id.clone(),
        key: Some("later".to_owned()),
        reply: None,
        launched_app: false,
        carried: None,
    });
    assert_eq!(s.backend.removes(), ["r"]);
    let pending = pollster::block_on(s.client.pending()).unwrap();
    assert_eq!(pending[0].next_fire_ms, Some(600_000));
    assert_eq!(s.host.armed(), Some(600_000));
    now.store(600_000, Ordering::SeqCst);
    s.host.tick();
    assert_eq!(s.backend.shows()[0].mode, ShowMode::New);
    let got: Vec<_> = events.try_iter().collect();
    assert!(got.contains(&NotificationEvent::Snoozed(id.clone())));
    assert_eq!(got.last(), Some(&NotificationEvent::Delivered(id)));
}

#[test]
fn scheduled_entries_survive_a_restart_and_missed_ones_come_late() {
    let (now, clock) = clock!(0);
    let dir;
    {
        let s = setup_with("store", Rate::LINUX, clock, true);
        let info = pollster::block_on(s.client.schedule_checked(n("soon"), Trigger::After(60_000)))
            .unwrap();
        assert_eq!(info.next_fire_ms, Some(60_000));
        pollster::block_on(s.client.schedule_checked(
            n("gone").with_missed(crate::MissedPolicy::Drop),
            Trigger::After(60_000),
        ))
        .unwrap();
        assert_eq!(pollster::block_on(s.client.pending()).unwrap().len(), 2);
        dir = s.dir.clone();
        // Keep the files for the second host.
        std::mem::forget(s);
    }
    // Ten minutes later, a new process.
    now.store(660_000, Ordering::SeqCst);
    let backend = Recorder::new(Rate::LINUX);
    let host = DesktopNotifications::with_backend(backend.clone())
        .images_in(dir.join("images"))
        .store_at(dir.join("schedule.bin"))
        .with_clock(clock)
        .manual_timer()
        .build();
    let events = host.events();
    let rt = Runtime::mock()
        .host(NotificationsHost::new(host.clone()))
        .runtime;
    host.attach(&rt);
    let shows = backend.shows();
    assert_eq!(shows.len(), 1);
    assert_eq!(shows[0].id, "soon");
    assert_eq!(
        events.try_recv().unwrap(),
        NotificationEvent::DeliveredLate(NotificationId::try_from("soon").unwrap())
    );
    // Both entries are done: the store file is gone.
    assert!(!dir.join("schedule.bin").exists());
    let _cleanup = std::fs::remove_dir_all(dir);
}

#[test]
fn timeouts_run_in_the_host_when_the_backend_has_none() {
    let (now, clock) = clock!(0);
    let s = setup_with("timeout", Rate::LINUX, clock, false);
    let events = s.host.events();
    let toast = n("t").with_timeout(std::time::Duration::from_secs(5));
    pollster::block_on(s.client.show_checked(toast)).unwrap();
    assert_eq!(s.host.armed(), Some(5_000));
    now.store(5_000, Ordering::SeqCst);
    s.host.tick();
    assert_eq!(s.backend.removes(), ["t"]);
    let got: Vec<_> = events.try_iter().collect();
    assert_eq!(
        got.last(),
        Some(&NotificationEvent::Dismissed(Dismissal {
            id: NotificationId::try_from("t").unwrap(),
            reason: DismissReason::Timeout,
        }))
    );
}

#[test]
fn messages_append_and_alert() {
    let (_now, clock) = clock!(0);
    let s = setup_with("chat", Rate::LINUX, clock, false);
    let ana = Participant::new("ana", "Ana");
    let chat = n("c").with_style(Style::Messaging(
        Conversation::new(Participant::new("me", "Me"))
            .with_message(ChatMessage::received(ana.clone(), "hi")),
    ));
    pollster::block_on(s.client.show_checked(chat)).unwrap();
    s.backend.shows();
    pollster::block_on(
        s.client
            .append_message_checked("c", ChatMessage::received(ana, "there")),
    )
    .unwrap();
    let shows = s.backend.shows();
    assert_eq!(shows[0].mode, ShowMode::Replace { alert: true });
    assert_eq!(shows[0].body.as_deref(), Some("Ana: hi\nAna: there"));
    let err = pollster::block_on(s.client.show_checked(n("p")))
        .and_then(|()| {
            pollster::block_on(s.client.append_message_checked("p", ChatMessage::sent("x")))
        })
        .unwrap_err();
    assert!(matches!(err, NotificationError::Invalid(_)));
}

#[test]
fn capabilities_add_the_host_simulations() {
    let (_now, clock) = clock!(0);
    let s = setup_with("caps", Rate::LINUX, clock, false);
    let caps = pollster::block_on(s.client.capabilities()).unwrap();
    assert_eq!(caps.backend, "recorder");
    assert_eq!(caps.support(Feature::Show), Support::Native);
    assert_eq!(caps.support(Feature::Schedule), Support::WhileRunning);
    assert_eq!(caps.support(Feature::InlineReply), Support::Simulated);
    assert_eq!(caps.support(Feature::ListDelivered), Support::WhileRunning);
    assert_eq!(caps.support(Feature::Picture), Support::Unsupported);
}

#[test]
fn without_a_platform_backend_everything_is_unsupported() {
    let host = DesktopNotifications::builder(Box::new(platform_backend))
        .without_store()
        .manual_timer()
        .build();
    let rt = Runtime::mock().host(NotificationsHost::new(host)).runtime;
    let client = NotificationsClient::from_runtime(&rt).unwrap();
    let caps = pollster::block_on(client.capabilities()).unwrap();
    assert_eq!(caps.backend, "unavailable");
    assert_eq!(
        pollster::block_on(client.permission()).unwrap().state,
        PermissionState::Unavailable
    );
    assert_eq!(
        pollster::block_on(client.show_checked(n("a"))),
        Err(NotificationError::Unsupported(Feature::Show))
    );
}

#[test]
fn live_notifications_stream_progress_through_one_sender() {
    let (_now, clock) = clock!(0);
    let s = setup_with("live", Rate::new(100, 100), clock, false);
    let live = pollster::block_on(
        s.client
            .show_live(n("dl").with_progress(Progress::fraction(0.0).with_label("Report"))),
    )
    .unwrap();
    for step in 1..=10u8 {
        live.progress(f32::from(step) / 10.0);
    }
    pollster::block_on(live.finish(n("dl").with_body("Done"))).unwrap();
    let shows = s.backend.shows();
    let last = shows.last().unwrap();
    assert_eq!(last.body.as_deref(), Some("Done"));
    // Progress calls in between collapsed: far fewer posts than calls.
    assert!(shows.len() <= 12, "{shows:?}");
    // A dropped handle cancels.
    let live = pollster::block_on(s.client.show_live(n("x"))).unwrap();
    drop(live);
    for _ in 0..200 {
        if lock(&s.backend.removes).contains(&"x".to_owned()) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("dropping a live notification did not cancel it");
}
