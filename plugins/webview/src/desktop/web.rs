//! Browser engine (`wasm32-unknown-unknown`, feature `web`): every view
//! is an `<iframe>` laid over the page.
//!
//! What a browser allows a parent page, and no more:
//!
//! - Only [`RenderTarget::Embedded`]. The iframe goes into the element
//!   with id `istmo-window-<window_id>` when the page has one
//!   (positioned inside it, so give it `position: relative`), otherwise
//!   into `<body>` with fixed positioning. Windows would be popups, which
//!   browsers block outside a user gesture: sign-in uses `istmo-web-auth`.
//! - The bridge is `postMessage`. The framed page loads
//!   [`crate::WEB_FRAME_BRIDGE`] to get `window.istmo`; the browser
//!   reports each message's origin, and only messages whose source is the
//!   view's own frame are read.
//! - Navigation rules apply to the app's loads; what the framed page
//!   navigates to itself is the browser's business (use `sandbox`
//!   restrictions and `frame-ancestors` on the content). `eval` works on
//!   same-origin frames only. Cookies, storage, history, find, zoom,
//!   snapshots and custom schemes are the browser's, not the app's.
//! - `sandbox` is set per load: `allow-same-origin` only for content
//!   whose origin differs from the app's, so framed content never runs
//!   with the app's origin and scripts at once.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use istmo_core::NativeHandleId;
use js_sys::{Function, Object, Reflect, Uint8Array};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::{HtmlIFrameElement, MessageEvent};

use crate::desktop::core::{Done, Engine, FrameSlot, UiTask, UiView, ViewCore};
use crate::present::FrameSink;
use crate::{
    ClearDataRequest, CookieFilter, FindQuery, FindResult, HistoryNav, LoadRequest, RenderTarget,
    ReplyResult, ScriptCall, ScriptWorld, SnapshotOptions, ViewFrame, WebCookie, WebHistory,
    WebSnapshot, WebViewCapabilities, WebViewError,
};

const SANDBOX: &str = "allow-scripts allow-forms allow-popups allow-popups-to-escape-sandbox";

fn window() -> Option<web_sys::Window> {
    web_sys::window()
}

/// The page's own origin.
fn app_origin() -> String {
    window()
        .and_then(|w| w.location().origin().ok())
        .unwrap_or_default()
}

fn js_text(value: &JsValue) -> String {
    value
        .as_string()
        .or_else(|| js_sys::JSON::stringify(value).ok().map(String::from))
        .unwrap_or_else(|| "script failed".into())
}

/// Thread-safe in name only: wasm32-unknown-unknown has one thread, and
/// every DOM object lives in thread-locals here.
#[derive(Debug, Default)]
pub(crate) struct WebEngine {
    frames: std::sync::Mutex<HashMap<NativeHandleId, Arc<FrameState>>>,
}

impl WebEngine {
    pub(crate) fn start() -> Result<Arc<Self>, WebViewError> {
        if window().is_none() {
            return Err(WebViewError::Unsupported(
                "web views need a window (not a worker)".into(),
            ));
        }
        install_listener();
        Ok(Arc::new(Self::default()))
    }
}

/// Open `url` in a new browsing context, without a handle to us.
pub(crate) fn open_external(url: &str) {
    if let Some(w) = window()
        && w.open_with_url_and_target_and_features(url, "_blank", "noopener,noreferrer")
            .is_err()
    {
        tracing::warn!("the browser refused to open a link");
    }
}

/// 8 bytes from `crypto.getRandomValues`, when the global has it.
pub(crate) fn random_bytes() -> Option<[u8; 8]> {
    let crypto = Reflect::get(&js_sys::global(), &"crypto".into()).ok()?;
    let fill: Function = Reflect::get(&crypto, &"getRandomValues".into())
        .ok()?
        .dyn_into()
        .ok()?;
    let array = Uint8Array::new_with_length(8);
    fill.call1(&crypto, &array).ok()?;
    let mut out = [0_u8; 8];
    array.copy_to(&mut out);
    Some(out)
}

impl Engine for WebEngine {
    fn describe(&self) -> (String, String) {
        let agent = window()
            .and_then(|w| w.navigator().user_agent().ok())
            .unwrap_or_default();
        ("iframe".into(), agent)
    }

