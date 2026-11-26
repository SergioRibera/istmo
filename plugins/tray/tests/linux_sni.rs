//! The Linux backend against a private session bus with a fake
//! `StatusNotifierWatcher`.
//!
//! The bus address reaches the backend through the environment, which a
//! test may not change while other threads run: the test re-runs itself
//! as a child process with `DBUS_SESSION_BUS_ADDRESS` set.

#![cfg(target_os = "linux")]

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use istmo_core::Runtime;
use istmo_tray::istmo_menu::{CheckState, IconImage, ImageData, MenuBuilder, RadioGroupId};
use istmo_tray::{
    CheckChange, DesktopTray, PrimaryAction, RadioChange, TrayClient, TrayEvent, TrayHost, TraySpec,
};
use zbus::blocking::Connection;
use zbus::interface;
use zbus::message::Header;
use zbus::zvariant::OwnedValue;

const CHILD: &str = "ISTMO_TRAY_SNI_CHILD";
const WATCHER: &str = "org.kde.StatusNotifierWatcher";

struct PrivateBus(Child);

impl Drop for PrivateBus {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn start_bus() -> Option<(PrivateBus, String)> {
    let mut daemon = Command::new("dbus-daemon")
        .args(["--session", "--nofork", "--print-address"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let stdout = daemon.stdout.take()?;
    let mut address = String::new();
    BufReader::new(stdout).read_line(&mut address).ok()?;
    Some((PrivateBus(daemon), address.trim().to_owned()))
}

#[test]
fn status_notifier_item_round_trips() {
    if std::env::var_os(CHILD).is_some() {
        scenario();
        return;
    }
    let Some((_bus, address)) = start_bus() else {
        eprintln!("dbus-daemon not available; skipping");
        return;
    };
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "status_notifier_item_round_trips", "--nocapture"])
        .env(CHILD, "1")
        .env("DBUS_SESSION_BUS_ADDRESS", &address)
        .env_remove("WAYLAND_DISPLAY")
        .status()
        .unwrap();
    assert!(status.success());
}

/// Records what items register.
#[derive(Default, Clone)]
struct FakeWatcher {
    items: Arc<Mutex<Vec<(String, String)>>>,
}

#[allow(
    clippy::needless_pass_by_value,
    clippy::unused_self,
    clippy::missing_const_for_fn,
    reason = "D-Bus method shapes the interface macro requires"
)]
#[interface(name = "org.kde.StatusNotifierWatcher")]
impl FakeWatcher {
    fn register_status_notifier_item(&self, service: &str, #[zbus(header)] header: Header<'_>) {
        let sender = header.sender().map(ToString::to_string).unwrap_or_default();
        self.items
            .lock()
            .unwrap()
            .push((sender, service.to_owned()));
    }

    #[zbus(property)]
    fn is_status_notifier_host_registered(&self) -> bool {
        true
    }
}

fn wait_for<T>(what: &str, mut probe: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(value) = probe() {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn property(bus: &Connection, service: &str, iface: &str, name: &str) -> OwnedValue {
    bus.call_method(
        Some(service),
        if iface.contains("dbusmenu") {
            "/MenuBar"
        } else {
            "/StatusNotifierItem"
        },
        Some("org.freedesktop.DBus.Properties"),
        "Get",
        &(iface, name),
    )
    .unwrap()
    .body()
    .deserialize::<OwnedValue>()
    .unwrap()
}

type Layout = (i32, HashMap<String, OwnedValue>, Vec<OwnedValue>);

fn layout(bus: &Connection, service: &str) -> Layout {
    let (_revision, layout): (u32, Layout) = bus
        .call_method(
            Some(service),
            "/MenuBar",
            Some("com.canonical.dbusmenu"),
            "GetLayout",
            &(0i32, -1i32, Vec::<String>::new()),
        )
        .unwrap()
        .body()
        .deserialize()
        .unwrap();
    layout
}

fn child(layout: &Layout, at: usize) -> Layout {
    Layout::try_from(layout.2[at].try_clone().unwrap()).unwrap()
}

fn label(layout: &Layout) -> String {
    String::try_from(layout.1["label"].try_clone().unwrap()).unwrap()
}

fn toggle_state(layout: &Layout) -> i32 {
    i32::try_from(layout.1["toggle-state"].try_clone().unwrap()).unwrap()
}

fn click(bus: &Connection, service: &str, id: i32) {
    bus.call_method(
        Some(service),
        "/MenuBar",
        Some("com.canonical.dbusmenu"),
        "Event",
        &(id, "clicked", zbus::zvariant::Value::from(""), 0u32),
    )
    .unwrap();
}

fn png(rgba: [u8; 4], size: u32) -> Vec<u8> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, size, size);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().unwrap();
    writer
        .write_image_data(&rgba.repeat((size * size) as usize))
        .unwrap();
    drop(writer);
    out
}

