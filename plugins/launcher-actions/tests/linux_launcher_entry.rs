//! `LauncherEntry` against a private session bus.
//!
//! The parent test starts its own `dbus-daemon` and runs the real check
//! in a child copy of this binary whose `DBUS_SESSION_BUS_ADDRESS` points
//! at it (no process-wide `set_var`). Skipped when `dbus-daemon` is not
//! installed.

#![cfg(target_os = "linux")]

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

use istmo_launcher_actions::{Attention, Badge, DesktopLauncherActions, LauncherActions, Progress};
use zbus::MatchRule;
use zbus::blocking::{Connection, MessageIterator};
use zbus::message::Type;
use zbus::zvariant::OwnedValue;

const CHILD: &str = "ISTMO_LA_DBUS_CHILD";
const PATH: &str = "/dev/istmo/LauncherEntry";
const INTERFACE: &str = "com.canonical.Unity.LauncherEntry";
const URI: &str = "application://dev.istmo.launcher_test.desktop";

type Props = HashMap<String, OwnedValue>;

#[test]
fn launcher_entry_signals_and_answers_query() {
    if std::env::var_os(CHILD).is_some() {
        return child();
    }
    let Ok(mut daemon) = Command::new("dbus-daemon")
        .args(["--session", "--nofork", "--nopidfile", "--print-address=1"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        eprintln!("dbus-daemon not installed: LauncherEntry test skipped");
        return;
    };
    let mut address = String::new();
    let stdout = daemon.stdout.take().expect("piped stdout");
    BufReader::new(stdout)
        .read_line(&mut address)
        .expect("bus address");
    let status = Command::new(std::env::current_exe().expect("test binary"))
        .args([
            "--exact",
            "launcher_entry_signals_and_answers_query",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .env("DBUS_SESSION_BUS_ADDRESS", address.trim())
        .env_remove("FLATPAK_ID")
        .status()
        .expect("run child");
    let _ = daemon.kill();
    let _ = daemon.wait();
    assert!(status.success(), "child failed: {status}");
}

fn child() {
    let observer = Connection::session().expect("observer connection");
    let rule = MatchRule::builder()
        .msg_type(Type::Signal)
        .interface(INTERFACE)
        .expect("interface")
        .member("Update")
        .expect("member")
        .build();
    let mut updates = MessageIterator::for_match_rule(rule, &observer, Some(16)).expect("match");
    let backend = DesktopLauncherActions::default().with_desktop_id("dev.istmo.launcher_test");

    pollster::block_on(backend.set_badge(Badge::Count(5))).expect("badge");
    let message = updates.next().expect("an Update").expect("message");
    let (uri, props): (String, Props) = message.body().deserialize().expect("Update body");
    assert_eq!(uri, URI);
    assert_eq!(int(&props, "count"), Some(5));
    assert_eq!(boolean(&props, "count-visible"), Some(true));
    assert!(!props.contains_key("progress"), "only changed properties");

    pollster::block_on(backend.set_progress(Progress::Normal(0.25))).expect("progress");
    let message = updates.next().expect("an Update").expect("message");
    let (_, props): (String, Props) = message.body().deserialize().expect("Update body");
    assert_eq!(double(&props, "progress"), Some(0.25));
    assert_eq!(boolean(&props, "progress-visible"), Some(true));
    assert!(!props.contains_key("count"));

    pollster::block_on(backend.request_attention(Attention::Critical)).expect("attention");
    let message = updates.next().expect("an Update").expect("message");
    let (_, props): (String, Props) = message.body().deserialize().expect("Update body");
    assert_eq!(boolean(&props, "urgent"), Some(true));

    // A dock that starts now asks the object for everything.
    let sender = message.header().sender().expect("sender").to_owned();
    let reply = observer
        .call_method(Some(sender), PATH, Some(INTERFACE), "Query", &())
        .expect("Query");
    let (uri, props): (String, Props) = reply.body().deserialize().expect("Query body");
    assert_eq!(uri, URI);
    assert_eq!(int(&props, "count"), Some(5));
    assert_eq!(double(&props, "progress"), Some(0.25));
    assert_eq!(boolean(&props, "urgent"), Some(true));

    assert_eq!(
        pollster::block_on(backend.set_badge(Badge::Label("new".into()))),
        Err(istmo_launcher_actions::LauncherError::Unsupported)
    );
}

fn int(props: &Props, key: &str) -> Option<i64> {
    props.get(key).and_then(|v| i64::try_from(v).ok())
}

fn double(props: &Props, key: &str) -> Option<f64> {
    props.get(key).and_then(|v| f64::try_from(v).ok())
}

fn boolean(props: &Props, key: &str) -> Option<bool> {
    props.get(key).and_then(|v| bool::try_from(v).ok())
}
