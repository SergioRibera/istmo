use std::path::PathBuf;

use super::*;
use crate::render::{RenderLimits, render};
use crate::{
    Action, CallInfo, ChatMessage, Conversation, Notification, NotificationImage, Participant,
    Progress, ReplyOptions,
};

const BUILD: u32 = 22_631;

fn limits() -> RenderLimits {
    super::super::limits()
}

fn shown() -> Options {
    Options {
        build: BUILD,
        bind: true,
        fire_ms: None,
        carry: Carry::All,
    }
}

fn files(entries: &[(ImageSlot, &str)]) -> ImageFiles {
    ImageFiles {
        entries: entries
            .iter()
            .map(|(slot, path)| (slot.clone(), PathBuf::from(path)))
            .collect(),
    }
}

fn toast(n: &Notification, images: &ImageFiles, options: Options) -> Toast {
    build(n, &render(n, &limits()), images, options)
}

#[test]
fn plain_toast_binds_its_texts() {
    let n = Notification::new("n1", "Title & <more>")
        .unwrap()
        .with_subtitle("Sub")
        .with_body("Body");
    let t = toast(&n, &ImageFiles::default(), shown());
    assert_eq!(
        t.xml,
        "<toast launch=\"istmo1&amp;i=n1\"><visual><binding template=\"ToastGeneric\">\
         <text>{title}</text><text>{subtitle}</text><text>{body}</text>\
         </binding></visual></toast>"
    );
    assert_eq!(
        values_text(&t),
        "title=Title & <more>\nsubtitle=Sub\nbody=Body\n"
    );
}

#[test]
fn scheduled_toasts_write_values_in_place_and_mark_the_fire_time() {
    let n = Notification::new("n1", "A \"quoted\" title").unwrap();
    let options = Options {
        bind: false,
        fire_ms: Some(42),
        ..shown()
    };
    let t = toast(&n, &ImageFiles::default(), options);
    assert_eq!(
        t.xml,
        "<toast launch=\"istmo1&amp;i=n1&amp;s=42\"><visual><binding template=\"ToastGeneric\">\
         <text>A &quot;quoted&quot; title</text></binding></visual></toast>"
    );
    assert_eq!(t.values, Vec::new());
}

#[test]
fn progress_updates_keep_the_template() {
    let base = Notification::new("dl", "Downloading")
        .unwrap()
        .with_status_text("Downloading...")
        .with_progress(Progress::fraction(0.25).with_label("report.pdf"));
    let later = base
        .clone()
        .with_progress(Progress::fraction(0.5).with_label("report.pdf"));
    let a = toast(&base, &ImageFiles::default(), shown());
    let b = toast(&later, &ImageFiles::default(), shown());
    assert_eq!(a.template, b.template);
    assert_ne!(a.values, b.values);
    assert!(a.xml.contains(
        "<progress title=\"{progressTitle}\" value=\"{progressValue}\" status=\"{progressStatus}\"/>"
    ));
    assert_eq!(
        values_text(&b),
        "title=Downloading\nprogressTitle=report.pdf\nprogressValue=0.5000\nprogressStatus=Downloading...\n"
    );
    let indeterminate = base.with_progress(Progress::indeterminate());
    let c = toast(&indeterminate, &ImageFiles::default(), shown());
    assert!(values_text(&c).contains("progressValue=indeterminate"));
    assert!(values_text(&c).contains("progressStatus=Downloading..."));
}

#[test]
fn status_text_is_attribution_without_progress() {
    let n = Notification::new("n", "T")
        .unwrap()
        .with_status_text("via SMS");
    let t = toast(&n, &ImageFiles::default(), shown());
    assert!(
        t.xml
            .contains("<text placement=\"attribution\">{attribution}</text>")
    );
}

