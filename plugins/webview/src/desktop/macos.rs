//! `WKWebView` engine for macOS.
//!
//! Everything runs on the main thread (`AppKit`'s rule); work is posted to
//! the main dispatch queue, which the app's event loop (winit, or any
//! `NSApplication` run loop) drains. Delegates receive the view id and
//! look the view up, so a late callback never touches freed state.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use block2::{DynBlock, RcBlock};
use dispatch2::DispatchQueue;
use istmo_core::NativeHandleId;
use istmo_core::sync::lock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, ProtocolObject};
use objc2::{AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{NSBackingStoreType, NSView, NSWindow, NSWindowDelegate, NSWindowStyleMask};
use objc2_foundation::{
    NSArray, NSData, NSDate, NSDictionary, NSError, NSHTTPURLResponse, NSNumber, NSObject,
    NSObjectProtocol, NSPoint, NSRect, NSSet, NSSize, NSString, NSURL, NSUUID,
};
use objc2_web_kit::{
    WKContentWorld, WKFrameInfo, WKNavigation, WKNavigationAction, WKNavigationActionPolicy,
    WKNavigationDelegate, WKScriptMessage, WKScriptMessageHandlerWithReply, WKUIDelegate,
    WKURLSchemeHandler, WKURLSchemeTask, WKUserContentController, WKUserScript,
    WKUserScriptInjectionTime, WKWebView, WKWebViewConfiguration, WKWebsiteDataStore,
    WKWindowFeatures,
};

use super::core::{Done, Engine, FrameSlot, SchemeResponder, UiTask, UiView, ViewCore, ViewTask};
use crate::present::FrameSink;
use crate::{
    ClearDataRequest, ContextMenuMode, CookieFilter, DataKind, DecisionKind, DecisionResponse,
    DialogAsk, DialogKind, FindQuery, FindResult, HistoryEntry, HistoryNav, InjectionTime,
    LoadRequest, NavErrorKind, NewViewInfo, RenderTarget, ReplyResult, SameSite, SchemeBody,
    SchemeRequestInfo, SchemeResponseHead, SchemeSource, ScriptCall, ScriptWorld, SnapshotOptions,
    TerminationInfo, TerminationReason, ViewFrame, WebCookie, WebHistory, WebProfile, WebSnapshot,
    WebViewCapabilities, WebViewError,
};

const NATIVE: &str = "__istmoNative";

/// Makes main-thread-only values movable into tasks that only ever run
/// on the main thread.
struct MainOnly<T>(T);

// SAFETY: every `MainOnly` is created on the main thread and consumed by
// a task posted to the main queue, so `T` never crosses threads in use.
#[expect(
    clippy::non_send_fields_in_send_ty,
    reason = "the wrapper exists to move main-thread values into main-queue tasks"
)]
unsafe impl<T> Send for MainOnly<T> {}

impl<T> MainOnly<T> {
    const fn get(&self) -> &T {
        &self.0
    }

    fn into_inner(self) -> T {
        self.0
    }
}

fn mtm() -> MainThreadMarker {
    MainThreadMarker::new().expect("invariant: engine work runs on the main queue")
}

fn ns(s: &str) -> Retained<NSString> {
    NSString::from_str(s)
}

// =====================================================================
// Engine
// =====================================================================

pub(crate) struct MacEngine {
    frames: Mutex<HashMap<NativeHandleId, Arc<FrameState>>>,
    me: Weak<Self>,
}

struct FrameState {
    slot: FrameSlot,
    scheduled: AtomicBool,
}

struct MacFrameSink {
    id: NativeHandleId,
    state: Arc<FrameState>,
    engine: Weak<MacEngine>,
}

impl FrameSink for MacFrameSink {
    fn set_frame(&self, frame: ViewFrame) {
        if !self.state.slot.store(frame) || self.state.scheduled.swap(true, Ordering::AcqRel) {
            return;
        }
        let Some(engine) = self.engine.upgrade() else {
            return;
        };
        let (id, state) = (self.id, Arc::clone(&self.state));
        engine.post(Box::new(move || {
            state.scheduled.store(false, Ordering::Release);
            if let Some(frame) = state.slot.take() {
                with(id, |view| view.apply_frame(frame));
            }
        }));
    }
}

static ENGINE: OnceLock<Weak<MacEngine>> = OnceLock::new();

impl MacEngine {
    pub(crate) fn start() -> Arc<Self> {
        let engine = Arc::new_cyclic(|me| Self {
            frames: Mutex::default(),
            me: me.clone(),
        });
        ENGINE.get_or_init(|| Arc::downgrade(&engine));
        engine
    }
}

struct Ui {
    views: HashMap<NativeHandleId, MacView>,
    cores: HashMap<NativeHandleId, Arc<ViewCore>>,
    stores: HashMap<String, Retained<WKWebsiteDataStore>>,
    scheme_handler: Option<Retained<SchemeHandler>>,
}

thread_local! {
    static UI: RefCell<Ui> = RefCell::new(Ui {
        views: HashMap::new(),
        cores: HashMap::new(),
        stores: HashMap::new(),
        scheme_handler: None,
    });
}

fn with<R>(id: NativeHandleId, f: impl FnOnce(&mut MacView) -> R) -> Option<R> {
    UI.with(|ui| ui.try_borrow_mut().ok()?.views.get_mut(&id).map(f))
}

fn core_of(id: NativeHandleId) -> Option<Arc<ViewCore>> {
    UI.with(|ui| ui.try_borrow().ok()?.cores.get(&id).cloned())
}

fn id_of(view: &WKWebView) -> Option<NativeHandleId> {
    let ptr = std::ptr::from_ref(view);
    UI.with(|ui| {
        ui.try_borrow()
            .ok()?
            .views
            .iter()
            .find_map(|(id, v)| std::ptr::eq(Retained::as_ptr(&v.web_view), ptr).then_some(*id))
    })
}

impl Engine for MacEngine {
    fn describe(&self) -> (String, String) {
        ("wkwebview".into(), String::new())
    }

    fn capabilities(&self, caps: &mut WebViewCapabilities) {
        caps.embedded = true;
        caps.windowed = true;
        caps.window_parenting = false;
        caps.ephemeral_profiles = true;
        caps.persistent_profiles = true;
        caps.isolated_world = true;
        caps.bridge_reply = true;
        caps.deferred_navigation = true;
        caps.scheme_request_bodies = false;
    }

    fn post(&self, task: UiTask) {
        DispatchQueue::main().exec_async(task);
    }

