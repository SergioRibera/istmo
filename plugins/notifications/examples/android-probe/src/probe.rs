//! The probe commands. Each returns a plain-text report; failures are
//! reported, never panicked on.

use std::fmt::{Debug, Write as _};
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime};

use istmo_core::StreamItem;
use istmo_notifications::{
    AckToken, Action, CallInfo, CallKind, ChatMessage, Conversation, Group, GroupId, Notification,
    NotificationId, NotificationImage, NotificationPatch, NotificationResponses,
    NotificationsClient, Participant, PermissionRequest, Progress, ProgressPoint, ProgressSegment,
    Style, Trigger,
};

type Outcome = Result<String, String>;

fn block<T, E: Debug>(future: impl Future<Output = Result<T, E>>) -> Result<T, String> {
    pollster::block_on(future).map_err(|e| format!("{e:?}"))
}

fn client() -> Result<&'static NotificationsClient, String> {
    static CLIENT: OnceLock<NotificationsClient> = OnceLock::new();
    if let Some(client) = CLIENT.get() {
        return Ok(client);
    }
    let client = NotificationsClient::acquire().map_err(|e| format!("acquire: {e:?}"))?;
    Ok(CLIENT.get_or_init(|| client))
}

/// A 64 px opaque PNG generated in place (one colour), so image paths
/// run without assets.
fn png(rgb: [u8; 3]) -> Vec<u8> {
    const SIDE: u32 = 64;
    fn crc(data: &[u8]) -> u32 {
        let mut c = 0xffff_ffff_u32;
        for &b in data {
            c ^= u32::from(b);
            for _ in 0..8 {
                c = if c & 1 == 1 {
                    0xedb8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
        }
        !c
    }
    fn adler(data: &[u8]) -> u32 {
        let (mut a, mut b) = (1_u32, 0_u32);
        for &d in data {
            a = (a + u32::from(d)) % 65_521;
            b = (b + a) % 65_521;
        }
        (b << 16) | a
    }
    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&u32::try_from(data.len()).unwrap_or(0).to_be_bytes());
        let mut body = kind.to_vec();
        body.extend_from_slice(data);
        out.extend_from_slice(&body);
        out.extend_from_slice(&crc(&body).to_be_bytes());
    }
    let mut raw = Vec::new();
    for _ in 0..SIDE {
        raw.push(0);
        for _ in 0..SIDE {
            raw.extend_from_slice(&rgb);
        }
    }
    // Stored (uncompressed) deflate blocks.
    let mut z = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = raw.chunks(65_535).collect();
    for (i, block) in blocks.iter().enumerate() {
        z.push(u8::from(i + 1 == blocks.len()));
        let len = u16::try_from(block.len()).unwrap_or(0);
        z.extend_from_slice(&len.to_le_bytes());
        z.extend_from_slice(&(!len).to_le_bytes());
        z.extend_from_slice(block);
    }
    z.extend_from_slice(&adler(&raw).to_be_bytes());
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut header = Vec::new();
    header.extend_from_slice(&SIDE.to_be_bytes());
    header.extend_from_slice(&SIDE.to_be_bytes());
    header.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &header);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

fn n(id: &str, title: &str) -> Result<Notification, String> {
    Notification::new(id, title).map_err(|e| format!("{e:?}"))
}

fn e<T, E: Debug>(r: Result<T, E>) -> Result<T, String> {
    r.map_err(|e| format!("{e:?}"))
}