#[test]
fn buttons_inputs_and_carried_fields() {
    let mut reply = Action::reply("reply", "Reply")
        .unwrap()
        .with_reply_options(ReplyOptions {
            placeholder: Some("Type a reply".to_owned()),
            submit_title: Some("Send".to_owned()),
            suggestions: Vec::new(),
        });
    reply.opens_app = false;
    let n = Notification::new("chat:1", "Ana")
        .unwrap()
        .with_user_data("u=1")
        .with_url("app://chat/1")
        .with_action(reply)
        .with_action(Action::select("mood", "Mood", ["Good", "Bad"]).unwrap())
        .with_action(Action::button("del", "Delete").unwrap().destructive());
    let t = toast(&n, &ImageFiles::default(), shown());
    let actions = t.xml.split_once("<actions>").unwrap().1;
    assert_eq!(
        actions,
        "<input type=\"text\" id=\"reply\" placeHolderContent=\"Type a reply\"/>\
         <input type=\"selection\" id=\"mood\" defaultInput=\"Good\">\
         <selection id=\"Good\" content=\"Good\"/><selection id=\"Bad\" content=\"Bad\"/></input>\
         <action content=\"Send\" arguments=\"istmo1&amp;i=chat:1&amp;k=reply&amp;u=u%3D1&amp;l=app://chat/1\" \
         activationType=\"background\" hint-inputId=\"reply\"/>\
         <action content=\"Mood\" arguments=\"istmo1&amp;i=chat:1&amp;k=mood&amp;u=u%3D1&amp;l=app://chat/1\" \
         activationType=\"foreground\" hint-inputId=\"mood\"/>\
         <action content=\"Delete\" arguments=\"istmo1&amp;i=chat:1&amp;k=del&amp;u=u%3D1&amp;l=app://chat/1\" \
         activationType=\"foreground\" hint-buttonStyle=\"Critical\"/>\
         </actions></toast>"
    );
    assert!(t.xml.starts_with(
        "<toast launch=\"istmo1&amp;i=chat:1&amp;u=u%3D1&amp;l=app://chat/1\" useButtonStyle=\"true\">"
    ));
    // Windows 10 has no styled buttons.
    let old = toast(
        &n,
        &ImageFiles::default(),
        Options {
            build: 19_045,
            ..shown()
        },
    );
    assert!(!old.xml.contains("hint-buttonStyle"));
    assert!(!old.xml.contains("useButtonStyle"));
}

