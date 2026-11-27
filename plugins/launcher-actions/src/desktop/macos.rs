//! macOS: the Dock tile badge, `requestUserAttention`, a Dock menu and
//! `NSDocumentController`'s recent documents.
//!
//! `AppKit` asks only the application delegate for a Dock menu
//! (`applicationDockMenu:`). The delegate belongs to the UI toolkit
//! (winit, …), so on the first `set_actions` the backend adds that method
//! to the delegate's class with `class_addMethod`; the method answers the
//! menu built from the current actions. When the class already implements
//! it (the app has a Dock menu of its own), `set_actions` fails
//! [`LauncherError::Unsupported`] and logs why.
//!
//! Everything runs on the main thread: a call from another thread is
//! queued on the main queue and awaited, never blocking the caller's
//! thread on the main thread.

use std::cell::RefCell;
use std::ffi::CStr;
use std::sync::{Arc, Weak};

use istmo_core::Runtime;
use istmo_plugins::{DeepLink, DeepLinks};
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Imp, NSObject, NSObjectProtocol, Sel};
use objc2::{AnyThread, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSDocumentController, NSImage, NSMenu, NSMenuItem, NSRequestUserAttentionType,
};
use objc2_foundation::{NSData, NSInteger, NSString, NSURL};

use super::Config;
use crate::{
    ActionIcon, Attention, Badge, BadgeSupport, Capabilities, LauncherAction, LauncherError,
    MACOS_MAX_ACTIONS, Progress, action_source,
};

/// Objective-C type encoding of `- (NSMenu *)applicationDockMenu:(NSApplication *)sender`.
const DOCK_MENU_TYPES: &CStr = c"@@:@";

/// Edge of a Dock menu item's image, in points.
const ICON_EDGE: f64 = 16.0;

/// Main-thread state of the Dock menu and attention requests.
#[derive(Default)]
struct Dock {
    actions: Vec<LauncherAction>,
    runtime: Option<Weak<Runtime>>,
    /// The delegate class got our `applicationDockMenu:`.
    installed: bool,
    /// The pending `requestUserAttention` request.
    attention: Option<NSInteger>,
    /// Target of every Dock menu item; lives as long as the process.
    target: Option<Retained<MenuTarget>>,
}

thread_local! {
    /// Only ever touched on the main thread.
    static DOCK: RefCell<Dock> = RefCell::new(Dock::default());
}

/// macOS side of [`super::DesktopLauncherActions`].
pub(crate) struct Platform {
    runtime: Option<Arc<Runtime>>,
}

impl std::fmt::Debug for Platform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Platform").finish_non_exhaustive()
    }
}

/// Run `work` on the main thread: inline when already there, else queued
/// on the main queue and awaited.
async fn on_main<R: Send + 'static>(
    work: impl FnOnce(MainThreadMarker) -> R + Send + 'static,
) -> Result<R, LauncherError> {
    if let Some(mtm) = MainThreadMarker::new() {
        return Ok(work(mtm));
    }
    let (tx, rx) = oneshot::channel();
    dispatch2::DispatchQueue::main().exec_async(move || {
        if let Some(mtm) = MainThreadMarker::new() {
            let _ = tx.send(work(mtm));
        }
    });
    rx.await
        .map_err(|_| LauncherError::Backend("the main queue dropped the call".into()))
}

impl Platform {
    pub(crate) fn new(config: &Config) -> Self {
        Self {
            runtime: config.runtime(),
        }
    }

    pub(crate) const fn capabilities() -> Capabilities {
        Capabilities {
            dynamic_actions: true,
            max_actions: MACOS_MAX_ACTIONS,
            pin: false,
            badge: BadgeSupport::Label,
            // Dock progress needs a custom tile view: drawing.
            progress: false,
            attention: true,
            recent_documents: true,
        }
    }

    pub(crate) async fn set_actions(
        &self,
        actions: Vec<LauncherAction>,
    ) -> Result<(), LauncherError> {
        let runtime = self
            .runtime
            .as_ref()
            .map(Arc::downgrade)
            .or_else(|| Runtime::global().ok().as_ref().map(Arc::downgrade));
        on_main(move |mtm| {
            if !actions.is_empty() {
                install_dock_menu(mtm)?;
            }
            DOCK.with(|dock| {
                let mut dock = dock.borrow_mut();
                dock.actions = actions;
                dock.runtime = runtime;
            });
            Ok(())
        })
        .await?
    }