    fn create(&self, core: Arc<ViewCore>, done: Done<()>) {
        self.post(Box::new(move || done(create(core))));
    }

    fn with_view(&self, id: NativeHandleId, f: ViewTask) {
        self.post(Box::new(move || {
            let taken = UI.with(|ui| ui.borrow_mut().views.remove(&id));
            match taken {
                Some(mut view) => {
                    f(Some(&mut view));
                    if view.closed {
                        view.destroy();
                        UI.with(|ui| ui.borrow_mut().cores.remove(&id));
                    } else {
                        UI.with(|ui| ui.borrow_mut().views.insert(id, view));
                    }
                }
                None => f(None),
            }
        }));
    }

    fn frame_sink(&self, id: NativeHandleId) -> Arc<dyn FrameSink> {
        let state = Arc::clone(lock(&self.frames).entry(id).or_insert_with(|| {
            Arc::new(FrameState {
                slot: FrameSlot::default(),
                scheduled: AtomicBool::new(false),
            })
        }));
        Arc::new(MacFrameSink {
            id,
            state,
            engine: self.me.clone(),
        })
    }
}

// =====================================================================
// Creation
// =====================================================================

/// Same UUID for the same profile name across launches.
fn profile_uuid(name: &str) -> Retained<NSUUID> {
    // FNV-1a over two seeds: stable, no extra dependency.
    let hash = |seed: u64| {
        let mut h = seed;
        for b in format!("istmo.webview.{name}").bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
        h
    };
    let mut bytes = [0_u8; 16];
    bytes[..8].copy_from_slice(&hash(0xcbf2_9ce4_8422_2325).to_be_bytes());
    bytes[8..].copy_from_slice(&hash(0x8422_2325_cbf2_9ce4).to_be_bytes());
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    NSUUID::from_bytes(bytes)
}

fn data_store(
    profile: &WebProfile,
    mtm: MainThreadMarker,
) -> Result<Retained<WKWebsiteDataStore>, WebViewError> {
    // SAFETY: plain WebKit class methods on the main thread.
    unsafe {
        Ok(match profile {
            WebProfile::Shared => WKWebsiteDataStore::defaultDataStore(mtm),
            WebProfile::Ephemeral => WKWebsiteDataStore::nonPersistentDataStore(mtm),
            WebProfile::Persistent(p) => {
                if p.name.is_empty()
                    || p.name.len() > 64
                    || !p
                        .name
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
                {
                    return Err(WebViewError::InvalidArgument("profile name".into()));
                }
                if let Some(store) = UI.with(|ui| ui.borrow().stores.get(&p.name).cloned()) {
                    return Ok(store);
                }
                // macOS 14: identified persistent stores.
                let store = WKWebsiteDataStore::dataStoreForIdentifier(&profile_uuid(&p.name), mtm);
                UI.with(|ui| ui.borrow_mut().stores.insert(p.name.clone(), store.clone()));
                store
            }
        })
    }
}

fn shim(origins: &[String], subframes: bool) -> String {
    let list = origins
        .iter()
        .map(|o| format!("\"{}\"", super::text::json_escape(o)))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        r#"(() => {{
  const handler = window.webkit && window.webkit.messageHandlers && window.webkit.messageHandlers.{NATIVE};
  if (!handler || window.istmo || ![{list}].includes(location.origin){top}) return;
  const listeners = new Set();
  const toB64 = (d) => {{
    const bytes = typeof d === "string" ? new TextEncoder().encode(d) : d instanceof Uint8Array ? d : new Uint8Array(d);
    let bin = "";
    for (let i = 0; i < bytes.length; i += 0x8000) bin += String.fromCharCode.apply(null, bytes.subarray(i, i + 0x8000));
    return btoa(bin);
  }};
  const fromB64 = (b) => Uint8Array.from(atob(b), (c) => c.charCodeAt(0));
  Object.defineProperty(window, "__istmoDeliver", {{
    value: (channel, b) => listeners.forEach((f) => {{ try {{ f({{ channel, data: fromB64(b) }}); }} catch (e) {{ console.error(e); }} }}),
    configurable: false, writable: false, enumerable: false,
  }});
  Object.defineProperty(window, "istmo", {{
    value: Object.freeze({{
      postMessage(channel, data) {{ handler.postMessage({{ t: "m", c: String(channel), b: toB64(data ?? "") }}); }},
      invoke(channel, data) {{ return handler.postMessage({{ t: "i", c: String(channel), b: toB64(data ?? "") }}).then(fromB64); }},
      addEventListener(type, f) {{ if (type === "message") listeners.add(f); }},
      removeEventListener(type, f) {{ if (type === "message") listeners.delete(f); }},
    }}),
    configurable: false, writable: false,
  }});
}})();"#,
        top = if subframes {
            ""
        } else {
            " || window !== window.top"
        },
    )
}

fn configuration(
    core: &ViewCore,
    mtm: MainThreadMarker,
) -> Result<Retained<WKWebViewConfiguration>, WebViewError> {
    // SAFETY: WebKit configuration setters on the main thread.
    unsafe {
        let config = WKWebViewConfiguration::new(mtm);
        config.setWebsiteDataStore(&*data_store(&core.spec.profile, mtm)?);
        if let Some(suffix) = &core.config.user_agent_suffix {
            config.setApplicationNameForUserAgent(Some(&ns(suffix)));
        }
        let handler = UI.with(|ui| {
            ui.borrow_mut()
                .scheme_handler
                .get_or_insert_with(|| SchemeHandler::new(mtm))
                .clone()
        });
        for scheme in &core.config.schemes {
            config.setURLSchemeHandler_forURLScheme(
                Some(ProtocolObject::from_ref(&*handler)),
                &ns(&scheme.name),
            );
        }
        let content = config.userContentController();
        for script in &core.spec.user_scripts {
            let world = if script.world == ScriptWorld::Isolated {
                WKContentWorld::defaultClientWorld(mtm)
            } else {
                WKContentWorld::pageWorld(mtm)
            };
            let source = if script.allowed_origins.is_empty() {
                script.source.clone()
            } else {
                super::text::origin_guard(&script.allowed_origins, &script.source)
            };
            let time = if script.inject_at == InjectionTime::DocumentStart {
                WKUserScriptInjectionTime::AtDocumentStart
            } else {
                WKUserScriptInjectionTime::AtDocumentEnd
            };
            let user_script =
                WKUserScript::initWithSource_injectionTime_forMainFrameOnly_inContentWorld(
                    WKUserScript::alloc(mtm),
                    &ns(&source),
                    time,
                    script.main_frame_only,
                    &world,
                );
            content.addUserScript(&user_script);
        }
        if let Some(bridge) = &core.spec.bridge
            && !core.bridge_origins().is_empty()
        {
            let handler = BridgeHandler::new(mtm, core.id);
            content.addScriptMessageHandlerWithReply_contentWorld_name(
                ProtocolObject::from_ref(&*handler),
                &WKContentWorld::pageWorld(mtm),
                &ns(NATIVE),
            );
            let shim = WKUserScript::initWithSource_injectionTime_forMainFrameOnly_inContentWorld(
                WKUserScript::alloc(mtm),
                &ns(&shim(core.bridge_origins(), bridge.allow_subframes)),
                WKUserScriptInjectionTime::AtDocumentStart,
                !bridge.allow_subframes,
                &WKContentWorld::pageWorld(mtm),
            );
            content.addUserScript(&shim);
        }
        Ok(config)
    }
}

