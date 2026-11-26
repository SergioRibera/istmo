//! macOS: `NSStatusItem` + `NSMenu`, on the main thread.
//!
//! Other threads only post a wake to the main queue (asynchronously, so a
//! caller never waits for the main thread). The app must run the main
//! run loop: winit / eframe do, tray-only apps call
//! [`run_event_loop`].

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};

use istmo_core::NativeHandleId;
use istmo_menu::{CheckState, ItemId, ItemKind, Label, Menu, MenuNode, Modifiers, Shortcut};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{
    AnyThread, ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send,
    sel,
};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSCellImagePosition, NSControlStateValueMixed,
    NSControlStateValueOff, NSControlStateValueOn, NSEventMask, NSEventModifierFlags, NSEventType,
    NSImage, NSMenu, NSMenuDelegate, NSMenuItem, NSMenuItemBadge, NSScreen, NSStatusBar,
    NSStatusItem, NSVariableStatusItemLength,
};
use objc2_foundation::{NSData, NSObject, NSObjectProtocol, NSSize, NSString};

use crate::host::{Backend, Dirty, MenuDirty, Shared, TrayState};
use crate::image::DecodedIcon;
use crate::{
    Anchor, AnchorPrecision, EventSupport, HostState, MenuSupport, Pointer, PrimaryAction, Rect,
    Support, TrayBackend, TrayCapabilities, TrayError, TrayEvent,
};

/// Menu bar icon height, in points.
const BAR_ICON: f64 = 18.0;
/// Menu item icon size, in points.
const ITEM_ICON: f64 = 16.0;

struct Mac {
    shared: Weak<Shared>,
    /// A wake is queued on the main queue and has not run yet.
    queued: Arc<AtomicBool>,
}

impl Backend for Mac {
    fn capabilities(&self) -> TrayCapabilities {
        capabilities()
    }

    fn wake(&self) {
        if self.queued.swap(true, Ordering::AcqRel) {
            return;
        }
        let shared = self.shared.clone();
        let queued = Arc::clone(&self.queued);
        dispatch2::DispatchQueue::main().exec_async(move || {
            queued.store(false, Ordering::Release);
            let Some(mtm) = MainThreadMarker::new() else {
                return;
            };
            if let Some(shared) = shared.upgrade() {
                apply_pending(mtm, &shared);
            }
        });
    }
}

#[expect(clippy::unnecessary_wraps, reason = "has the `StartBackend` signature")]
pub(super) fn start(shared: &Arc<Shared>) -> Result<Arc<dyn Backend>, TrayError> {
    // The menu bar is always there.
    shared.set_host(HostState::Available);
    Ok(Arc::new(Mac {
        shared: Arc::downgrade(shared),
        queued: Arc::new(AtomicBool::new(false)),
    }))
}

/// Runs the application's main run loop, for apps whose only UI is the
/// tray. `dock_icon: false` keeps the app out of the Dock and the app
/// switcher. Never returns.
///
/// # Panics
///
/// When called off the main thread.
pub fn run_event_loop(dock_icon: bool) -> ! {
    let mtm = MainThreadMarker::new().expect("run_event_loop must be called on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    let policy = if dock_icon {
        NSApplicationActivationPolicy::Regular
    } else {
        NSApplicationActivationPolicy::Accessory
    };
    let _changed = app.setActivationPolicy(policy);
    app.run();
    std::process::exit(0)
}

thread_local! {
    /// The main thread's status items.
    static ITEMS: RefCell<HashMap<NativeHandleId, Item>> = RefCell::new(HashMap::new());
}

/// One status item and what it was built from.
struct Item {
    status: Retained<NSStatusItem>,
    target: Retained<Target>,
    menu: Retained<NSMenu>,
    menu_items: HashMap<ItemId, Retained<NSMenuItem>>,
    primary: PrimaryAction,
}

impl Drop for Item {
    fn drop(&mut self) {
        NSStatusBar::systemStatusBar().removeStatusItem(&self.status);
    }
}

fn apply_pending(mtm: MainThreadMarker, shared: &Arc<Shared>) {
    for (tray, dirty) in shared.take_pending() {
        apply(mtm, shared, tray, &dirty);
    }
}