    pub(crate) async fn set_badge(&self, badge: Badge) -> Result<(), LauncherError> {
        let label = match badge {
            Badge::Clear | Badge::Count(0) => None,
            Badge::Count(n) => Some(n.to_string()),
            Badge::Label(text) => Some(text).filter(|t| !t.is_empty()),
        };
        on_main(move |mtm| {
            let tile = NSApplication::sharedApplication(mtm).dockTile();
            tile.setBadgeLabel(label.map(|l| NSString::from_str(&l)).as_deref());
        })
        .await
    }

    #[expect(clippy::unused_async, reason = "same shape as the other platforms")]
    pub(crate) async fn set_progress(&self, _: Progress) -> Result<(), LauncherError> {
        Err(LauncherError::Unsupported)
    }

    pub(crate) async fn request_attention(
        &self,
        attention: Attention,
    ) -> Result<(), LauncherError> {
        on_main(move |mtm| {
            let app = NSApplication::sharedApplication(mtm);
            DOCK.with(|dock| {
                let mut dock = dock.borrow_mut();
                if let Some(pending) = dock.attention.take() {
                    app.cancelUserAttentionRequest(pending);
                }
                let kind = match attention {
                    Attention::Informational => NSRequestUserAttentionType::InformationalRequest,
                    Attention::Critical => NSRequestUserAttentionType::CriticalRequest,
                    Attention::Cancel => return,
                };
                // 0 = the app is active and nothing was requested.
                let request = app.requestUserAttention(kind);
                dock.attention = (request != 0).then_some(request);
            });
        })
        .await
    }

    pub(crate) async fn add_recent_document(&self, path: String) -> Result<(), LauncherError> {
        on_main(move |mtm| {
            let url = NSURL::fileURLWithPath(&NSString::from_str(&path));
            NSDocumentController::sharedDocumentController(mtm).noteNewRecentDocumentURL(&url);
        })
        .await
    }

    pub(crate) async fn clear_recent_documents(&self) -> Result<(), LauncherError> {
        on_main(|mtm| {
            let controller = NSDocumentController::sharedDocumentController(mtm);
            // SAFETY: `clearRecentDocuments:` takes any sender, nil included.
            unsafe { controller.clearRecentDocuments(None) };
        })
        .await
    }
}

/// Add `applicationDockMenu:` to the delegate's class, once.
fn install_dock_menu(mtm: MainThreadMarker) -> Result<(), LauncherError> {
    if DOCK.with(|dock| dock.borrow().installed) {
        return Ok(());
    }
    let app = NSApplication::sharedApplication(mtm);
    let Some(delegate) = app.delegate() else {
        tracing::warn!(
            target: "istmo.launcher_actions",
            "no NSApplication delegate yet: set the Dock menu after the event loop starts"
        );
        return Err(LauncherError::Unsupported);
    };
    let object: &AnyObject = delegate.as_ref();
    let class: &AnyClass = object.class();
    let selector = sel!(applicationDockMenu:);
    if class.instance_method(selector).is_some() {
        tracing::warn!(
            target: "istmo.launcher_actions",
            class = %class.name().to_string_lossy(),
            "the app delegate has its own Dock menu; launcher actions not added"
        );
        return Err(LauncherError::Unsupported);
    }
    // SAFETY: `dock_menu` has the signature `DOCK_MENU_TYPES` describes
    // (object, selector, object) -> object; transmuting it to the untyped
    // `Imp` is how the runtime stores every method.
    let imp: Imp = unsafe {
        std::mem::transmute::<
            unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject) -> *mut NSMenu,
            Imp,
        >(dock_menu)
    };
    // SAFETY: a live class, a selector it lacks, and a matching encoding.
    let added = unsafe {
        objc2::ffi::class_addMethod(
            std::ptr::from_ref(class).cast_mut(),
            selector,
            imp,
            DOCK_MENU_TYPES.as_ptr(),
        )
    };
    if !added.as_bool() {
        return Err(LauncherError::Backend(
            "class_addMethod(applicationDockMenu:) failed".into(),
        ));
    }
    DOCK.with(|dock| dock.borrow_mut().installed = true);
    Ok(())
}