fn create(core: Arc<ViewCore>) -> Result<(), WebViewError> {
    let mtm = mtm();
    let config = configuration(&core, mtm)?;
    create_with(core, &config, mtm).map(drop)
}

fn create_with(
    core: Arc<ViewCore>,
    config: &WKWebViewConfiguration,
    mtm: MainThreadMarker,
) -> Result<Retained<WKWebView>, WebViewError> {
    let id = core.id;
    // SAFETY: AppKit / WebKit objects created and wired on the main thread.
    unsafe {
        let web_view =
            WKWebView::initWithFrame_configuration(WKWebView::alloc(mtm), NSRect::ZERO, config);
        let delegate = Delegate::new(mtm, id);
        web_view.setNavigationDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        web_view.setUIDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        let s = &core.spec.settings;
        if let Some(ua) = &s.user_agent {
            web_view.setCustomUserAgent(Some(&ns(ua)));
        }
        web_view.setAllowsMagnification(s.zoom_gestures);
        if core.config.allow_inspector {
            web_view.setInspectable(true);
        }
        if s.transparent {
            let _: () = msg_send![&web_view, setValue: &*NSNumber::new_bool(false), forKey: &*ns("drawsBackground")];
        }
        let mut window = None;
        match &core.spec.target {
            RenderTarget::Embedded(target) => {
                let parent = parent_view(target.window_id)?;
                web_view.setHidden(true);
                parent.addSubview(&web_view);
            }
            RenderTarget::Window(target) => {
                let w = if target.width == 0 {
                    800.0
                } else {
                    f64::from(target.width)
                };
                let h = if target.height == 0 {
                    600.0
                } else {
                    f64::from(target.height)
                };
                let mut style = NSWindowStyleMask::Titled
                    | NSWindowStyleMask::Closable
                    | NSWindowStyleMask::Miniaturizable;
                if target.resizable {
                    style |= NSWindowStyleMask::Resizable;
                }
                let win = NSWindow::initWithContentRect_styleMask_backing_defer(
                    NSWindow::alloc(mtm),
                    NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h)),
                    style,
                    NSBackingStoreType::Buffered,
                    false,
                );
                win.setReleasedWhenClosed(false);
                win.setTitle(&ns(&target.title));
                win.setContentView(Some(&web_view));
                win.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
                win.center();
                win.makeKeyAndOrderFront(None);
                window = Some(win);
            }
        }
        if core.spec.settings.context_menu == ContextMenuMode::Hidden {
            // Suppress the default menu: an empty menu is never shown.
            web_view.setMenu(None);
        }
        UI.with(|ui| {
            let mut ui = ui.borrow_mut();
            ui.cores.insert(id, Arc::clone(&core));
            ui.views.insert(
                id,
                MacView {
                    core,
                    web_view: web_view.clone(),
                    window,
                    _delegate: delegate,
                    closed: false,
                },
            );
        });
        Ok(web_view)
    }
}

fn parent_view(window_id: u64) -> Result<Retained<NSView>, WebViewError> {
    let native = istmo_window::WindowRegistry::global()
        .get(istmo_window::WindowId(window_id))
        .ok_or_else(|| {
            WebViewError::InvalidArgument(format!("window {window_id} is not registered"))
        })?;
    let istmo_window::NativeWindow::AppKit { ns_view } = native else {
        return Err(WebViewError::RenderModeUnsupported);
    };
    let ptr = ns_view.get() as *mut NSView;
    // SAFETY: the app keeps the registered view alive until it
    // unregisters it; retaining extends that while we use it.
    unsafe { Retained::retain(ptr) }.ok_or(WebViewError::RenderModeUnsupported)
}

// =====================================================================
// View
// =====================================================================

struct MacView {
    core: Arc<ViewCore>,
    web_view: Retained<WKWebView>,
    window: Option<Retained<NSWindow>>,
    /// Kept alive here: `WebKit` and `AppKit` hold delegates weakly.
    _delegate: Retained<Delegate>,
    closed: bool,
}

impl MacView {
    fn apply_frame(&mut self, frame: ViewFrame) {
        // SAFETY: AppKit geometry on the main thread.
        unsafe {
            let Some(parent) = self.web_view.superview() else {
                return;
            };
            if !frame.visible || frame.width == 0 || frame.height == 0 {
                self.web_view.setHidden(true);
                return;
            }
            let scale = parent.window().map_or(1.0, |w| w.backingScaleFactor());
            let (w, h) = (
                f64::from(frame.width) / scale,
                f64::from(frame.height) / scale,
            );
            let x = f64::from(frame.x) / scale;
            let mut y = f64::from(frame.y) / scale;
            if !parent.isFlipped() {
                y = parent.bounds().size.height - y - h;
            }
            self.web_view
                .setFrame(NSRect::new(NSPoint::new(x, y), NSSize::new(w, h)));
            self.web_view.setHidden(false);
        }
    }

    fn destroy(&mut self) {
        // SAFETY: tear-down on the main thread.
        unsafe {
            self.web_view.stopLoading();
            self.web_view.setNavigationDelegate(None);
            self.web_view.setUIDelegate(None);
            self.web_view
                .configuration()
                .userContentController()
                .removeAllScriptMessageHandlers();
            if let Some(window) = self.window.take() {
                window.setDelegate(None);
                window.close();
            } else {
                self.web_view.removeFromSuperview();
            }
        }
    }