    fn capabilities(&self, caps: &mut WebViewCapabilities) {
        caps.embedded = true;
        caps.bridge_reply = true;
    }

    fn post(&self, task: UiTask) {
        wasm_bindgen_futures::spawn_local(async move { task() });
    }

    fn post_after(self: Arc<Self>, ms: u32, task: UiTask) {
        let callback = Closure::once_into_js(task);
        let set_timeout = Reflect::get(&js_sys::global(), &"setTimeout".into())
            .ok()
            .and_then(|f| f.dyn_into::<Function>().ok());
        let delay = JsValue::from(ms);
        if set_timeout
            .and_then(|f| f.call2(&JsValue::NULL, &callback, &delay).ok())
            .is_none()
        {
            tracing::warn!("no setTimeout: a web view timeout will not fire");
        }
    }

    fn create(&self, core: Arc<ViewCore>, done: Done<()>) {
        self.post(Box::new(move || done(create(core))));
    }

    fn with_view(&self, id: NativeHandleId, f: crate::desktop::core::ViewTask) {
        self.post(Box::new(move || {
            let taken = VIEWS.with(|v| v.borrow_mut().remove(&id));
            match taken {
                Some(mut view) => {
                    f(Some(&mut view));
                    if view.closed {
                        view.destroy();
                    } else {
                        VIEWS.with(|v| v.borrow_mut().insert(id, view));
                    }
                }
                None => f(None),
            }
        }));
    }

    fn frame_sink(&self, id: NativeHandleId) -> Arc<dyn FrameSink> {
        let state = Arc::clone(
            self.frames
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .entry(id)
                .or_insert_with(|| {
                    Arc::new(FrameState {
                        slot: FrameSlot::default(),
                        scheduled: AtomicBool::new(false),
                    })
                }),
        );
        Arc::new(WebFrameSink { id, state })
    }
}

struct FrameState {
    slot: FrameSlot,
    scheduled: AtomicBool,
}

impl std::fmt::Debug for FrameState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FrameState").finish_non_exhaustive()
    }
}

struct WebFrameSink {
    id: NativeHandleId,
    state: Arc<FrameState>,
}

impl FrameSink for WebFrameSink {
    fn set_frame(&self, frame: ViewFrame) {
        // Coalesced: one style update per task, the latest frame.
        if !self.state.slot.store(frame) || self.state.scheduled.swap(true, Ordering::AcqRel) {
            return;
        }
        let (id, state) = (self.id, Arc::clone(&self.state));
        wasm_bindgen_futures::spawn_local(async move {
            state.scheduled.store(false, Ordering::Release);
            if let Some(frame) = state.slot.take() {
                VIEWS.with(|v| {
                    if let Some(view) = v.borrow().get(&id) {
                        view.apply_frame(frame);
                    }
                });
            }
        });
    }
}

// =====================================================================
// Views
// =====================================================================

struct FrameView {
    core: Arc<ViewCore>,
    frame: HtmlIFrameElement,
    /// URL of the app's last load, until its `load` event.
    expected: Option<String>,
    closed: bool,
    _onload: Closure<dyn FnMut()>,
}

/// The page-wide `message` listener.
type Listener = Closure<dyn FnMut(MessageEvent)>;

thread_local! {
    static VIEWS: RefCell<HashMap<NativeHandleId, FrameView>> = RefCell::new(HashMap::new());
    static LISTENER: RefCell<Option<Listener>> = const { RefCell::new(None) };
}