fn apply(mtm: MainThreadMarker, shared: &Arc<Shared>, tray: NativeHandleId, dirty: &Dirty) {
    if dirty.removed {
        // Dropped outside the map borrow: removing the item may call
        // back into the menu delegate.
        let removed = ITEMS.with(|items| items.borrow_mut().remove(&tray));
        drop(removed);
        if !dirty.added {
            return;
        }
    }
    if dirty.added {
        let Some(item) = create(mtm, shared, tray) else {
            return;
        };
        let replaced = ITEMS.with(|items| items.borrow_mut().insert(tray, item));
        drop(replaced);
        return;
    }
    ITEMS.with(|items| {
        let items = items.borrow();
        let Some(item) = items.get(&tray) else {
            return;
        };
        let state = shared.lock();
        let Some(entry) = state.trays.get(&tray) else {
            return;
        };
        if dirty.icon || dirty.title {
            set_button(mtm, item, entry);
        }
        if dirty.tooltip {
            set_tooltip(mtm, item, entry);
        }
        if dirty.visible {
            item.status.setVisible(entry.visible);
        }
        match &dirty.menu {
            MenuDirty::Clean => {}
            MenuDirty::Patches(patches) => {
                for patch in patches {
                    if let (Some(node), Some(menu_item)) =
                        (entry.menu.get(patch.id), item.menu_items.get(&patch.id))
                    {
                        configure(menu_item, node, entry);
                    }
                }
            }
            MenuDirty::Rebuild => {
                drop(items);
                rebuild_menu(mtm, tray, entry);
            }
        }
        drop(state);
    });
}

fn create(mtm: MainThreadMarker, shared: &Arc<Shared>, tray: NativeHandleId) -> Option<Item> {
    let state = shared.lock();
    let entry = state.trays.get(&tray)?;
    let status = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
    status.setAutosaveName(Some(&NSString::from_str(&entry.id)));
    let target = Target::new(mtm, Arc::downgrade(shared), tray);
    let menu = NSMenu::new(mtm);
    menu.setAutoenablesItems(false);
    menu.setDelegate(Some(ProtocolObject::from_ref(&*target)));
    let mut item = Item {
        status,
        target,
        menu,
        menu_items: HashMap::new(),
        primary: entry.primary,
    };
    fill_menu(
        mtm,
        &item.menu,
        &mut item.menu_items,
        &item.target,
        &entry.menu,
        None,
        entry,
    );
    match entry.primary {
        PrimaryAction::Activate => {
            if let Some(button) = item.status.button(mtm) {
                // SAFETY: the target outlives the button (both live in
                // `Item`, the button is removed with the status item) and
                // `buttonClicked:` takes one object argument.
                unsafe {
                    button.setTarget(Some(&item.target));
                }
                // SAFETY: as above.
                unsafe {
                    button.setAction(Some(sel!(buttonClicked:)));
                }
                let _old = button.sendActionOn(
                    NSEventMask::LeftMouseUp
                        | NSEventMask::RightMouseUp
                        | NSEventMask::OtherMouseUp,
                );
            }
        }
        // `ShowMenu`, and actions added later.
        _ => item.status.setMenu(Some(&item.menu)),
    }
    set_button(mtm, &item, entry);
    set_tooltip(mtm, &item, entry);
    item.status.setVisible(entry.visible);
    drop(state);
    Some(item)
}

fn rebuild_menu(mtm: MainThreadMarker, tray: NativeHandleId, entry: &TrayState) {
    ITEMS.with(|items| {
        let mut items = items.borrow_mut();
        let Some(item) = items.get_mut(&tray) else {
            return;
        };
        item.menu.removeAllItems();
        item.menu_items.clear();
        let Item {
            menu,
            menu_items,
            target,
            ..
        } = item;
        fill_menu(mtm, menu, menu_items, target, &entry.menu, None, entry);
    });
}

fn image(icon: &DecodedIcon, points: f64) -> Option<Retained<NSImage>> {
    let image = if let Some(name) = icon.name() {
        NSImage::imageWithSystemSymbolName_accessibilityDescription(
            &NSString::from_str(name),
            None,
        )?
    } else {
        // Two device pixels per point: sharp on Retina, scaled down
        // by AppKit elsewhere.
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "icon sizes are small positive constants"
        )]
        let pixels = (points * 2.0) as u32;
        let png = icon.raster(pixels)?.to_png();
        NSImage::initWithData(NSImage::alloc(), &NSData::with_bytes(&png))?
    };
    image.setSize(NSSize::new(points, points));
    image.setTemplate(icon.is_template());
    Some(image)
}