    fn js(&self, source: &str, world: ScriptWorld, done: Option<Done<String>>) {
        let mtm = mtm();
        let world = if world == ScriptWorld::Isolated {
            // SAFETY: main thread.
            unsafe { WKContentWorld::defaultClientWorld(mtm) }
        } else {
            // SAFETY: main thread.
            unsafe { WKContentWorld::pageWorld(mtm) }
        };
        let done = MainOnly(RefCell::new(done));
        let block = RcBlock::new(move |value: *mut AnyObject, error: *mut NSError| {
            let Some(done) = done.get().borrow_mut().take() else {
                return;
            };
            if !error.is_null() {
                // SAFETY: WebKit passes a live error.
                let message = unsafe { (*error).localizedDescription() }.to_string();
                return done(Err(WebViewError::Backend(message)));
            }
            done(Ok(json_of(value)));
        });
        // SAFETY: main thread; the block is copied by WebKit.
        unsafe {
            self.web_view
                .callAsyncJavaScript_arguments_inFrame_inContentWorld_completionHandler(
                    &ns(source),
                    None,
                    None,
                    &world,
                    Some(&block),
                );
        }
    }
}

/// JSON of a value `callAsyncJavaScript` returned (strings, numbers,
/// booleans, null; containers fall back to their description).
fn json_of(value: *mut AnyObject) -> String {
    if value.is_null() {
        return "null".into();
    }
    // SAFETY: WebKit returns a live Foundation object.
    let object = unsafe { &*value };
    if let Some(s) = object.downcast_ref::<NSString>() {
        return format!("\"{}\"", super::text::json_escape(&s.to_string()));
    }
    if let Some(n) = object.downcast_ref::<NSNumber>() {
        return n.stringValue().to_string();
    }
    // SAFETY: every Foundation object answers `description`.
    let description: Retained<NSString> = unsafe { msg_send![object, description] };
    format!("\"{}\"", super::text::json_escape(&description.to_string()))
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "cookie expiry dates are far inside the i64 millisecond range"
)]
fn unix_ms(seconds: f64) -> i64 {
    (seconds * 1000.0) as i64
}

fn base64(data: &[u8]) -> String {
    NSData::with_bytes(data)
        .base64EncodedStringWithOptions(objc2_foundation::NSDataBase64EncodingOptions(0))
        .to_string()
}

impl UiView for MacView {
    fn load(&mut self, request: &LoadRequest) {
        let Some(url) = NSURL::URLWithString(&ns(&request.url)) else {
            return self
                .core
                .failed(&request.url, NavErrorKind::BadUrl, "invalid URL");
        };
        let req = objc2_foundation::NSMutableURLRequest::requestWithURL(&url);
        for h in &request.headers {
            req.setValue_forHTTPHeaderField(Some(&ns(&h.value)), &ns(&h.name));
        }
        if let Some(lang) = &self.core.spec.settings.accept_language {
            req.setValue_forHTTPHeaderField(Some(&ns(lang)), &ns("Accept-Language"));
        }
        if let Some(body) = &request.post_body {
            req.setHTTPMethod(&ns("POST"));
            req.setHTTPBody(Some(&NSData::with_bytes(body)));
        }
        // SAFETY: main thread.
        unsafe { self.web_view.loadRequest(&req) };
    }

    fn load_html(&mut self, html: &str, base_url: Option<&str>) {
        let base = base_url.and_then(|b| NSURL::URLWithString(&ns(b)));
        // SAFETY: main thread.
        unsafe {
            self.web_view
                .loadHTMLString_baseURL(&ns(html), base.as_deref())
        };
    }

    fn navigate(&mut self, nav: HistoryNav) {
        // SAFETY: main thread.
        unsafe {
            match nav {
                HistoryNav::Back => drop(self.web_view.goBack()),
                HistoryNav::Forward => drop(self.web_view.goForward()),
                HistoryNav::Reload => drop(self.web_view.reload()),
                HistoryNav::ReloadBypassCache => drop(self.web_view.reloadFromOrigin()),
                HistoryNav::Stop => self.web_view.stopLoading(),
                HistoryNav::Offset(n) => {
                    if let Some(item) = self.web_view.backForwardList().itemAtIndex(n as isize) {
                        drop(self.web_view.goToBackForwardListItem(&item));
                    }
                }
            }
        }
    }

    fn history(&mut self) -> WebHistory {
        // SAFETY: main thread.
        unsafe {
            let list = self.web_view.backForwardList();
            let entry = |item: &objc2_web_kit::WKBackForwardListItem| HistoryEntry {
                url: item
                    .URL()
                    .absoluteString()
                    .map(|s| s.to_string())
                    .unwrap_or_default(),
                title: item.title().map(|s| s.to_string()).unwrap_or_default(),
            };
            let back = list.backList();
            let mut entries: Vec<HistoryEntry> = back.iter().map(|i| entry(&i)).collect();
            let current = u32::try_from(entries.len()).unwrap_or(u32::MAX);
            if let Some(cur) = list.currentItem() {
                entries.push(entry(&cur));
            }
            entries.extend(list.forwardList().iter().map(|i| entry(&i)));
            WebHistory { entries, current }
        }
    }

    fn focus(&mut self) {
        if let Some(window) = self.web_view.window() {
            window.makeFirstResponder(Some(&self.web_view));
        }
    }

    fn eval(&mut self, call: &ScriptCall, done: Done<String>) {
        let body = format!(
            "return await eval(\"{}\");",
            super::text::json_escape(&call.source)
        );
        self.js(&body, call.world, Some(done));
    }

    fn deliver(&mut self, channel: &str, data: &[u8]) {
        let body = format!(
            "window.__istmoDeliver && window.__istmoDeliver(\"{}\", \"{}\");",
            super::text::json_escape(channel),
            base64(data)
        );
        self.js(&body, ScriptWorld::Page, None);
    }