fn create(core: Arc<ViewCore>) -> Result<(), WebViewError> {
    let RenderTarget::Embedded(target) = &core.spec.target else {
        return Err(WebViewError::RenderModeUnsupported);
    };
    if !core.config.schemes.is_empty() {
        tracing::warn!("custom schemes do not exist in a browser; their loads fail");
    }
    let document = window()
        .and_then(|w| w.document())
        .ok_or_else(|| WebViewError::Backend("no document".into()))?;
    let frame: HtmlIFrameElement = document
        .create_element("iframe")
        .map_err(|e| WebViewError::Backend(js_text(&e)))?
        .unchecked_into();
    let container = document.get_element_by_id(&format!("istmo-window-{}", target.window_id));
    let inside = container.is_some();
    let parent: web_sys::Element = match container {
        Some(element) => element,
        None => document
            .body()
            .ok_or_else(|| WebViewError::Backend("no body".into()))?
            .into(),
    };
    let style = frame.style();
    let position = if inside { "absolute" } else { "fixed" };
    for (name, value) in [
        ("position", position),
        ("border", "0"),
        ("display", "none"),
        ("left", "0"),
        ("top", "0"),
    ] {
        style
            .set_property(name, value)
            .map_err(|e| WebViewError::Backend(js_text(&e)))?;
    }
    let s = &core.spec.settings;
    if s.transparent {
        style.set_property("background", "transparent").ok();
        frame.set_attribute("allowtransparency", "true").ok();
    }
    frame
        .set_attribute("referrerpolicy", "strict-origin-when-cross-origin")
        .ok();
    frame.set_attribute("sandbox", SANDBOX).ok();
    let id = core.id;
    let onload = Closure::<dyn FnMut()>::new(move || on_load(id));
    frame.set_onload(Some(onload.as_ref().unchecked_ref()));
    parent
        .append_child(&frame)
        .map_err(|e| WebViewError::Backend(js_text(&e)))?;
    VIEWS.with(|v| {
        v.borrow_mut().insert(
            id,
            FrameView {
                core,
                frame,
                expected: None,
                closed: false,
                _onload: onload,
            },
        );
    });
    Ok(())
}

fn on_load(id: NativeHandleId) {
    let loaded = VIEWS.with(|v| {
        let mut views = v.borrow_mut();
        let view = views.get_mut(&id)?;
        // A cross-origin frame hides its location; the app's own load
        // is the best name for it, an unknown document has none.
        let readable = view
            .frame
            .content_window()
            .and_then(|w| w.location().href().ok());
        let expected = view.expected.take();
        let url = readable.or_else(|| expected.clone()).unwrap_or_default();
        if url == "about:blank" && expected.is_none() {
            return None;
        }
        Some((Arc::clone(&view.core), url, expected.is_some()))
    });
    let Some((core, url, announced)) = loaded else {
        return;
    };
    if !announced {
        core.started(&url);
    }
    core.committed(&url);
    core.finished(&url, false, false);
}

impl FrameView {
    fn apply_frame(&self, frame: ViewFrame) {
        let style = self.frame.style();
        if !frame.visible || frame.width == 0 || frame.height == 0 {
            style.set_property("display", "none").ok();
            return;
        }
        // Frames are physical pixels; CSS wants logical ones.
        let ratio = window().map_or(1.0, |w| w.device_pixel_ratio()).max(0.1);
        let px = |v: f64| format!("{:.2}px", v / ratio);
        style.set_property("left", &px(f64::from(frame.x))).ok();
        style.set_property("top", &px(f64::from(frame.y))).ok();
        style
            .set_property("width", &px(f64::from(frame.width)))
            .ok();
        style
            .set_property("height", &px(f64::from(frame.height)))
            .ok();
        style.set_property("display", "block").ok();
    }

    fn destroy(&self) {
        let id = self.core.id;
        self.frame.set_onload(None);
        self.frame.remove();
        VIEWS.with(|v| v.borrow_mut().remove(&id));
    }
}

/// `sandbox` for a load of `url`: the content keeps its own origin unless
/// that origin is the app's.
fn sandbox_for(url: &str) -> String {
    let foreign = url::Url::parse(url)
        .ok()
        .map(|u| u.origin().ascii_serialization())
        .is_some_and(|o| o != "null" && o != app_origin());
    if foreign {
        format!("{SANDBOX} allow-same-origin")
    } else {
        SANDBOX.to_owned()
    }
}

impl UiView for FrameView {
    fn load(&mut self, request: &LoadRequest) {
        if !request.headers.is_empty() || request.post_body.is_some() {
            tracing::warn!("headers and POST bodies cannot be set on an iframe load");
        }
        self.frame
            .set_attribute("sandbox", &sandbox_for(&request.url))
            .ok();
        self.frame.remove_attribute("srcdoc").ok();
        self.expected = Some(request.url.clone());
        self.core.started(&request.url);
        self.frame.set_src(&request.url);
    }