#[test]
fn carried_user_data_shrinks_until_the_xml_fits() {
    let mut n = Notification::new("n", "T")
        .unwrap()
        .with_user_data("x".repeat(3 * 1024));
    for i in 0..4 {
        n = n.with_action(Action::button(&format!("b{i}"), "B").unwrap());
    }
    let rendered = render(&n, &limits());
    let t = build_fitting(&n, &rendered, &ImageFiles::default(), shown()).unwrap();
    assert!(t.xml.len() <= MAX_XML);
    // On the body only.
    assert_eq!(t.xml.matches("&amp;u=").count(), 1);
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one case per scenario, read top to bottom"
)]
fn images_scenarios_and_audio() {
    let ana = Participant::new("ana", "Ana").with_avatar(NotificationImage::png(vec![1]));
    let chat = Notification::new("c", "Chat")
        .unwrap()
        .with_icon(NotificationImage::png(vec![2]))
        .with_style(Style::Messaging(Conversation {
            user: Participant::new("me", "Me"),
            title: None,
            is_group: false,
            messages: vec![
                ChatMessage::received(ana.clone(), "hi")
                    .with_image(NotificationImage::png(vec![3])),
            ],
        }));
    let images = files(&[
        (ImageSlot::Avatar("ana".into()), r"C:\cache\a.png"),
        (ImageSlot::Icon, r"C:\cache\i.png"),
        (ImageSlot::Message(0), r"C:\cache\m 1.png"),
    ]);
    let t = toast(&chat, &images, shown());
    assert!(t.xml.contains(
        "<image placement=\"appLogoOverride\" hint-crop=\"circle\" src=\"file:///C:/cache/a.png\"/>\
         <image src=\"file:///C:/cache/m%201.png\"/>"
    ));

    let call = Notification::new("call", "Ana")
        .unwrap()
        .with_style(Style::Call(CallInfo {
            caller: ana,
            kind: crate::CallKind::Incoming,
            video: false,
        }));
    let t = toast(&call, &ImageFiles::default(), shown());
    assert!(t.xml.contains("scenario=\"incomingCall\""));
    assert!(
        t.xml
            .contains("<text hint-callScenarioCenterAlign=\"true\">{title}</text>")
    );
    assert!(
        t.xml
            .contains("arguments=\"istmo1&amp;i=call&amp;k=istmo.call.answer\"")
    );

    let picture = Notification::new("p", "P")
        .unwrap()
        .with_style(Style::Picture(NotificationImage::file(r"D:\pics\x.jpg")));
    let t = toast(
        &picture,
        &files(&[(ImageSlot::Picture, r"D:\pics\x.jpg")]),
        shown(),
    );
    assert!(
        t.xml
            .contains("<image placement=\"hero\" src=\"file:///D:/pics/x.jpg\"/>")
    );

    let urgent = Notification::new("u", "U")
        .unwrap()
        .with_interruption(Interruption::Urgent)
        .with_sound(Sound::Named(
            "ms-winsoundevent:Notification.Reminder".into(),
        ));
    let t = toast(&urgent, &ImageFiles::default(), shown());
    assert!(t.xml.contains("scenario=\"urgent\" duration=\"long\""));
    assert!(
        t.xml
            .contains("<audio src=\"ms-winsoundevent:Notification.Reminder\"/>")
    );
    let t = toast(
        &urgent,
        &ImageFiles::default(),
        Options {
            build: 19_045,
            ..shown()
        },
    );
    assert!(!t.xml.contains("scenario"));

    let bundled = Notification::new("s", "S")
        .unwrap()
        .with_sound(Sound::Named("ding".into()));
    assert!(
        !toast(&bundled, &ImageFiles::default(), shown())
            .xml
            .contains("<audio")
    );
    let quiet = Notification::new("q", "Q")
        .unwrap()
        .with_interruption(Interruption::Passive);
    assert!(
        toast(&quiet, &ImageFiles::default(), shown())
            .xml
            .contains("<audio silent=\"true\"/>")
    );

    // An alarm without buttons would be shown as a normal toast anyway.
    let alarm = Notification::new("a", "A")
        .unwrap()
        .with_category(Category::Alarm);
    assert!(
        !toast(&alarm, &ImageFiles::default(), shown())
            .xml
            .contains("scenario")
    );
    let alarm = alarm.with_action(Action::button("stop", "Stop").unwrap());
    assert!(
        toast(&alarm, &ImageFiles::default(), shown())
            .xml
            .contains("scenario=\"alarm\"")
    );
}

#[test]
fn groups_get_a_header_and_timestamps_are_iso() {
    let n = Notification::new("m", "M")
        .unwrap()
        .with_group("inbox")
        .unwrap()
        .with_timestamp(
            std::time::UNIX_EPOCH + std::time::Duration::from_millis(1_700_000_000_123),
        );
    let t = toast(&n, &ImageFiles::default(), shown());
    assert!(t.xml.contains("displayTimestamp=\"2023-11-14T22:13:20Z\""));
    assert!(t.xml.ends_with(
        "<header id=\"inbox\" title=\"inbox\" arguments=\"istmo1&amp;i=m\" activationType=\"foreground\"/></toast>"
    ));
}

#[test]
fn control_characters_never_reach_the_xml() {
    let n = Notification::new("n", "a\u{1}b").unwrap();
    let options = Options {
        bind: false,
        ..shown()
    };
    assert!(
        toast(&n, &ImageFiles::default(), options)
            .xml
            .contains("<text>ab</text>")
    );
}

#[test]
fn scheduled_toasts_use_only_file_images() {
    let n = Notification::new("n", "N")
        .unwrap()
        .with_icon(NotificationImage::png(vec![1]))
        .with_style(Style::Picture(NotificationImage::file(r"C:\p.png")));
    let images = file_images(&n);
    assert_eq!(images.get(&ImageSlot::Icon), None);
    assert_eq!(
        images.get(&ImageSlot::Picture),
        Some(std::path::Path::new(r"C:\p.png"))
    );
}