    fn cookies(&mut self, filter: &CookieFilter, done: Done<Vec<WebCookie>>) {
        let host = filter
            .url
            .as_deref()
            .and_then(|u| url::Url::parse(u).ok())
            .and_then(|u| u.host_str().map(str::to_ascii_lowercase));
        let name = filter.name.clone();
        let done = MainOnly(RefCell::new(Some(done)));
        let block = RcBlock::new(
            move |cookies: NonNull<NSArray<objc2_foundation::NSHTTPCookie>>| {
                let Some(done) = done.get().borrow_mut().take() else {
                    return;
                };
                // SAFETY: WebKit passes a live array.
                let cookies = unsafe { cookies.as_ref() };
                let out = cookies
                    .iter()
                    .filter_map(|c| {
                        let cookie = WebCookie {
                            name: c.name().to_string(),
                            value: c.value().to_string(),
                            domain: c.domain().to_string(),
                            path: c.path().to_string(),
                            expires_unix_ms: c
                                .expiresDate()
                                .map(|d| unix_ms(d.timeIntervalSince1970())),
                            secure: c.isSecure(),
                            http_only: c.isHTTPOnly(),
                            same_site: SameSite::Lax,
                        };
                        let domain = cookie.domain.trim_start_matches('.').to_ascii_lowercase();
                        let host_ok = host
                            .as_ref()
                            .is_none_or(|h| *h == domain || h.ends_with(&format!(".{domain}")));
                        let name_ok = name.as_ref().is_none_or(|n| *n == cookie.name);
                        (host_ok && name_ok).then_some(cookie)
                    })
                    .collect();
                done(Ok(out));
            },
        );
        // SAFETY: main thread; WebKit copies the block.
        unsafe {
            self.web_view
                .configuration()
                .websiteDataStore()
                .httpCookieStore()
                .getAllCookies(&block);
        };
    }

    fn set_cookie(&mut self, cookie: &WebCookie, done: Done<()>) {
        let mut props: Vec<(&str, Retained<AnyObject>)> = vec![
            (
                "Name",
                Retained::into_super(Retained::into_super(ns(&cookie.name))),
            ),
            (
                "Value",
                Retained::into_super(Retained::into_super(ns(&cookie.value))),
            ),
            (
                "Domain",
                Retained::into_super(Retained::into_super(ns(&cookie.domain))),
            ),
            (
                "Path",
                Retained::into_super(Retained::into_super(ns(if cookie.path.is_empty() {
                    "/"
                } else {
                    &cookie.path
                }))),
            ),
        ];
        if cookie.secure {
            props.push((
                "Secure",
                Retained::into_super(Retained::into_super(ns("TRUE"))),
            ));
        }
        if let Some(ms) = cookie.expires_unix_ms {
            #[expect(
                clippy::cast_precision_loss,
                reason = "millisecond timestamps fit a double"
            )]
            let date = NSDate::dateWithTimeIntervalSince1970(ms as f64 / 1000.0);
            props.push(("Expires", Retained::into_super(Retained::into_super(date))));
        }
        let keys: Vec<Retained<NSString>> = props.iter().map(|(k, _)| ns(k)).collect();
        let key_refs: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
        let values: Vec<&AnyObject> = props.iter().map(|(_, v)| &**v).collect();
        let dict = NSDictionary::from_slices(&key_refs, &values);
        // SAFETY: `NSHTTPCookie` property keys are these plain strings.
        let http: Option<Retained<objc2_foundation::NSHTTPCookie>> =
            unsafe { msg_send![objc2::class!(NSHTTPCookie), cookieWithProperties: &*dict] };
        let Some(http) = http else {
            return done(Err(WebViewError::InvalidArgument("cookie".into())));
        };
        let done = MainOnly(RefCell::new(Some(done)));
        let block = RcBlock::new(move || {
            if let Some(done) = done.get().borrow_mut().take() {
                done(Ok(()));
            }
        });
        // SAFETY: main thread; WebKit copies the block.
        unsafe {
            self.web_view
                .configuration()
                .websiteDataStore()
                .httpCookieStore()
                .setCookie_completionHandler(&http, Some(&block));
        };
    }

    fn clear_data(&mut self, request: &ClearDataRequest, done: Done<()>) {
        let mtm = mtm();
        let mut types: Vec<&str> = Vec::new();
        for kind in &request.kinds {
            types.extend(match kind {
                DataKind::Cookies => &["WKWebsiteDataTypeCookies"][..],
                DataKind::Cache => {
                    &["WKWebsiteDataTypeDiskCache", "WKWebsiteDataTypeMemoryCache"][..]
                }
                DataKind::LocalStorage => &[
                    "WKWebsiteDataTypeLocalStorage",
                    "WKWebsiteDataTypeSessionStorage",
                ][..],
                DataKind::IndexedDb => &["WKWebsiteDataTypeIndexedDBDatabases"][..],
                DataKind::ServiceWorkers => &["WKWebsiteDataTypeServiceWorkerRegistrations"][..],
                DataKind::All => &[][..],
            });
        }
        // SAFETY: main thread.
        let set: Retained<NSSet<NSString>> = if request.kinds.contains(&DataKind::All) {
            unsafe { WKWebsiteDataStore::allWebsiteDataTypes(mtm) }
        } else {
            let names: Vec<Retained<NSString>> = types.iter().map(|t| ns(t)).collect();
            NSSet::from_retained_slice(&names)
        };
        #[expect(
            clippy::cast_precision_loss,
            reason = "millisecond timestamps fit a double"
        )]
        let since = request
            .since_unix_ms
            .map_or_else(NSDate::distantPast, |ms| {
                NSDate::dateWithTimeIntervalSince1970(ms as f64 / 1000.0)
            });
        let done = MainOnly(RefCell::new(Some(done)));
        let block = RcBlock::new(move || {
            if let Some(done) = done.get().borrow_mut().take() {
                done(Ok(()));
            }
        });
        // SAFETY: main thread; WebKit copies the block.
        unsafe {
            self.web_view
                .configuration()
                .websiteDataStore()
                .removeDataOfTypes_modifiedSince_completionHandler(&set, &since, &block);
        };
    }

    fn set_zoom(&mut self, factor: f32) {
        // SAFETY: main thread.
        unsafe { self.web_view.setPageZoom(f64::from(factor)) };
    }

    fn find(&mut self, _query: &FindQuery, done: Done<FindResult>) {
        done(Err(WebViewError::Unsupported("find on macOS".into())));
    }

    fn snapshot(&mut self, _options: SnapshotOptions, done: Done<WebSnapshot>) {
        done(Err(WebViewError::Unsupported("snapshots on macOS".into())));
    }

    fn print(&mut self) -> Result<(), WebViewError> {
        Err(WebViewError::Unsupported("printing on macOS".into()))
    }

    fn set_inspector(&mut self, open: bool) -> Result<(), WebViewError> {
        // SAFETY: main thread.
        unsafe { self.web_view.setInspectable(open) };
        Ok(())
    }

    fn close(&mut self) {
        self.closed = true;
    }
}

// =====================================================================
// Delegates
// =====================================================================