    fn load_html(&mut self, html: &str, _base_url: Option<&str>) {
        // `srcdoc` would inherit the app's origin: never with
        // `allow-same-origin`, so it runs with an opaque one.
        self.frame.set_attribute("sandbox", SANDBOX).ok();
        self.expected = Some("about:srcdoc".into());
        self.core.started("about:srcdoc");
        self.frame.set_srcdoc(html);
    }

    fn navigate(&mut self, nav: HistoryNav) {
        let history = self.frame.content_window().and_then(|w| w.history().ok());
        let step = match nav {
            HistoryNav::Back => -1,
            HistoryNav::Forward => 1,
            HistoryNav::Offset(n) => n,
            HistoryNav::Reload | HistoryNav::ReloadBypassCache => {
                let src = self.frame.src();
                self.frame.set_src(&src);
                return;
            }
            HistoryNav::Stop => {
                if let Some(w) = self.frame.content_window() {
                    w.stop().ok();
                }
                return;
            }
        };
        // Cross-origin frames refuse history access: nothing to do then.
        if let Some(history) = history {
            history.go_with_delta(step).ok();
        }
    }

    fn history(&mut self) -> WebHistory {
        WebHistory {
            entries: Vec::new(),
            current: 0,
        }
    }

    fn focus(&mut self) {
        self.frame.focus().ok();
    }

    fn eval(&mut self, call: &ScriptCall, done: Done<String>) {
        if call.world == ScriptWorld::Isolated {
            return done(Err(WebViewError::Unsupported(
                "isolated worlds in a browser".into(),
            )));
        }
        let Some(target) = self.frame.content_window() else {
            return done(Err(WebViewError::ViewClosed));
        };
        let Some(eval) = Reflect::get(&target, &"eval".into())
            .ok()
            .and_then(|f| f.dyn_into::<Function>().ok())
        else {
            return done(Err(WebViewError::Unsupported(
                "eval in a cross-origin frame".into(),
            )));
        };
        let value = match eval.call1(&target, &JsValue::from_str(&call.source)) {
            Ok(value) => value,
            Err(e) => return done(Err(WebViewError::Backend(js_text(&e)))),
        };
        let json = |v: &JsValue| {
            js_sys::JSON::stringify(v)
                .ok()
                .and_then(|s| s.as_string())
                .unwrap_or_else(|| "null".into())
        };
        match value.dyn_into::<js_sys::Promise>() {
            Ok(promise) => wasm_bindgen_futures::spawn_local(async move {
                done(
                    wasm_bindgen_futures::JsFuture::from(promise)
                        .await
                        .map(|v| json(&v))
                        .map_err(|e| WebViewError::Backend(js_text(&e))),
                );
            }),
            Err(value) => done(Ok(json(&value))),
        }
    }

    fn deliver(&mut self, channel: &str, data: &[u8]) {
        let (Some(target), Some(origin)) =
            (self.frame.content_window(), self.core.committed_origin())
        else {
            return;
        };
        let message = Object::new();
        Reflect::set(&message, &"__istmo".into(), &JsValue::from(1)).ok();
        Reflect::set(&message, &"e".into(), &JsValue::from_str(channel)).ok();
        Reflect::set(&message, &"d".into(), &Uint8Array::from(data)).ok();
        // The browser drops it if the frame is no longer at `origin`.
        target.post_message(&message, &origin).ok();
    }

    fn cookies(&mut self, _filter: &CookieFilter, done: Done<Vec<WebCookie>>) {
        done(Err(WebViewError::Unsupported(
            "cookies in a browser".into(),
        )));
    }

    fn set_cookie(&mut self, _cookie: &WebCookie, done: Done<()>) {
        done(Err(WebViewError::Unsupported(
            "cookies in a browser".into(),
        )));
    }

    fn clear_data(&mut self, _request: &ClearDataRequest, done: Done<()>) {
        done(Err(WebViewError::Unsupported(
            "site data in a browser".into(),
        )));
    }

    fn set_zoom(&mut self, _factor: f32) {}