/// `- (NSMenu *)applicationDockMenu:(NSApplication *)sender`, added to the
/// delegate's class. `AppKit` calls it on the main thread.
unsafe extern "C-unwind" fn dock_menu(
    _this: *mut AnyObject,
    _cmd: Sel,
    _sender: *mut AnyObject,
) -> *mut NSMenu {
    let Some(mtm) = MainThreadMarker::new() else {
        return std::ptr::null_mut();
    };
    DOCK.with(|dock| dock.borrow_mut().menu(mtm))
        .map_or(std::ptr::null_mut(), Retained::autorelease_return)
}

impl Dock {
    /// The Dock menu for the current actions, or `nil` without any.
    fn menu(&mut self, mtm: MainThreadMarker) -> Option<Retained<NSMenu>> {
        if self.actions.is_empty() {
            return None;
        }
        let target = self
            .target
            .get_or_insert_with(|| MenuTarget::new(mtm))
            .clone();
        let menu = NSMenu::new(mtm);
        for (index, action) in self.actions.iter().enumerate() {
            let title = NSString::from_str(&action.title);
            let key = NSString::from_str("");
            // SAFETY: `activate:` is implemented by `MenuTarget`, the item's target.
            let item = unsafe {
                NSMenuItem::initWithTitle_action_keyEquivalent(
                    NSMenuItem::alloc(mtm),
                    &title,
                    Some(sel!(activate:)),
                    &key,
                )
            };
            // SAFETY: the target outlives every menu (kept in `Dock`).
            unsafe { item.setTarget(Some(&target)) };
            item.setTag(NSInteger::try_from(index).unwrap_or(NSInteger::MAX));
            if let Some(subtitle) = &action.subtitle {
                item.setToolTip(Some(&NSString::from_str(subtitle)));
            }
            if let Some(image) = action.icon.as_ref().and_then(image) {
                item.setImage(Some(&image));
            }
            menu.addItem(&item);
        }
        Some(menu)
    }

    /// Publish the deep link of the action at `index`.
    fn activate(&self, index: usize) {
        let Some(action) = self.actions.get(index) else {
            return;
        };
        let Some(runtime) = self.runtime.as_ref().and_then(Weak::upgrade) else {
            tracing::warn!(target: "istmo.launcher_actions", "Dock menu action without a runtime");
            return;
        };
        let link = DeepLink {
            uri: action.url.clone(),
            source: Some(action_source(&action.id)),
            received_at_ms: now_ms(),
        };
        if let Err(err) = DeepLinks::publish(&runtime, &link) {
            tracing::warn!(target: "istmo.launcher_actions", %err, "Dock menu action not published");
        }
    }
}

fn now_ms() -> Option<u64> {
    let since = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    u64::try_from(since.as_millis()).ok()
}

fn image(icon: &ActionIcon) -> Option<Retained<NSImage>> {
    let image = match icon {
        ActionIcon::Symbol(name) => NSImage::imageWithSystemSymbolName_accessibilityDescription(
            &NSString::from_str(name),
            None,
        )?,
        ActionIcon::Png(png) => NSImage::initWithData(NSImage::alloc(), &NSData::with_bytes(png))?,
    };
    image.setSize(objc2_foundation::NSSize::new(ICON_EDGE, ICON_EDGE));
    Some(image)
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and the class
    // implements no `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "IstmoLauncherActionsMenuTarget"]
    struct MenuTarget;

    unsafe impl NSObjectProtocol for MenuTarget {}

    impl MenuTarget {
        #[unsafe(method(activate:))]
        fn activate(&self, sender: &NSMenuItem) {
            let index = usize::try_from(sender.tag()).unwrap_or(usize::MAX);
            DOCK.with(|dock| dock.borrow().activate(index));
        }
    }
);

impl MenuTarget {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: NSObject's `init` on a freshly allocated object.
        unsafe { msg_send![super(this), init] }
    }
}