fn scenario() {
    let watcher = FakeWatcher::default();
    let _watcher_bus = zbus::blocking::connection::Builder::session()
        .unwrap()
        .name(WATCHER)
        .unwrap()
        .serve_at("/StatusNotifierWatcher", watcher.clone())
        .unwrap()
        .build()
        .unwrap();
    let bus = Connection::session().unwrap();

    let init = Runtime::mock()
        .expects::<TrayClient>()
        .host(TrayHost::new(DesktopTray::default()))
        .finish();
    let client = TrayClient::from_runtime(&init.runtime).unwrap();

    let theme = RadioGroupId::from(1);
    let menu = MenuBuilder::default()
        .action(1, "&Open")
        .check(2, "Start at login", CheckState::Off)
        .separator(3)
        .submenu(4, "Theme", |m| {
            m.radio(5, "Light", theme, true)
                .radio(6, "Dark", theme, false)
        })
        .into_nodes();
    let spec = TraySpec::new(
        "istmo-test",
        IconImage::new(ImageData::Png(png([255, 0, 0, 255], 32))),
    )
    .with_title("Test")
    .with_tooltip("Hello")
    .with_menu(menu)
    .with_primary(PrimaryAction::Activate);
    let item = pollster::block_on(client.create_owned(spec)).unwrap();
    let tray = item.tray.id();
    let events = forward(client.events(tray).unwrap());

    let (sender, service) = wait_for("registration", || {
        watcher.items.lock().unwrap().first().cloned()
    });
    assert!(
        service.starts_with("org.kde.StatusNotifierItem-"),
        "{service}"
    );
    assert!(!sender.is_empty());

    check_item(&bus, &service);
    check_clicks(&bus, &service, &events, theme);
    check_pointer(&bus, &service, &events);

    // Property changes are visible on the bus.
    pollster::block_on(client.set_title(tray, Some("Changed".to_owned()))).unwrap();
    wait_for("new title", || {
        (String::try_from(property(&bus, &service, SNI, "Title")).ok()? == "Changed").then_some(())
    });

    // Dropping the handle closes the item's connection.
    drop(item);
    let dbus = zbus::blocking::fdo::DBusProxy::new(&bus).unwrap();
    wait_for("item gone", || {
        let name = zbus::names::BusName::try_from(service.as_str()).unwrap();
        (!dbus.name_has_owner(name).unwrap()).then_some(())
    });
}

const SNI: &str = "org.kde.StatusNotifierItem";

fn check_item(bus: &Connection, service: &str) {
    let sni = SNI;
    assert_eq!(
        String::try_from(property(bus, service, sni, "Title")).unwrap(),
        "Test"
    );
    assert!(!bool::try_from(property(bus, service, sni, "ItemIsMenu")).unwrap());
    let pixmaps: Vec<(i32, i32, Vec<u8>)> = property(bus, service, sni, "IconPixmap")
        .try_into()
        .unwrap();
    assert_eq!(pixmaps.len(), 6);
    let (w, h, argb) = &pixmaps[0];
    assert_eq!((*w, *h), (16, 16));
    assert_eq!(&argb[0..4], &[255, 255, 0, 0]);

    let root = layout(bus, service);
    assert_eq!(root.2.len(), 4);
    assert_eq!(label(&child(&root, 0)), "_Open");
    assert_eq!(toggle_state(&child(&root, 1)), 0);
    let theme_menu = child(&root, 3);
    assert_eq!(theme_menu.2.len(), 2);
    assert_eq!(toggle_state(&child(&theme_menu, 0)), 1);
}

/// Clicks run the automatic state changes and reach the app.
fn check_clicks(
    bus: &Connection,
    service: &str,
    events: &flume::Receiver<TrayEvent>,
    theme: RadioGroupId,
) {
    click(bus, service, 3);
    click(bus, service, 7);
    let mut got = Vec::new();
    while got.len() < 4 {
        got.push(events.recv_timeout(Duration::from_secs(5)).unwrap());
    }
    assert_eq!(
        got,
        [
            TrayEvent::ItemActivated(2.into()),
            TrayEvent::CheckChanged(CheckChange {
                item: 2.into(),
                state: CheckState::On
            }),
            TrayEvent::ItemActivated(6.into()),
            TrayEvent::RadioSelected(RadioChange {
                group: theme,
                item: 6.into()
            }),
        ]
    );
    let root = layout(bus, service);
    assert_eq!(toggle_state(&child(&root, 1)), 1);
    let theme_menu = child(&root, 3);
    assert_eq!(toggle_state(&child(&theme_menu, 0)), 0);
    assert_eq!(toggle_state(&child(&theme_menu, 1)), 1);
}

/// Activate (the primary action) and scroll.
fn check_pointer(bus: &Connection, service: &str, events: &flume::Receiver<TrayEvent>) {
    let sni = SNI;
    bus.call_method(
        Some(service),
        "/StatusNotifierItem",
        Some(sni),
        "Activate",
        &(10i32, 20i32),
    )
    .unwrap();
    bus.call_method(
        Some(service),
        "/StatusNotifierItem",
        Some(sni),
        "Scroll",
        &(3i32, "vertical"),
    )
    .unwrap();
    let activate = events.recv_timeout(Duration::from_secs(5)).unwrap();
    let TrayEvent::Activate(pointer) = activate else {
        panic!("expected Activate, got {activate:?}");
    };
    assert_eq!(pointer.anchor.unwrap().rect.x, 10);
    assert!(matches!(
        events.recv_timeout(Duration::from_secs(5)).unwrap(),
        TrayEvent::Scroll(_)
    ));
}

/// The stream's events on a channel that can be read with a timeout.
fn forward(
    stream: istmo_core::TypedStream<TrayEvent, istmo_tray::TrayError>,
) -> flume::Receiver<TrayEvent> {
    let (tx, rx) = flume::unbounded();
    std::thread::spawn(move || {
        while let Ok(istmo_core::StreamItem::Event(event)) = stream.recv() {
            if tx.send(event).is_err() {
                return;
            }
        }
    });
    rx
}