    fn find(&mut self, _query: &FindQuery, done: Done<FindResult>) {
        done(Err(WebViewError::Unsupported("find in a browser".into())));
    }

    fn snapshot(&mut self, _options: SnapshotOptions, done: Done<WebSnapshot>) {
        done(Err(WebViewError::Unsupported(
            "snapshots in a browser".into(),
        )));
    }

    fn print(&mut self) -> Result<(), WebViewError> {
        self.frame
            .content_window()
            .ok_or(WebViewError::ViewClosed)?
            .print()
            .map_err(|_| WebViewError::Unsupported("printing a cross-origin frame".into()))
    }

    fn set_inspector(&mut self, _open: bool) -> Result<(), WebViewError> {
        Err(WebViewError::Unsupported(
            "the inspector is the browser's devtools".into(),
        ))
    }

    fn close(&mut self) {
        self.closed = true;
    }
}

// =====================================================================
// Bridge
// =====================================================================

fn install_listener() {
    LISTENER.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_some() {
            return;
        }
        let Some(w) = window() else { return };
        let listener = Closure::<dyn FnMut(MessageEvent)>::new(on_message);
        if w.add_event_listener_with_callback("message", listener.as_ref().unchecked_ref())
            .is_ok()
        {
            *slot = Some(listener);
        }
    });
}

fn field(data: &JsValue, key: &str) -> JsValue {
    Reflect::get(data, &key.into()).unwrap_or(JsValue::UNDEFINED)
}

fn bytes_of(value: &JsValue) -> Vec<u8> {
    if let Some(text) = value.as_string() {
        return text.into_bytes();
    }
    if value.is_instance_of::<Uint8Array>() || value.is_instance_of::<js_sys::ArrayBuffer>() {
        return Uint8Array::new(value).to_vec();
    }
    Vec::new()
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "wasm-bindgen closures take the event by value"
)]
fn on_message(event: MessageEvent) {
    let data = event.data();
    if field(&data, "__istmo").as_f64() != Some(1.0) {
        return;
    }
    let Some(source) = event.source() else { return };
    // The frame whose own window sent this; nothing else is read.
    let owner = VIEWS.with(|v| {
        v.borrow().values().find_map(|view| {
            let window = view.frame.content_window()?;
            Object::is(window.as_ref(), source.as_ref()).then(|| Arc::clone(&view.core))
        })
    });
    let Some(core) = owner else { return };
    let (Some(kind), Some(channel)) =
        (field(&data, "k").as_string(), field(&data, "c").as_string())
    else {
        return;
    };
    let payload = bytes_of(&field(&data, "d"));
    let origin = event.origin();
    let document = core.document_id();
    match kind.as_str() {
        "post" => core.bridge_message(channel, origin, true, document, payload, None),
        "invoke" => {
            let Some(call) = field(&data, "i").as_f64() else {
                return;
            };
            let id = core.id;
            let reply_origin = origin.clone();
            core.bridge_message(
                channel,
                origin,
                true,
                document,
                payload,
                Some(Box::new(move |result| {
                    reply(id, &reply_origin, call, result);
                })),
            );
        }
        _ => {}
    }
}

/// Settle the framed page's `invoke` `call`.
fn reply(id: NativeHandleId, origin: &str, call: f64, result: ReplyResult) {
    let target = VIEWS.with(|v| {
        v.borrow()
            .get(&id)
            .and_then(|view| view.frame.content_window())
    });
    let Some(target) = target else { return };
    let message = Object::new();
    Reflect::set(&message, &"__istmo".into(), &JsValue::from(1)).ok();
    Reflect::set(&message, &"r".into(), &JsValue::from(call)).ok();
    match result {
        ReplyResult::Ok(bytes) => {
            Reflect::set(&message, &"ok".into(), &JsValue::TRUE).ok();
            Reflect::set(&message, &"d".into(), &Uint8Array::from(bytes.as_slice())).ok();
        }
        ReplyResult::Err(text) => {
            Reflect::set(&message, &"ok".into(), &JsValue::FALSE).ok();
            Reflect::set(&message, &"d".into(), &JsValue::from_str(&text)).ok();
        }
    }
    target.post_message(&message, origin).ok();
}
