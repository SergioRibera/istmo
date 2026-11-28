//! The desktop host end to end: client calls through a runtime, a
//! recording backend, a manual timer and a fake clock.

use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
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
    /// Thread of each remove.
    remove_threads: Mutex<Vec<Option<String>>>,
    completed: Mutex<Vec<AckToken>>,
    info_reads: AtomicUsize,
    remove_alls: AtomicUsize,
}

impl Recorder {
    fn new(rate: Rate) -> Arc<Self> {
        let mut info = BackendInfo::basic("recorder");
        info.rate = rate;
        Arc::new(Self {
            info: Mutex::new(info),
            shows: Mutex::default(),
            removes: Mutex::default(),
            remove_threads: Mutex::default(),
            completed: Mutex::default(),
            info_reads: AtomicUsize::new(0),
            remove_alls: AtomicUsize::new(0),
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
        self.info_reads.fetch_add(1, Ordering::SeqCst);
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
        lock(&self.remove_threads).push(std::thread::current().name().map(str::to_owned));
        Ok(())
    }

    fn remove_all(&self) -> Result<(), NotificationError> {
        self.remove_alls.fetch_add(1, Ordering::SeqCst);
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
    let backend = Recorder::new(rate);
    setup_on(name, backend.clone(), backend, clock, store)
}

/// A host over `shown` (which records into `backend`).
fn setup_on(
    name: &str,
    backend: Arc<Recorder>,
    shown: Arc<dyn Backend>,
    clock: fn() -> i64,
    store: bool,
) -> Setup {
    let dir = temp(name);
    let mut builder = DesktopNotifications::with_backend(shown)
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
    assert_eq!(
        pollster::block_on(s.client.delivered()).unwrap(),
        Vec::new()
    );
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
    s.host.settle();
    pollster::block_on(
        s.client
            .set_progress(id.clone(), Some(Progress::fraction(0.5))),
    )
    .unwrap();
    assert_eq!(s.backend.shows(), Vec::new());
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
    s.host.settle();
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
    s.host.settle();
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
    s.host.settle();
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
    s.host.settle();
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
    // A backend that fails to start (what targets without one get); the
    // real platform backend would talk to the session.
    let host = DesktopNotifications::builder(Box::new(|_sink| {
        Err(NotificationError::Unsupported(Feature::Show))
    }))
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
    // The start error comes back as is, on every target.
    let shown = pollster::block_on(client.show_checked(n("a")));
    assert_eq!(shown, Err(NotificationError::Unsupported(Feature::Show)));
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

#[test]
fn schedules_that_never_fire_are_refused() {
    let (_now, clock) = clock!(86_400_000);
    let s = setup_with("dead", Rate::LINUX, clock, false);
    let ended = crate::CalendarRule {
        until_ms: Some(1_000),
        ..crate::CalendarRule::daily(9, 0).in_zone("UTC")
    };
    let err = pollster::block_on(s.client.schedule_checked(n("x"), Trigger::Calendar(ended)))
        .unwrap_err();
    assert_eq!(
        err,
        NotificationError::Invalid(InvalidField::new("trigger.until_ms", Problem::OutOfRange))
    );
    assert_eq!(pollster::block_on(s.client.pending()).unwrap(), Vec::new());
}

/// A backend that holds the show of one id until released, and tells
/// when a show starts.
struct Gate {
    inner: Arc<Recorder>,
    held: String,
    release: flume::Receiver<()>,
    entered: flume::Sender<String>,
}

impl Gate {
    /// The gate, its release (drop it to open) and the started shows.
    fn new(
        inner: Arc<Recorder>,
        held: &str,
    ) -> (Arc<Self>, flume::Sender<()>, flume::Receiver<String>) {
        let (release_tx, release) = flume::bounded(0);
        let (entered, entered_rx) = flume::unbounded();
        let gate = Arc::new(Self {
            inner,
            held: held.to_owned(),
            release,
            entered,
        });
        (gate, release_tx, entered_rx)
    }
}

impl Backend for Gate {
    fn info(&self) -> BackendInfo {
        self.inner.info()
    }

    fn show(&self, shown: &Shown<'_>) -> Result<(), NotificationError> {
        let id = shown.notification.id.value.clone();
        let held = id == self.held;
        let _told = self.entered.send(id);
        if held {
            // Disconnected once released: later shows pass at once.
            let _released = self.release.recv();
        }
        self.inner.show(shown)
    }

    fn remove(&self, id: &NotificationId) -> Result<(), NotificationError> {
        self.inner.remove(id)
    }
}

fn client_of(s: &Setup) -> NotificationsClient {
    NotificationsClient::from_runtime(&s.rt).unwrap()
}

fn id(value: &str) -> NotificationId {
    NotificationId::try_from(value).unwrap()
}

#[test]
fn a_new_show_with_the_same_id_takes_its_new_images() {
    let (_now, clock) = clock!(0);
    let s = setup_with("reshow", Rate::LINUX, clock, false);
    let icon = |byte: u8| NotificationImage::png(vec![byte]);
    pollster::block_on(s.client.show_checked(n("a").with_icon(icon(1)))).unwrap();
    let first = s.backend.shows()[0].icon.clone().unwrap();
    pollster::block_on(s.client.show_checked(n("a").with_icon(icon(2)))).unwrap();
    let second = s.backend.shows()[0].icon.clone().unwrap();
    assert_ne!(first, second);
    assert_eq!(std::fs::read(&second).unwrap(), [2]);
    assert!(!first.exists(), "the replaced image is released");
    // A progress update reuses the shown files.
    pollster::block_on(
        s.client
            .set_progress(id("a"), Some(Progress::fraction(0.5))),
    )
    .unwrap();
    assert_eq!(s.backend.shows()[0].icon.as_ref(), Some(&second));
    assert!(second.exists());
}

#[test]
fn a_cancel_waits_for_the_post_in_flight_and_wins() {
    let (_now, clock) = clock!(0);
    let backend = Recorder::new(Rate::LINUX);
    let (gate, release, entered) = Gate::new(backend.clone(), "slow");
    let s = setup_on("lane-cancel", backend, gate, clock, false);
    let showing = client_of(&s);
    let show = std::thread::spawn(move || {
        let slow = n("slow").with_icon(NotificationImage::png(vec![7]));
        pollster::block_on(showing.show_checked(slow))
    });
    assert_eq!(entered.recv().unwrap(), "slow");
    let cancelling = client_of(&s);
    let cancel = std::thread::spawn(move || pollster::block_on(cancelling.cancel(id("slow"))));
    std::thread::sleep(std::time::Duration::from_millis(50));
    assert!(!cancel.is_finished(), "the cancel waits for the show");
    drop(release);
    show.join().unwrap().unwrap();
    cancel.join().unwrap().unwrap();
    let icon = s.backend.shows()[0].icon.clone().unwrap();
    assert_eq!(s.backend.removes(), ["slow"]);
    assert!(!icon.exists());
    assert_eq!(
        pollster::block_on(s.client.delivered()).unwrap(),
        Vec::new()
    );
}

#[test]
fn posts_for_one_id_land_in_the_order_they_were_made() {
    let (_now, clock) = clock!(0);
    let backend = Recorder::new(Rate::LINUX);
    let (gate, release, entered) = Gate::new(backend.clone(), "x");
    let s = setup_on("lane-order", backend, gate, clock, false);
    let first = client_of(&s);
    let one =
        std::thread::spawn(move || pollster::block_on(first.show_checked(n("x").with_body("one"))));
    assert_eq!(entered.recv().unwrap(), "x");
    let second = client_of(&s);
    let two = std::thread::spawn(move || {
        pollster::block_on(second.show_checked(n("x").with_body("two")))
    });
    std::thread::sleep(std::time::Duration::from_millis(50));
    drop(release);
    one.join().unwrap().unwrap();
    two.join().unwrap().unwrap();
    let bodies: Vec<_> = s
        .backend
        .shows()
        .into_iter()
        .map(|show| show.body)
        .collect();
    assert_eq!(bodies, [Some("one".to_owned()), Some("two".to_owned())]);
    let delivered = pollster::block_on(s.client.delivered()).unwrap();
    assert_eq!(delivered[0].body.as_deref(), Some("two"));
}

#[test]
fn a_scheduled_delivery_cancelled_after_it_was_taken_is_not_shown() {
    let (now, clock) = clock!(0);
    let backend = Recorder::new(Rate::LINUX);
    let (gate, release, entered) = Gate::new(backend.clone(), "a");
    let s = setup_on("lane-due", backend, gate, clock, false);
    for name in ["a", "b"] {
        pollster::block_on(s.client.schedule_checked(n(name), Trigger::After(1_000))).unwrap();
    }
    now.store(1_000, Ordering::SeqCst);
    let host = s.host.clone();
    // Both are taken from the engine; `a` is held on its way out.
    let tick = std::thread::spawn(move || host.tick());
    assert_eq!(entered.recv().unwrap(), "a");
    pollster::block_on(s.client.cancel(id("b"))).unwrap();
    drop(release);
    tick.join().unwrap();
    let shown: Vec<_> = s.backend.shows().into_iter().map(|show| show.id).collect();
    assert_eq!(shown, ["a"]);
}

#[test]
fn calls_wait_until_the_store_is_loaded() {
    let (_now, clock) = clock!(0);
    let dir = temp("load");
    let store = Store::new(dir.join("schedule.bin"));
    store
        .save(&[crate::schedule::Entry {
            notification: n("kept"),
            trigger: Trigger::After(600_000),
            anchor_ms: 0,
            next_fire_ms: Some(600_000),
        }])
        .unwrap();
    let backend = Recorder::new(Rate::LINUX);
    let (release, started) = flume::bounded::<()>(0);
    let host = DesktopNotifications::builder(Box::new(move |_sink| {
        // The backend starts slowly, inside the load.
        let _released = started.recv();
        Ok(Arc::clone(&backend) as Arc<dyn Backend>)
    }))
    .images_in(dir.join("images"))
    .store_at(dir.join("schedule.bin"))
    .with_clock(clock)
    .manual_timer()
    .build();
    let rt = Runtime::mock()
        .host(NotificationsHost::new(host.clone()))
        .runtime;
    let attaching = (host, Arc::clone(&rt));
    let attach = std::thread::spawn(move || attaching.0.attach(&attaching.1));
    std::thread::sleep(std::time::Duration::from_millis(30));
    let client = NotificationsClient::from_runtime(&rt).unwrap();
    let cancel = std::thread::spawn(move || pollster::block_on(client.cancel(id("kept"))));
    std::thread::sleep(std::time::Duration::from_millis(50));
    assert!(!cancel.is_finished(), "the cancel waits for the load");
    drop(release);
    attach.join().unwrap();
    cancel.join().unwrap().unwrap();
    let client = NotificationsClient::from_runtime(&rt).unwrap();
    assert_eq!(pollster::block_on(client.pending()).unwrap(), Vec::new());
    assert!(!dir.join("schedule.bin").exists());
    let _cleanup = std::fs::remove_dir_all(dir);
}

#[test]
fn sink_calls_while_the_backend_starts_do_not_reenter_the_host() {
    let (_now, clock) = clock!(0);
    let backend = Recorder::new(Rate::LINUX);
    let dir = temp("reenter");
    let host = DesktopNotifications::builder(Box::new(move |sink: BackendSink| {
        sink.changed();
        sink.dismissed(&id("x"), DismissReason::User);
        Ok(Arc::clone(&backend) as Arc<dyn Backend>)
    }))
    .images_in(dir.join("images"))
    .without_store()
    .with_clock(clock)
    .manual_timer()
    .build();
    let rt = Runtime::mock()
        .host(NotificationsHost::new(host.clone()))
        .runtime;
    host.attach(&rt);
    host.settle();
    let client = NotificationsClient::from_runtime(&rt).unwrap();
    pollster::block_on(client.show_checked(n("x"))).unwrap();
    let _cleanup = std::fs::remove_dir_all(dir);
}

#[test]
fn reports_are_handled_off_the_reporting_thread() {
    let (_now, clock) = clock!(0);
    let s = setup_with("worker", Rate::LINUX, clock, false);
    let reminder = n("r").with_action(
        Action::snooze("later", "Later", std::time::Duration::from_secs(600)).unwrap(),
    );
    pollster::block_on(s.client.show_checked(reminder)).unwrap();
    s.host.sink().activated(Activation {
        id: id("r"),
        key: Some("later".to_owned()),
        reply: None,
        launched_app: false,
        carried: None,
    });
    s.host.settle();
    // The snooze removed it from the report worker, not from here.
    assert_eq!(
        *lock(&s.backend.remove_threads),
        [Some("istmo-notifications-reports".to_owned())]
    );
}

#[test]
fn info_is_read_once_and_a_new_rate_applies_with_a_queue() {
    let (now, clock) = clock!(0);
    let s = setup_with("info", Rate::new(1, 1), clock, false);
    pollster::block_on(s.client.show_checked(n("dl"))).unwrap();
    for step in 1..=5u8 {
        let bar = Progress::fraction(f32::from(step) / 10.0);
        pollster::block_on(s.client.set_progress(id("dl"), Some(bar))).unwrap();
    }
    assert_eq!(s.backend.info_reads.load(Ordering::SeqCst), 1);
    s.backend.shows();
    assert_eq!(s.host.armed(), Some(1_000));
    // The backend speeds up: the queued update keeps its place and goes
    // at the new rate.
    lock(&s.backend.info).rate = Rate::new(100, 100);
    s.host.sink().changed();
    s.host.settle();
    assert_eq!(s.backend.info_reads.load(Ordering::SeqCst), 2);
    assert_eq!(s.host.armed(), Some(10));
    now.store(10, Ordering::SeqCst);
    s.host.tick();
    let shows = s.backend.shows();
    assert_eq!(shows.len(), 1);
    assert_eq!(shows[0].body.as_deref(), Some("50%"));
}

#[test]
fn patches_of_a_call_may_carry_call_actions() {
    let (_now, clock) = clock!(0);
    let s = setup_with("call", Rate::LINUX, clock, false);
    let call = n("call").with_style(Style::Call(crate::CallInfo {
        caller: Participant::new("ana", "Ana"),
        kind: crate::CallKind::Incoming,
        video: false,
    }));
    pollster::block_on(s.client.show_checked(call)).unwrap();
    let answer = Action {
        id: ActionId {
            value: ActionId::CALL_ANSWER.to_owned(),
        },
        ..Action::button("x", "Answer now").unwrap()
    };
    let patch = NotificationPatch {
        actions: Some(vec![answer.clone()]),
        ..NotificationPatch::default()
    };
    pollster::block_on(s.client.update(id("call"), patch)).unwrap();
    // On a notification that is not a call the host refuses it.
    pollster::block_on(s.client.show_checked(n("plain"))).unwrap();
    let patch = NotificationPatch {
        actions: Some(vec![answer]),
        ..NotificationPatch::default()
    };
    let err = NotificationError::from(
        pollster::block_on(s.client.update(id("plain"), patch)).unwrap_err(),
    );
    assert!(matches!(err, NotificationError::Invalid(_)), "{err:?}");
}

#[test]
fn acks_and_dismissals_are_bounded_and_tokens_unguessable() {
    let (_now, clock) = clock!(0);
    let s = setup_with("bounded", Rate::new(1_000, 1_000), clock, false);
    let chat = n("chat").with_action(Action::button("read", "Read").unwrap().background());
    pollster::block_on(s.client.show_checked(chat)).unwrap();
    let activate = || {
        s.host.sink().activated(Activation {
            id: id("chat"),
            key: Some("read".to_owned()),
            reply: None,
            launched_app: false,
            carried: None,
        });
    };
    activate();
    activate();
    s.host.settle();
    let (first, second) = {
        let state = s.host.inner.lock();
        (state.acks[0].0, state.acks[1].0)
    };
    assert_ne!(first, 1);
    assert_ne!(second, first.wrapping_add(1));
    for _ in 0..MAX_ACKS {
        activate();
    }
    s.host.settle();
    assert_eq!(s.host.inner.lock().acks.len(), MAX_ACKS);
    // The oldest was forgotten.
    assert!(pollster::block_on(s.client.complete(AckToken { value: first })).is_err());
    for i in 0..MAX_DISMISSED + 20 {
        let name = format!("d{i}");
        pollster::block_on(s.client.show_checked(n(&name))).unwrap();
        s.host.sink().dismissed(&id(&name), DismissReason::User);
    }
    s.host.settle();
    let state = s.host.inner.lock();
    assert_eq!(state.dismissed.len(), MAX_DISMISSED);
    assert_eq!(state.coalescer.dismissed_len(), MAX_DISMISSED);
}

#[test]
fn progress_updates_reuse_images_without_the_image_cache() {
    let (_now, clock) = clock!(0);
    let s = setup_with("reuse", Rate::new(1_000, 1_000), clock, false);
    let first = n("dl").with_icon(NotificationImage::png(vec![4, 5, 6]));
    pollster::block_on(s.client.show_checked(first)).unwrap();
    let icon = s.backend.shows()[0].icon.clone().unwrap();
    // A file write elsewhere holds the cache: a progress update and the
    // state lock must not wait for it.
    let cache = lock(&s.host.inner.images);
    let (tx, rx) = flume::bounded(1);
    let client = client_of(&s);
    std::thread::spawn(move || {
        let done = pollster::block_on(client.set_progress(id("dl"), Some(Progress::fraction(0.5))));
        let _sent = tx.send(done);
    });
    let done = rx.recv_timeout(std::time::Duration::from_secs(5));
    drop(cache);
    done.expect("the update waited for the image cache")
        .unwrap();
    let shows = s.backend.shows();
    assert_eq!(shows[0].icon.as_deref(), Some(icon.as_path()));
    // The shared use is still one: the cancel removes the file.
    pollster::block_on(s.client.cancel(id("dl"))).unwrap();
    assert!(!icon.exists());
}

#[test]
fn cancels_reach_the_backend_for_ids_of_an_earlier_process() {
    let (_now, clock) = clock!(0);
    let s = setup_with("earlier", Rate::LINUX, clock, false);
    // Shown by an earlier process: the host has no record of it.
    pollster::block_on(s.client.cancel(id("old"))).unwrap();
    assert_eq!(s.backend.removes(), ["old"]);
    // A dismissed one the host knows is gone is not removed again.
    pollster::block_on(s.client.show_checked(n("seen"))).unwrap();
    s.host.sink().dismissed(&id("seen"), DismissReason::User);
    s.host.settle();
    pollster::block_on(s.client.cancel(id("seen"))).unwrap();
    assert_eq!(s.backend.removes(), Vec::<String>::new());
    pollster::block_on(s.client.show_checked(n("a"))).unwrap();
    pollster::block_on(s.client.cancel_all()).unwrap();
    assert_eq!(s.backend.removes(), ["a"]);
    assert_eq!(s.backend.remove_alls.load(Ordering::SeqCst), 1);
}

/// A [`Recorder`] that also records what it is armed with.
#[derive(Debug)]
struct Armed {
    rec: Arc<Recorder>,
    arms: Mutex<Vec<Vec<(String, i64)>>>,
}

impl Backend for Armed {
    fn info(&self) -> BackendInfo {
        self.rec.info()
    }

    fn show(&self, shown: &Shown<'_>) -> Result<(), NotificationError> {
        self.rec.show(shown)
    }

    fn remove(&self, id: &NotificationId) -> Result<(), NotificationError> {
        self.rec.remove(id)
    }

    fn arm(&self, upcoming: &[Upcoming]) {
        lock(&self.arms).push(
            upcoming
                .iter()
                .map(|u| (u.notification.id.value.clone(), u.fire_ms))
                .collect(),
        );
    }
}

#[test]
fn the_arm_window_rolls_forward_while_the_app_runs() {
    const DAY: i64 = 86_400_000;
    let (now, clock) = clock!(1_000_000);
    let rec = Recorder::new(Rate::LINUX);
    let armed = Arc::new(Armed {
        rec: rec.clone(),
        arms: Mutex::default(),
    });
    let s = setup_on("roll", rec, armed.clone(), clock, true);
    let fire = 1_000_000 + 15 * DAY;
    pollster::block_on(s.client.schedule_checked(n("far"), Trigger::At(fire))).unwrap();
    assert_eq!(lock(&armed.arms).last(), Some(&Vec::new()));
    // The timer wakes when the entry enters the window.
    assert_eq!(s.host.armed(), Some(fire - ARM_WINDOW_MS));
    now.store(fire - ARM_WINDOW_MS, Ordering::SeqCst);
    assert_eq!(s.host.tick(), Some(fire));
    assert_eq!(
        lock(&armed.arms).last(),
        Some(&vec![("far".to_owned(), fire)])
    );
}