pub(crate) fn run(command: &str) -> String {
    let started = Instant::now();
    let mut words = command.split_whitespace();
    let name = words.next().unwrap_or_default();
    let arg: Option<u64> = words.next().and_then(|w| w.parse().ok());
    let outcome = match name {
        "listen" => listen(),
        "caps" => caps(),
        "perm" => client()
            .and_then(|c| block(c.permission()))
            .map(|p| format!("{p:#?}")),
        "ask" => client()
            .and_then(|c| block(c.request_permission(PermissionRequest::default())))
            .map(|p| format!("{p:#?}")),
        "show" => show(),
        "styles" => styles(),
        "chat" => chat(),
        "actions" => actions(),
        "call" => call(),
        "progress" => progress(arg.unwrap_or(40)),
        "segments" => segments(),
        "group" => group(),
        "group-shrink" => group_shrink(),
        "limit" => limit(),
        "invalid" => invalid(),
        "schedule" => schedule(arg.unwrap_or(15)),
        "pending" => client().and_then(|c| block(c.pending())).map(|p| {
            p.iter()
                .map(|p| {
                    format!(
                        "{} {:?} next {:?}",
                        p.notification.id.as_str(),
                        p.trigger,
                        p.next_fire_ms
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        }),
        "delivered" => client()
            .and_then(|c| block(c.delivered()))
            .map(|d| format!("{d:#?}")),
        "events" => events(arg.unwrap_or(10)),
        "cancel-all" => client()
            .and_then(|c| block(c.cancel_all()))
            .map(|()| "cancelled".to_owned()),
        other => Err(format!("unknown command {other:?}")),
    };
    let ms = started.elapsed().as_millis();
    match outcome {
        Ok(report) => format!("{report}\n({ms} ms)"),
        Err(err) => format!("FAILED: {err}\n({ms} ms)"),
    }
}

/// Logs every response (and completes background acks) on a thread.
fn listen() -> Outcome {
    static STARTED: OnceLock<()> = OnceLock::new();
    if STARTED.set(()).is_err() {
        return Ok("already listening".to_owned());
    }
    let responses = e(NotificationResponses::acquire())?.stream();
    std::thread::Builder::new()
        .name("probe-responses".to_owned())
        .spawn(move || {
            while let Ok(r) = responses.recv() {
                log::info!(
                    "response id={} action={:?} reply={:?} input_requested={} url={:?} user_data={:?} launched_app={} overflow={} ack={:?} kind={:?}",
                    r.id.as_str(),
                    r.action,
                    r.reply,
                    r.input_requested,
                    r.url,
                    r.user_data,
                    r.launched_app,
                    r.overflow_actions.len(),
                    r.ack,
                    r.kind(),
                );
                if let (Some(ack), Ok(c)) = (r.ack.clone(), client()) {
                    std::thread::sleep(Duration::from_millis(500));
                    let done = block(c.complete(ack));
                    log::info!("complete: {done:?}");
                }
            }
        })
        .map_err(|e| e.to_string())?;
    Ok("listening for responses (logcat notif-probe)".to_owned())
}

fn caps() -> Outcome {
    let caps = block(client()?.capabilities())?;
    let mut out = format!(
        "backend {} max_actions {} max_active {:?}\n",
        caps.backend, caps.max_actions, caps.max_active
    );
    for f in &caps.features {
        let _ = writeln!(out, "{:?}: {:?}", f.feature, f.support);
    }
    Ok(out)
}

fn show() -> Outcome {
    let c = client()?;
    let note = n("probe.show", "Hello from istmo")?
        .with_body("Tap me: an Open response, then the deep link")
        .with_url("probe://show")
        .with_user_data("user-data-1")
        .with_icon(NotificationImage::Png(png([224, 102, 43])));
    block(c.show_checked(note))?;
    Ok("shown probe.show".to_owned())
}

fn styles() -> Outcome {
    let c = client()?;
    block(
        c.show_checked(
            n("probe.big", "Big text")?
                .with_body("collapsed")
                .with_style(Style::BigText(
                    "A long body that only shows when the notification is expanded. ".repeat(6),
                )),
        ),
    )?;
    block(
        c.show_checked(
            n("probe.inbox", "Inbox")?
                .with_style(Style::Inbox((1..=5).map(|i| format!("line {i}")).collect())),
        ),
    )?;
    block(
        c.show_checked(
            n("probe.picture", "Picture")?
                .with_body("BigPicture from PNG bytes")
                .with_style(Style::Picture(NotificationImage::Png(png([30, 120, 200])))),
        ),
    )?;
    Ok("shown probe.big, probe.inbox, probe.picture".to_owned())
}

fn chat() -> Outcome {
    let c = client()?;
    let me = Participant::new("me", "Me");
    let ana =
        Participant::new("ana", "Ana").with_avatar(NotificationImage::Png(png([200, 60, 120])));
    let conversation = Conversation::new(me)
        .with_message(ChatMessage::received(ana.clone(), "Are you coming?"))
        .with_message(ChatMessage::sent("In 5"));
    let note = n("probe.chat", "Ana")?
        .with_style(Style::Messaging(conversation))
        .with_action(e(Action::reply("reply", "Reply"))?.background());
    block(c.show_checked(note))?;
    std::thread::sleep(Duration::from_secs(2));
    block(c.append_message_checked(
        "probe.chat",
        ChatMessage::received(ana, "Waiting at the door"),
    ))?;
    Ok("shown probe.chat, appended one message after 2 s; reply from the shade".to_owned())
}

fn actions() -> Outcome {
    let c = client()?;
    let note = n("probe.actions", "Five actions")?
        .with_body("Two buttons, then More with the rest")
        .with_action(e(Action::button("open", "Open"))?.with_url("probe://open"))
        .with_action(e(Action::button("bg", "Background"))?.background())
        .with_action(e(Action::select("pick", "Pick", ["red", "green", "blue"]))?)
        .with_action(e(Action::snooze(
            "later",
            "Snooze 20 s",
            Duration::from_secs(20),
        ))?)
        .with_action(e(Action::reply("reply", "Reply"))?);
    block(c.show_checked(note))?;
    let few = n("probe.actions3", "Reply, choices, snooze")?
        .with_action(e(Action::reply("reply", "Reply"))?.background())
        .with_action(e(Action::select("pick", "Pick", ["yes", "no"]))?.background())
        .with_action(e(Action::snooze(
            "later",
            "Snooze 20 s",
            Duration::from_secs(20),
        ))?);
    block(c.show_checked(few))?;
    Ok("shown probe.actions (More) and probe.actions3".to_owned())
}

fn call() -> Outcome {
    let c = client()?;
    let mut note = n("probe.call", "Incoming call")?.with_style(Style::Call(CallInfo {
        caller: Participant::new("bob", "Bob"),
        kind: CallKind::Incoming,
        video: false,
    }));
    note.ongoing = true;
    block(c.show_checked(note))?;
    Ok("shown probe.call (no full-screen: simulated call buttons)".to_owned())
}

fn progress(steps: u64) -> Outcome {
    let c = client()?;
    let mut note = n("probe.progress", "Downloading")?
        .with_body("report.pdf")
        .with_progress(Progress::fraction(0.0))
        .with_status_text("0%");
    note.ongoing = true;
    note.live = true;
    let live = block(c.show_live(note))?;
    let started = Instant::now();
    for i in 1..=steps {
        #[allow(clippy::cast_precision_loss)]
        live.progress(i as f32 / steps as f32);
        std::thread::sleep(Duration::from_millis(100));
    }
    let spent = started.elapsed();
    block(
        live.finish(
            n("probe.progress", "Downloaded")?
                .with_body("report.pdf, tap to open")
                .with_url("probe://file"),
        ),
    )?;
    Ok(format!(
        "{steps} progress steps in {spent:?} (10 per second, coalesced to 4), then finish"
    ))
}

fn segments() -> Outcome {
    let c = client()?;
    let mut note = n("probe.segments", "Ride to the airport")?
        .with_body("Driver on the way")
        .with_status_text("5 min")
        .with_progress(Progress {
            value: Some(0.35),
            label: Some("5 min".to_owned()),
            segments: vec![
                ProgressSegment {
                    length: 40,
                    color: Some(0xff_e0_66_2b),
                },
                ProgressSegment {
                    length: 60,
                    color: Some(0xff_2b_7b_e0),
                },
            ],
            points: vec![ProgressPoint {
                position: 40,
                color: Some(0xff_ff_ff_ff),
            }],
        });
    note.ongoing = true;
    note.live = true;
    block(c.show_checked(note))?;
    std::thread::sleep(Duration::from_secs(3));
    block(c.update_checked(
        "probe.segments",
        NotificationPatch::progress(Some(Progress {
            value: Some(0.7),
            label: None,
            segments: vec![
                ProgressSegment {
                    length: 40,
                    color: Some(0xff_e0_66_2b),
                },
                ProgressSegment {
                    length: 60,
                    color: Some(0xff_2b_7b_e0),
                },
            ],
            points: vec![ProgressPoint {
                position: 40,
                color: Some(0xff_ff_ff_ff),
            }],
        })),
    ))?;
    Ok("shown probe.segments (ProgressStyle on 36), updated to 70% after 3 s".to_owned())
}

fn group() -> Outcome {
    let c = client()?;
    for i in 1..=3 {
        let mut note =
            n(&format!("probe.group.{i}"), &format!("Message {i}"))?.with_body("in group probe");
        note.group = Some(Group {
            id: e(GroupId::try_from("probe"))?,
            title: Some("Probe group".to_owned()),
            summary: Some("3 messages".to_owned()),
        });
        block(c.show_checked(note))?;
    }
    Ok("shown 3 grouped notifications (summary posted by the plugin)".to_owned())
}

/// Two grouped notifications, then one cancelled: the other must stay
/// (cancelling the summary would take it along).
fn group_shrink() -> Outcome {
    let c = client()?;
    let group = Group {
        id: e(GroupId::try_from("probe.shrink"))?,
        title: Some("Shrinking group".to_owned()),
        summary: None,
    };
    for i in 1..=2 {
        let mut note = n(&format!("probe.shrink.{i}"), &format!("Member {i}"))?;
        note.group = Some(group.clone());
        block(c.show_checked(note))?;
    }
    block(c.cancel(e(NotificationId::try_from("probe.shrink.1"))?))?;
    std::thread::sleep(Duration::from_millis(500));
    let left: Vec<_> = block(c.delivered())?
        .into_iter()
        .filter(|d| d.id.as_str().starts_with("probe.shrink."))
        .map(|d| d.id.value)
        .collect();
    if left == ["probe.shrink.2"] {
        Ok("probe.shrink.2 survived its sibling's cancel".to_owned())
    } else {
        Err(format!("left after the cancel: {left:?}"))
    }
}

/// Shows distinct ids until the backend refuses one (`TooMany(50)`),
/// then removes them.
fn limit() -> Outcome {
    let c = client()?;
    let mut shown = 0;
    let mut refusal = None;
    for i in 0..60 {
        match block(c.show_checked(n(&format!("probe.limit.{i}"), &format!("Limit {i}"))?)) {
            Ok(()) => shown += 1,
            Err(err) => {
                refusal = Some(err);
                break;
            }
        }
    }
    for i in 0..shown {
        let _ = block(c.cancel(e(NotificationId::try_from(
            format!("probe.limit.{i}").as_str(),
        ))?));
    }
    Ok(format!("shown {shown}, then {refusal:?}"))
}

/// Unchecked calls: the Kotlin host answers what the desktop host does.
fn invalid() -> Outcome {
    let c = client()?;
    let mut blank = n("probe.invalid", "x")?;
    blank.title = " ".to_owned();
    let show = block(c.show(blank));
    let every = block(c.schedule(n("probe.invalid", "Every second")?, Trigger::Every(1_000)));
    let ack = block(c.complete(AckToken { value: 987_654_321 }));
    Ok(format!(
        "show: {show:?}\nschedule: {every:?}\ncomplete: {ack:?}"
    ))
}

fn schedule(seconds: u64) -> Outcome {
    let c = client()?;
    let at = block(c.schedule_checked(
        n("probe.later", "Scheduled")?.with_body(format!("{seconds} s after scheduling")),
        Trigger::after(Duration::from_secs(seconds)),
    ))?;
    let every = block(c.schedule_checked(
        n("probe.every", "Every minute")?.with_body("repeats until cancel-all"),
        Trigger::every(Duration::from_secs(60)),
    ))?;
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    Ok(format!(
        "probe.later {at:?}\nprobe.every {every:?}\nnow {now}"
    ))
}

fn events(seconds: u64) -> Outcome {
    let stream = e(client()?.events())?;
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut out = String::new();
    while Instant::now() < deadline {
        match stream.try_recv() {
            Some(Ok(StreamItem::Event(event))) => {
                let _ = writeln!(out, "{event:?}");
            }
            Some(Ok(other)) => {
                let _ = writeln!(out, "stream {other:?}");
                break;
            }
            Some(Err(err)) => return Err(format!("{err:?}")),
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    }
    Ok(if out.is_empty() {
        format!("no events in {seconds} s")
    } else {
        out
    })
}