struct DelegateIvars {
    id: NativeHandleId,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; no `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "IstmoWebViewDelegate"]
    #[ivars = DelegateIvars]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl WKNavigationDelegate for Delegate {
        #[unsafe(method(webView:decidePolicyForNavigationAction:decisionHandler:))]
        fn decide(
            &self,
            _web_view: &WKWebView,
            action: &WKNavigationAction,
            handler: &DynBlock<dyn Fn(WKNavigationActionPolicy)>,
        ) {
            // SAFETY: WebKit hands live objects.
            let (main, url, gesture) = unsafe {
                let main = action.targetFrame().is_none_or(|f| f.isMainFrame());
                let url = action
                    .request()
                    .URL()
                    .and_then(|u| u.absoluteString())
                    .map(|s| s.to_string())
                    .unwrap_or_default();
                (
                    main,
                    url,
                    action.navigationType() == objc2_web_kit::WKNavigationType::LinkActivated,
                )
            };
            let Some(core) = core_of(self.ivars().id).filter(|_| main) else {
                return handler.call((WKNavigationActionPolicy::Allow,));
            };
            let held = MainOnly(handler.copy());
            core.navigation(
                &url,
                gesture,
                Box::new(move |allow| {
                    held.get().call((if allow {
                        WKNavigationActionPolicy::Allow
                    } else {
                        WKNavigationActionPolicy::Cancel
                    },));
                }),
            );
        }

        #[unsafe(method(webView:didStartProvisionalNavigation:))]
        fn started(&self, web_view: &WKWebView, _navigation: Option<&WKNavigation>) {
            if let Some(core) = core_of(self.ivars().id) {
                core.started(&url_of(web_view));
            }
        }

        #[unsafe(method(webView:didCommitNavigation:))]
        fn committed(&self, web_view: &WKWebView, _navigation: Option<&WKNavigation>) {
            if let Some(core) = core_of(self.ivars().id) {
                core.committed(&url_of(web_view));
            }
        }

        #[unsafe(method(webView:didFinishNavigation:))]
        fn finished(&self, web_view: &WKWebView, _navigation: Option<&WKNavigation>) {
            if let Some(core) = core_of(self.ivars().id) {
                // SAFETY: live view.
                let (back, forward) = unsafe { (web_view.canGoBack(), web_view.canGoForward()) };
                core.finished(&url_of(web_view), back, forward);
                // SAFETY: live view.
                core.title(
                    &unsafe { web_view.title() }
                        .map(|t| t.to_string())
                        .unwrap_or_default(),
                );
            }
        }

        #[unsafe(method(webView:didFailNavigation:withError:))]
        fn failed(
            &self,
            web_view: &WKWebView,
            _navigation: Option<&WKNavigation>,
            error: &NSError,
        ) {
            fail(self.ivars().id, web_view, error);
        }

        #[unsafe(method(webView:didFailProvisionalNavigation:withError:))]
        fn failed_provisional(
            &self,
            web_view: &WKWebView,
            _navigation: Option<&WKNavigation>,
            error: &NSError,
        ) {
            fail(self.ivars().id, web_view, error);
        }

        #[unsafe(method(webViewWebContentProcessDidTerminate:))]
        fn terminated(&self, _web_view: &WKWebView) {
            if let Some(core) = core_of(self.ivars().id) {
                core.fail_pending();
                core.emit(crate::ControlEvent::ProcessTerminated(TerminationInfo {
                    reason: TerminationReason::Crashed,
                }));
            }
        }
    }

    unsafe impl WKUIDelegate for Delegate {
        #[unsafe(method_id(webView:createWebViewWithConfiguration:forNavigationAction:windowFeatures:))]
        fn create_web_view(
            &self,
            _web_view: &WKWebView,
            config: &WKWebViewConfiguration,
            action: &WKNavigationAction,
            _features: &WKWindowFeatures,
        ) -> Option<Retained<WKWebView>> {
            popup(self.ivars().id, config, action)
        }
        #[unsafe(method(webViewDidClose:))]
        fn did_close(&self, _web_view: &WKWebView) {
            if let Some(core) = core_of(self.ivars().id) {
                core.emit(crate::ControlEvent::CloseRequested);
            }
        }

        #[unsafe(method(webView:runJavaScriptAlertPanelWithMessage:initiatedByFrame:completionHandler:))]
        fn alert(
            &self,
            _web_view: &WKWebView,
            message: &NSString,
            frame: &WKFrameInfo,
            handler: &DynBlock<dyn Fn()>,
        ) {
            let held = MainOnly(handler.copy());
            dialog(
                self.ivars().id,
                DialogKind::Alert,
                message,
                None,
                frame,
                Box::new(move |_| held.get().call(())),
            );
        }

        #[unsafe(method(webView:runJavaScriptConfirmPanelWithMessage:initiatedByFrame:completionHandler:))]
        fn confirm(
            &self,
            _web_view: &WKWebView,
            message: &NSString,
            frame: &WKFrameInfo,
            handler: &DynBlock<dyn Fn(Bool)>,
        ) {
            let held = MainOnly(handler.copy());
            dialog(
                self.ivars().id,
                DialogKind::Confirm,
                message,
                None,
                frame,
                Box::new(move |r| {
                    held.get()
                        .call((Bool::new(!matches!(r, DecisionResponse::Deny)),));
                }),
            );
        }

        #[unsafe(method(webView:runJavaScriptTextInputPanelWithPrompt:defaultText:initiatedByFrame:completionHandler:))]
        fn prompt(
            &self,
            _web_view: &WKWebView,
            prompt: &NSString,
            default: Option<&NSString>,
            frame: &WKFrameInfo,
            handler: &DynBlock<dyn Fn(*mut NSString)>,
        ) {
            let held = MainOnly(handler.copy());
            dialog(
                self.ivars().id,
                DialogKind::Prompt,
                prompt,
                default,
                frame,
                Box::new(move |r| match r {
                    DecisionResponse::DialogAccept(text) => {
                        let s = ns(text.as_deref().unwrap_or(""));
                        held.get().call((Retained::as_ptr(&s).cast_mut(),));
                    }
                    _ => held.get().call((std::ptr::null_mut(),)),
                }),
            );
        }
    }

    unsafe impl NSWindowDelegate for Delegate {
        #[unsafe(method(windowShouldClose:))]
        fn should_close(&self, _window: &NSWindow) -> bool {
            if let Some(core) = core_of(self.ivars().id) {
                core.emit(crate::ControlEvent::CloseRequested);
            }
            // The app closes the view.
            false
        }
    }
);

impl Delegate {
    fn new(mtm: MainThreadMarker, id: NativeHandleId) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(DelegateIvars { id });
        // SAFETY: NSObject's designated initialiser.
        unsafe { msg_send![super(this), init] }
    }
}

fn url_of(web_view: &WKWebView) -> String {
    // SAFETY: live view.
    unsafe { web_view.URL() }
        .and_then(|u| u.absoluteString())
        .map(|s| s.to_string())
        .unwrap_or_default()
}

fn fail(id: NativeHandleId, web_view: &WKWebView, error: &NSError) {
    let Some(core) = core_of(id) else { return };
    let kind = match error.code() {
        -1003 | -1006 => NavErrorKind::Dns,
        -1001 => NavErrorKind::Timeout,
        -1206..=-1200 => NavErrorKind::Tls,
        -999 | 102 => NavErrorKind::Cancelled,
        -1000 | -1002 => NavErrorKind::BadUrl,
        -1004 | -1005 | -1009 => NavErrorKind::Network,
        _ => NavErrorKind::Other,
    };
    core.failed(
        &url_of(web_view),
        kind,
        &error.localizedDescription().to_string(),
    );
}

fn dialog(
    id: NativeHandleId,
    kind: DialogKind,
    message: &NSString,
    default: Option<&NSString>,
    frame: &WKFrameInfo,
    answer: Box<dyn FnOnce(DecisionResponse)>,
) {
    let Some(core) = core_of(id) else {
        return answer(DecisionResponse::Deny);
    };
    // SAFETY: live frame info.
    let origin = unsafe {
        let o = frame.securityOrigin();
        format!("{}://{}", o.protocol(), o.host())
    };
    let answer = MainOnly(answer);
    core.ask(
        DecisionKind::Dialog(DialogAsk {
            kind,
            message: message.to_string(),
            default_text: default
                .map(std::string::ToString::to_string)
                .unwrap_or_default(),
            origin,
        }),
        Box::new(move |response| (answer.into_inner())(response)),
        None,
    );
}

// Bridge handler: one per view (it carries the id).

struct BridgeIvars {
    id: NativeHandleId,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; no `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "IstmoWebViewBridge"]
    #[ivars = BridgeIvars]
    struct BridgeHandler;

    unsafe impl NSObjectProtocol for BridgeHandler {}

    unsafe impl WKScriptMessageHandlerWithReply for BridgeHandler {
        #[unsafe(method(userContentController:didReceiveScriptMessage:replyHandler:))]
        fn received(
            &self,
            _controller: &WKUserContentController,
            message: &WKScriptMessage,
            reply: &DynBlock<dyn Fn(*mut AnyObject, *mut NSString)>,
        ) {
            let fail = |reply: &DynBlock<dyn Fn(*mut AnyObject, *mut NSString)>, text: &str| {
                let s = ns(text);
                reply.call((std::ptr::null_mut(), Retained::as_ptr(&s).cast_mut()));
            };
            let Some(core) = core_of(self.ivars().id) else {
                return fail(reply, "closed");
            };
            // SAFETY: live message and frame info.
            let (origin, main, body) = unsafe {
                let frame = message.frameInfo();
                let o = frame.securityOrigin();
                let port = o.port();
                let scheme = o.protocol().to_string();
                let default = match scheme.as_str() {
                    "http" => 80,
                    "https" => 443,
                    _ => 0,
                };
                let origin = if port == 0 || port == default {
                    format!("{scheme}://{}", o.host())
                } else {
                    format!("{scheme}://{}:{port}", o.host())
                };
                (origin, frame.isMainFrame(), message.body())
            };
            let Some(dict) = body.downcast_ref::<NSDictionary>() else {
                return fail(reply, "malformed");
            };
            let field = |k: &str| -> Option<String> {
                dict.objectForKey(&ns(k)).and_then(|v| {
                    v.downcast_ref::<NSString>()
                        .map(std::string::ToString::to_string)
                })
            };
            let (Some(kind), Some(channel), Some(b64)) = (field("t"), field("c"), field("b"))
            else {
                return fail(reply, "malformed");
            };
            let Some(data) = NSData::initWithBase64EncodedString_options(
                NSData::alloc(),
                &ns(&b64),
                objc2_foundation::NSDataBase64DecodingOptions(0),
            ) else {
                return fail(reply, "malformed");
            };
            let data = data.to_vec();
            let document = core.document_id();
            if kind == "i" {
                let held = MainOnly(reply.copy());
                core.bridge_message(
                    channel,
                    origin,
                    main,
                    document,
                    data,
                    Some(Box::new(move |result| match result {
                        ReplyResult::Ok(bytes) => {
                            let s = ns(&base64(&bytes));
                            held.get().call((
                                Retained::as_ptr(&s).cast_mut().cast(),
                                std::ptr::null_mut(),
                            ));
                        }
                        ReplyResult::Err(message) => {
                            let s = ns(&message);
                            held.get()
                                .call((std::ptr::null_mut(), Retained::as_ptr(&s).cast_mut()));
                        }
                    })),
                );
            } else {
                core.bridge_message(channel, origin, main, document, data, None);
                reply.call((std::ptr::null_mut(), std::ptr::null_mut()));
            }
        }
    }
);