fn set_button(mtm: MainThreadMarker, item: &Item, entry: &TrayState) {
    let Some(button) = item.status.button(mtm) else {
        return;
    };
    button.setImage(image(entry.icon.frame(), BAR_ICON).as_deref());
    let title = entry.title.as_deref().unwrap_or_default();
    button.setTitle(&NSString::from_str(title));
    button.setImagePosition(if title.is_empty() {
        NSCellImagePosition::ImageOnly
    } else {
        NSCellImagePosition::ImageLeft
    });
}

fn set_tooltip(mtm: MainThreadMarker, item: &Item, entry: &TrayState) {
    let Some(button) = item.status.button(mtm) else {
        return;
    };
    let text = entry.tooltip.as_ref().map(|tip| {
        tip.body.as_ref().map_or_else(
            || NSString::from_str(&tip.title),
            |body| NSString::from_str(&format!("{}\n{body}", tip.title)),
        )
    });
    button.setToolTip(text.as_deref());
}

fn fill_menu(
    mtm: MainThreadMarker,
    menu: &NSMenu,
    menu_items: &mut HashMap<ItemId, Retained<NSMenuItem>>,
    target: &Target,
    model: &Menu,
    parent: Option<ItemId>,
    entry: &TrayState,
) {
    for node in model.children(parent) {
        let menu_item = if matches!(node.kind, ItemKind::Separator) {
            NSMenuItem::separatorItem(mtm)
        } else if matches!(node.kind, ItemKind::Header) && has_section_headers() {
            NSMenuItem::sectionHeaderWithTitle(&NSString::from_str(&label(node)), mtm)
        } else {
            let (key, modifiers) = node.shortcut.as_ref().map_or_else(
                || (String::new(), NSEventModifierFlags::empty()),
                key_equivalent,
            );
            // SAFETY: `itemClicked:` takes one object argument; the
            // target set below outlives the item (both live in `Item`).
            let menu_item = unsafe {
                NSMenuItem::initWithTitle_action_keyEquivalent(
                    NSMenuItem::alloc(mtm),
                    &NSString::from_str(&label(node)),
                    Some(sel!(itemClicked:)),
                    &NSString::from_str(&key),
                )
            };
            menu_item.setKeyEquivalentModifierMask(modifiers);
            // SAFETY: as above.
            unsafe { menu_item.setTarget(Some(target)) };
            menu_item.setTag(isize::try_from(node.id.value).unwrap_or(isize::MAX));
            menu_item
        };
        configure(&menu_item, node, entry);
        if matches!(node.kind, ItemKind::Submenu) {
            let submenu = NSMenu::new(mtm);
            submenu.setAutoenablesItems(false);
            fill_menu(
                mtm,
                &submenu,
                menu_items,
                target,
                model,
                Some(node.id),
                entry,
            );
            menu_item.setSubmenu(Some(&submenu));
        }
        menu.addItem(&menu_item);
        menu_items.insert(node.id, menu_item);
    }
}

/// The text shown (mnemonics have no meaning in macOS menus).
fn label(node: &MenuNode) -> String {
    Label::parse(&node.label).text().to_owned()
}

/// `sectionHeaderWithTitle:` exists (macOS 14).
fn has_section_headers() -> bool {
    NSMenuItem::class()
        .metaclass()
        .responds_to(sel!(sectionHeaderWithTitle:))
}

