use std::error::Error;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use istmo_core::{NativeHandle, StreamItem};
use istmo_tray::istmo_menu::{
    CheckState, IconImage, IconRef, ImageData, ItemChange, ItemId, ItemPatch, MenuBuilder,
    MenuNode, Modifiers, RadioGroupId, Shortcut,
};
use istmo_tray::{
    Animation, AnimationFrame, IconTag, Repeat, TrayClient, TrayEvent, TrayIcon, TraySpec,
};

const CLOCK: u32 = 2;
const SHOW_TITLE: u32 = 3;
const STATIC: u32 = 11;
const ANIMATED: u32 = 12;
const SWAPPED: u32 = 13;
const RESET: u32 = 20;
const QUIT: u32 = 99;
const ICON_MODE: RadioGroupId = RadioGroupId { value: 1 };

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Static,
    Animated,
    Swapped,
}

fn svg(body: &str) -> IconImage {
    let svg =
        format!(r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32">{body}</svg>"#);
    IconImage::new(ImageData::Svg(svg.into_bytes())).template()
}

fn static_icon() -> IconImage {
    svg(
        r##"<circle cx="16" cy="16" r="12" fill="#3b82f6"/><circle cx="16" cy="16" r="5" fill="#fff"/>"##,
    )
}

/// One frame of a spinner, turned `step` eighths.
fn spinner(step: u32) -> IconImage {
    let angle = step * 45;
    svg(&format!(
        r##"<g transform="rotate({angle} 16 16)"><circle cx="16" cy="16" r="11" fill="none" stroke="#94a3b8" stroke-width="4"/><path d="M16 5 A11 11 0 0 1 27 16" fill="none" stroke="#3b82f6" stroke-width="4" stroke-linecap="round"/></g>"##
    ))
}

/// A battery filled to `level` of 4.
fn battery(level: u32) -> IconImage {
    let width = level * 5;
    let colour = if level <= 1 { "#ef4444" } else { "#22c55e" };
    svg(&format!(
        r##"<rect x="3" y="9" width="24" height="14" rx="2" fill="none" stroke="#64748b" stroke-width="2"/><rect x="27" y="13" width="3" height="6" fill="#64748b"/><rect x="5" y="11" width="{width}" height="10" fill="{colour}"/>"##
    ))
}

fn menu(clock: &str) -> Vec<MenuNode> {
    MenuBuilder::default()
        .header(1, "istmo tray demo")
        .action(CLOCK, clock)
        .disabled()
        .check(SHOW_TITLE, "Show the clock as the &title", CheckState::Off)
        .separator(4)
        .submenu(10, "&Icon", |m| {
            m.radio(STATIC, "&Static", ICON_MODE, true)
                .radio(ANIMATED, "&Animated", ICON_MODE, false)
                .radio(SWAPPED, "Swapped by &handle", ICON_MODE, false)
        })
        .action(RESET, "&Reset the menu")
        .subtitle("Sends the whole menu again")
        .badge("new")
        .separator(30)
        .action(QUIT, "&Quit")
        .shortcut(Shortcut {
            modifiers: Modifiers::CONTROL,
            key: "Q".to_owned(),
        })
        .into_nodes()
}

fn clock() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let day = secs % 86_400;
    format!(
        "UTC {:02}:{:02}:{:02}",
        day / 3600,
        day % 3600 / 60,
        day % 60
    )
}

pub(crate) fn run(tray: &TrayClient) -> Result<(), Box<dyn Error>> {
    let caps = pollster::block_on(tray.capabilities())?;
    println!(
        "backend {:?}, host {:?}, radio {:?}, title {:?}",
        caps.backend, caps.host, caps.menu.radio, caps.title
    );

    // Icons swapped every second: decoded once, switched by handle.
    let batteries: Vec<NativeHandle<IconTag>> = (0..=4)
        .map(|level| pollster::block_on(tray.register_icon_owned(battery(level))).map(|i| i.icon))
        .collect::<Result<_, _>>()?;
    let spin = TrayIcon::Animated(Animation {
        frames: (0..8)
            .map(|step| AnimationFrame {
                icon: IconRef::Inline(spinner(step)),
                duration_ms: 90,
            })
            .collect(),
        repeat: Repeat::Forever,
    });

    let item = pollster::block_on(
        tray.create_owned(
            TraySpec::new("istmo-tray-demo", static_icon())
                .with_tooltip("istmo tray demo")
                .with_menu(menu(&clock())),
        ),
    )?;
    let id = item.tray.id();
    let events = tray.events(id)?;

    let mut mode = Mode::Static;
    let mut show_title = false;
    let mut tick = 0usize;
    loop {
        while let Some(event) = events.try_recv() {
            let StreamItem::Event(event) = event? else {
                return Ok(());
            };
            println!("{event:?}");
            match event {
                TrayEvent::ItemActivated(ItemId { value: QUIT }) => return Ok(()),
                TrayEvent::ItemActivated(ItemId { value: RESET }) => {
                    pollster::block_on(tray.set_menu(id, menu(&clock())))?;
                    mode = Mode::Static;
                    pollster::block_on(tray.set_icon(id, static_icon().into()))?;
                }
                TrayEvent::CheckChanged(change) if change.item.value == SHOW_TITLE => {
                    show_title = change.state == CheckState::On;
                    if !show_title {
                        pollster::block_on(tray.set_title(id, None))?;
                    }
                }
                TrayEvent::RadioSelected(change) => {
                    mode = match change.item.value {
                        ANIMATED => Mode::Animated,
                        SWAPPED => Mode::Swapped,
                        _ => Mode::Static,
                    };
                    let icon = match mode {
                        Mode::Static => static_icon().into(),
                        Mode::Animated => spin.clone(),
                        Mode::Swapped => TrayIcon::Static(IconRef::Registered(batteries[0].id())),
                    };
                    pollster::block_on(tray.set_icon(id, icon))?;
                }
                _ => {}
            }
        }

        let now = clock();
        pollster::block_on(tray.update_items(
            id,
            vec![ItemPatch {
                id: ItemId::from(CLOCK),
                change: ItemChange::Label(now.clone()),
            }],
        ))?;
        if show_title {
            pollster::block_on(tray.set_title(id, Some(now)))?;
        }
        if mode == Mode::Swapped {
            tick += 1;
            let icon = &batteries[tick % batteries.len()];
            pollster::block_on(
                tray.set_icon(id, TrayIcon::Static(IconRef::Registered(icon.id()))),
            )?;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}