impl BridgeHandler {
    fn new(mtm: MainThreadMarker, id: NativeHandleId) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(BridgeIvars { id });
        // SAFETY: NSObject's designated initialiser.
        unsafe { msg_send![super(this), init] }
    }
}

// Scheme handler: shared by every configuration.

#[derive(Default)]
struct SchemeIvars {
    stopped: RefCell<Vec<usize>>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; no `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "IstmoWebViewSchemes"]
    #[ivars = SchemeIvars]
    struct SchemeHandler;

    unsafe impl NSObjectProtocol for SchemeHandler {}

    unsafe impl WKURLSchemeHandler for SchemeHandler {
        #[unsafe(method(webView:startURLSchemeTask:))]
        fn start(&self, web_view: &WKWebView, task: &ProtocolObject<dyn WKURLSchemeTask>) {
            let key = std::ptr::from_ref(task) as usize;
            self.ivars().stopped.borrow_mut().retain(|k| *k != key);
            // SAFETY: `task` is a live object; retaining keeps it past this call.
            let Some(task) = (unsafe { Retained::retain(std::ptr::from_ref(task).cast_mut()) })
            else {
                return;
            };
            let task = MainOnly(task);
            // SAFETY: live task.
            let (url, method) = unsafe {
                let request = task.0.request();
                (
                    request
                        .URL()
                        .and_then(|u| u.absoluteString())
                        .map(|s| s.to_string())
                        .unwrap_or_default(),
                    request
                        .HTTPMethod()
                        .map_or_else(|| "GET".into(), |m| m.to_string()),
                )
            };
            let Some(core) = id_of(web_view).and_then(core_of) else {
                return respond(&task.0, &url, 404, "text/plain", &[]);
            };
            let Ok(parsed) = url::Url::parse(&url) else {
                return respond(&task.0, &url, 400, "text/plain", &[]);
            };
            let Some(scheme) = core
                .config
                .schemes
                .iter()
                .find(|s| s.name == parsed.scheme())
            else {
                return respond(&task.0, &url, 404, "text/plain", &[]);
            };
            match &scheme.source {
                SchemeSource::Assets(source) => {
                    serve_asset(&task.0, &url, &source.dir, parsed.path());
                }
                SchemeSource::Dynamic => core.scheme_request(
                    SchemeRequestInfo {
                        request_id: 0,
                        scheme: scheme.name.clone(),
                        method,
                        url: url.clone(),
                        headers: Vec::new(),
                        has_body: false,
                        initiator_origin: None,
                    },
                    Box::new(MacResponder {
                        task: Some(task),
                        url,
                        handler: key,
                    }),
                ),
            }
        }

        #[unsafe(method(webView:stopURLSchemeTask:))]
        fn stop(&self, _web_view: &WKWebView, task: &ProtocolObject<dyn WKURLSchemeTask>) {
            self.ivars()
                .stopped
                .borrow_mut()
                .push(std::ptr::from_ref(task) as usize);
        }
    }
);

impl SchemeHandler {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(SchemeIvars::default());
        // SAFETY: NSObject's designated initialiser.
        unsafe { msg_send![super(this), init] }
    }

    /// `WebKit` raises if a stopped task is answered.
    fn stopped(key: usize) -> bool {
        UI.with(|ui| {
            ui.borrow()
                .scheme_handler
                .as_ref()
                .is_some_and(|h| h.ivars().stopped.borrow().contains(&key))
        })
    }
}

fn http_response(
    url: &str,
    status: u16,
    mime: &str,
    extra: &[crate::HttpHeader],
    length: Option<usize>,
) -> Option<Retained<NSHTTPURLResponse>> {
    let url = NSURL::URLWithString(&ns(url))?;
    let mut keys = vec![
        ns("Content-Type"),
        ns("X-Content-Type-Options"),
        ns("Cache-Control"),
    ];
    let mut values = vec![ns(mime), ns("nosniff"), ns("no-cache")];
    for h in extra {
        if h.name.eq_ignore_ascii_case("access-control-allow-origin") && h.value == "*" {
            continue;
        }
        keys.push(ns(&h.name));
        values.push(ns(&h.value));
    }
    if let Some(len) = length {
        keys.push(ns("Content-Length"));
        values.push(ns(&len.to_string()));
    }
    let key_refs: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
    let value_refs: Vec<&NSString> = values.iter().map(|v| &**v).collect();
    let headers = NSDictionary::from_slices(&key_refs, &value_refs);
    NSHTTPURLResponse::initWithURL_statusCode_HTTPVersion_headerFields(
        NSHTTPURLResponse::alloc(),
        &url,
        isize::try_from(status).unwrap_or(500),
        Some(&ns("HTTP/1.1")),
        Some(&headers),
    )
}

fn respond(
    task: &ProtocolObject<dyn WKURLSchemeTask>,
    url: &str,
    status: u16,
    mime: &str,
    body: &[u8],
) {
    let Some(response) = http_response(url, status, mime, &[], Some(body.len())) else {
        return;
    };
    // SAFETY: live, unstopped task answered on the main thread.
    unsafe {
        task.didReceiveResponse(&response);
        task.didReceiveData(&NSData::with_bytes(body));
        task.didFinish();
    }
}

fn serve_asset(task: &ProtocolObject<dyn WKURLSchemeTask>, url: &str, dir: &str, path: &str) {
    match super::read_asset(dir, path) {
        Ok((mime, bytes)) => respond(task, url, 200, mime, &bytes),
        Err(status) => respond(task, url, status, "text/plain", &[]),
    }
}

struct MacResponder {
    task: Option<MainOnly<Retained<ProtocolObject<dyn WKURLSchemeTask>>>>,
    url: String,
    handler: usize,
}

impl SchemeResponder for MacResponder {
    fn head(&mut self, head: &SchemeResponseHead) {
        let Some(task) = &self.task else { return };
        if SchemeHandler::stopped(self.handler) {
            self.task = None;
            return;
        }
        let length = head.content_length.and_then(|l| usize::try_from(l).ok());
        if let Some(response) = http_response(
            &self.url,
            head.status,
            &head.mime_type,
            &head.headers,
            length,
        ) {
            // SAFETY: live task on the main thread.
            unsafe { task.0.didReceiveResponse(&response) };
        }
    }

    fn body(&mut self, body: SchemeBody) {
        let Some(task) = &self.task else { return };
        if SchemeHandler::stopped(self.handler) {
            self.task = None;
            return;
        }
        // SAFETY: live task on the main thread.
        unsafe {
            match body {
                SchemeBody::Data(data) => task.0.didReceiveData(&NSData::with_bytes(&data)),
                SchemeBody::End => {
                    task.0.didFinish();
                    self.task = None;
                }
                SchemeBody::Abort(reason) => {
                    let error = NSError::new(-999, &ns(&reason));
                    task.0.didFailWithError(&error);
                    self.task = None;
                }
            }
        }
    }
}

/// A view the page opens, created now because `WebKit` wants it now.
fn popup(
    id: NativeHandleId,
    config: &WKWebViewConfiguration,
    action: &WKNavigationAction,
) -> Option<Retained<WKWebView>> {
    let opener = core_of(id)?;
    // SAFETY: live action.
    let url = unsafe { action.request().URL().and_then(|u| u.absoluteString()) }
        .map(|s| s.to_string())
        .unwrap_or_default();
    // WebKit wants the new view now: the rules decide.
    if !opener.allows_popup(&url) {
        return None;
    }
    let mut spec = opener.spec.clone();
    spec.target = RenderTarget::Window(crate::WindowTarget {
        title: String::new(),
        width: 0,
        height: 0,
        parent_window_id: None,
        modal: false,
        resizable: true,
    });
    let (core, instance) = opener.new_popup(spec)?;
    let child = create_with(core, config, mtm()).ok()?;
    opener.emit(crate::ControlEvent::NewView(NewViewInfo { instance, url }));
    Some(child)
}