/// Applies every property of `node` that can change in place.
fn configure(menu_item: &NSMenuItem, node: &MenuNode, entry: &TrayState) {
    if matches!(node.kind, ItemKind::Separator) {
        menu_item.setHidden(!node.visible);
        return;
    }
    menu_item.setTitle(&NSString::from_str(&label(node)));
    menu_item.setEnabled(node.enabled && !matches!(node.kind, ItemKind::Header));
    menu_item.setHidden(!node.visible);
    match node.kind {
        ItemKind::Check(check) => menu_item.setState(match check.state {
            CheckState::On => NSControlStateValueOn,
            CheckState::Off => NSControlStateValueOff,
            // `Mixed`, and states added later.
            _ => NSControlStateValueMixed,
        }),
        ItemKind::Radio(radio) => {
            menu_item.setState(if radio.selected {
                NSControlStateValueOn
            } else {
                NSControlStateValueOff
            });
            let dot = NSImage::imageWithSystemSymbolName_accessibilityDescription(
                &NSString::from_str("circle.fill"),
                None,
            );
            if let Some(dot) = &dot {
                dot.setSize(NSSize::new(6.0, 6.0));
            }
            // SAFETY: the image is retained by the item.
            unsafe { menu_item.setOnStateImage(dot.as_deref()) };
        }
        // Actions, submenus, headers, and kinds added later.
        _ => {}
    }
    let icon = entry
        .item_icons
        .get(&node.id)
        .and_then(|icon| image(icon, ITEM_ICON));
    menu_item.setImage(icon.as_deref());
    if menu_item.respondsToSelector(sel!(setSubtitle:)) {
        let subtitle = node.subtitle.as_deref().map(NSString::from_str);
        menu_item.setSubtitle(subtitle.as_deref());
    }
    if menu_item.respondsToSelector(sel!(setBadge:)) {
        let badge = node.badge.as_deref().map(|text| {
            // SAFETY: `initWithString:` takes an `NSString`; the badge
            // class exists wherever `setBadge:` does.
            let badge: Retained<NSMenuItemBadge> = unsafe {
                msg_send![NSMenuItemBadge::alloc(), initWithString: &*NSString::from_str(text)]
            };
            badge
        });
        menu_item.setBadge(badge.as_deref());
    }
}

fn key_equivalent(shortcut: &Shortcut) -> (String, NSEventModifierFlags) {
    let mut flags = NSEventModifierFlags::empty();
    for (bit, flag) in [
        (Modifiers::SHIFT, NSEventModifierFlags::Shift),
        (Modifiers::CONTROL, NSEventModifierFlags::Control),
        (Modifiers::ALT, NSEventModifierFlags::Option),
        (Modifiers::SUPER, NSEventModifierFlags::Command),
    ] {
        if shortcut.modifiers.contains(bit) {
            flags |= flag;
        }
    }
    let named = |code: u32| char::from_u32(code).map(String::from);
    let key = match shortcut.key.as_str() {
        "Enter" => Some("\r".to_owned()),
        "Escape" => Some("\u{1b}".to_owned()),
        "Tab" => Some("\t".to_owned()),
        "Space" => Some(" ".to_owned()),
        "Backspace" => Some("\u{8}".to_owned()),
        "Delete" => Some("\u{7f}".to_owned()),
        "Up" => named(0xF700),
        "Down" => named(0xF701),
        "Left" => named(0xF702),
        "Right" => named(0xF703),
        "Insert" => named(0xF727),
        "Home" => named(0xF729),
        "End" => named(0xF72B),
        "PageUp" => named(0xF72C),
        "PageDown" => named(0xF72D),
        key => key
            .strip_prefix('F')
            .and_then(|n| n.parse::<u32>().ok())
            .filter(|n| (1..=24).contains(n))
            .and_then(|n| named(0xF704 + n - 1))
            .or_else(|| Some(key.to_lowercase())),
    };
    (key.unwrap_or_default(), flags)
}

/// The status item's frame, in physical pixels from the top-left of the
/// main screen.
fn anchor(mtm: MainThreadMarker, status: &NSStatusItem) -> Option<Anchor> {
    let window = status.button(mtm)?.window()?;
    let frame = window.frame();
    let scale = window.backingScaleFactor();
    let screens = NSScreen::screens(mtm);
    let main_height = screens.firstObject()?.frame().size.height;
    let top = main_height - (frame.origin.y + frame.size.height);
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "screen coordinates fit an i32; sizes are positive"
    )]
    let rect = Rect {
        x: (frame.origin.x * scale).round() as i32,
        y: (top * scale).round() as i32,
        width: (frame.size.width * scale).round() as u32,
        height: (frame.size.height * scale).round() as u32,
    };
    Some(Anchor {
        rect,
        scale,
        precision: AnchorPrecision::IconRect,
    })
}

struct TargetIvars {
    shared: Weak<Shared>,
    tray: NativeHandleId,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; the target does
    // not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "IstmoTrayTarget"]
    #[ivars = TargetIvars]
    struct Target;

    impl Target {
        #[unsafe(method(itemClicked:))]
        fn item_clicked(&self, sender: &NSMenuItem) {
            let Some(shared) = self.ivars().shared.upgrade() else {
                return;
            };
            if let Ok(item) = u32::try_from(sender.tag()) {
                shared.activate_item(self.ivars().tray, ItemId::from(item));
            }
        }

        #[unsafe(method(buttonClicked:))]
        fn button_clicked(&self, _sender: &AnyObject) {
            self.on_button();
        }
    }

    unsafe impl NSObjectProtocol for Target {}

    unsafe impl NSMenuDelegate for Target {
        #[unsafe(method(menuWillOpen:))]
        fn menu_will_open(&self, _menu: &NSMenu) {
            self.emit(TrayEvent::MenuOpened);
        }

        #[unsafe(method(menuDidClose:))]
        fn menu_did_close(&self, _menu: &NSMenu) {
            self.emit(TrayEvent::MenuClosed);
            let tray = self.ivars().tray;
            // With `Activate` the menu is attached only while a right
            // click shows it.
            ITEMS.with(|items| {
                if let Ok(items) = items.try_borrow()
                    && let Some(item) = items.get(&tray)
                    && item.primary == PrimaryAction::Activate
                {
                    item.status.setMenu(None);
                }
            });
        }
    }
);

impl Target {
    fn new(mtm: MainThreadMarker, shared: Weak<Shared>, tray: NativeHandleId) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(TargetIvars { shared, tray });
        // SAFETY: `NSObject`'s `init` on a freshly allocated object.
        unsafe { msg_send![super(this), init] }
    }

    fn emit(&self, event: TrayEvent) {
        if let Some(shared) = self.ivars().shared.upgrade() {
            shared.emit(self.ivars().tray, event);
        }
    }

    /// A click on the status button with `PrimaryAction::Activate`.
    fn on_button(&self) {
        let mtm = self.mtm();
        let kind = NSApplication::sharedApplication(mtm)
            .currentEvent()
            .map(|event| event.r#type());
        let tray = self.ivars().tray;
        let found = ITEMS.with(|items| {
            let items = items.try_borrow().ok()?;
            let item = items.get(&tray)?;
            Some((item.status.clone(), item.menu.clone()))
        });
        let Some((status, menu)) = found else {
            return;
        };
        let pointer = Pointer {
            anchor: anchor(mtm, &status),
        };
        match kind {
            Some(NSEventType::RightMouseUp) => {
                status.setMenu(Some(&menu));
                if let Some(button) = status.button(mtm) {
                    // SAFETY: a click on our own button with no sender.
                    unsafe { button.performClick(None) };
                }
            }
            Some(NSEventType::OtherMouseUp) => self.emit(TrayEvent::SecondaryActivate(pointer)),
            _ => self.emit(TrayEvent::Activate(pointer)),
        }
    }
}

const fn capabilities() -> TrayCapabilities {
    TrayCapabilities {
        backend: TrayBackend::AppKit,
        host: HostState::Available,
        max_items: None,
        min_frame_ms: 16,
        image_names: Support::Native,
        template_images: Support::Native,
        title: Support::Native,
        tooltip_body: Support::Emulated,
        menu: MenuSupport {
            check: Support::Native,
            mixed: Support::Native,
            radio: Support::Emulated,
            item_icons: Support::Native,
            shortcuts: Support::Native,
            header: Support::Native,
            subtitle: Support::Native,
            badge: Support::Native,
            default_item: Support::Unsupported,
            mnemonics: Support::Unsupported,
            live_while_open: Support::Native,
        },
        events: EventSupport {
            activate: Support::Native,
            secondary: Support::Native,
            double_click: Support::Unsupported,
            scroll: Support::Unsupported,
            menu_open_close: Support::Native,
        },
        anchor: Some(AnchorPrecision::IconRect),
    }
}
