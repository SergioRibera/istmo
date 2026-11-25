//! Qt `WebEngine` (Chromium) engine through the C++ shim in `native/qt`,
//! loaded with `dlopen` on a dedicated Qt GUI thread.
//!
//! The shim owns every Qt object; this side keeps the `ViewCore`s and
//! answers the shim's callbacks, which all run on the Qt thread and name
//! the view by id, so a late callback never touches freed state.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::path::PathBuf;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use istmo_core::NativeHandleId;
use istmo_core::sync::lock;
use libloading::Library;

use crate::desktop::core::{Done, Engine, FrameSlot, SchemeResponder, UiTask, UiView, ViewCore};
use crate::desktop::text::{base64, json_escape, origin_guard};
use crate::present::FrameSink;
use crate::{
    ClearDataRequest, ContextMenuMode, CookieFilter, DataKind, DecisionKind, DecisionResponse,
    FindQuery, FindResult, HistoryEntry, HistoryNav, InjectionTime, LoadRequest, NavErrorKind,
    NewViewInfo, PermissionAsk, RenderTarget, ReplyResult, SameSite, SchemeBody, SchemeRequestInfo,
    SchemeResponseHead, SchemeSource, ScriptCall, ScriptWorld, SnapshotFormat, SnapshotOptions,
    TerminationInfo, TerminationReason, ViewFrame, WebCookie, WebHistory, WebPermission,
    WebProfile, WebSnapshot, WebViewCapabilities, WebViewError,
};

/// File name of the shim library.
const SHIM_NAME: &str = "libistmo_webview_qt.so";

/// Longest wait for a promise an `eval` returned.
const EVAL_PROMISE_MS: u32 = 60_000;

// =====================================================================
// C ABI of the shim (`native/qt/istmo_webview_qt.cpp`)
// =====================================================================

type Task = unsafe extern "C" fn(*mut c_void);
type HistoryItem = unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char);

#[repr(C)]
struct Callbacks {
    navigation: unsafe extern "C" fn(u64, *const c_char, c_int) -> c_int,
    load_started: unsafe extern "C" fn(u64, *const c_char),
    committed: unsafe extern "C" fn(u64, *const c_char),
    load_finished:
        unsafe extern "C" fn(u64, *const c_char, c_int, c_int, c_int, *const c_char, c_int, c_int),
    title: unsafe extern "C" fn(u64, *const c_char),
    progress: unsafe extern "C" fn(u64, c_int),
    close_requested: unsafe extern "C" fn(u64),
    terminated: unsafe extern "C" fn(u64, c_int, c_int),
    bridge: unsafe extern "C" fn(
        u64,
        u64,
        *const c_char,
        *const c_char,
        *const c_char,
        *const u8,
        usize,
    ),
    scheme: unsafe extern "C" fn(u64, u64, *const c_char, *const c_char, *const c_char, c_int),
    permission: unsafe extern "C" fn(u64, u64, *const c_char, c_int),
    eval_done: unsafe extern "C" fn(u64, c_int, *const c_char),
    find_done: unsafe extern "C" fn(u64, c_int, c_int),
    popup: unsafe extern "C" fn(u64, *const c_char, c_int) -> u64,
}

#[repr(C)]
struct Script {
    source: *const c_char,
    at_end: c_int,
    subframes: c_int,
    isolated: c_int,
}

#[repr(C)]
struct ViewSpec {
    id: u64,
    profile_kind: c_int,
    profile_name: *const c_char,
    data_dir: *const c_char,
    opener: u64,
    embedded: c_int,
    parent_xid: u64,
    title: *const c_char,
    width: c_int,
    height: c_int,
    resizable: c_int,
    user_agent: *const c_char,
    user_agent_suffix: *const c_char,
    accept_language: *const c_char,
    javascript: c_int,
    local_storage: c_int,
    autoplay: c_int,
    third_party_cookies: c_int,
    context_menu: c_int,
    background_rgba: u32,
}

/// Resolve every listed symbol from the shim.
macro_rules! shim {
    ($( fn $name:ident ( $($arg:ty),* ) $(-> $ret:ty)? ; )*) => {
        /// The shim's exports.
        struct Shim {
            $( $name: unsafe extern "C" fn($($arg),*) $(-> $ret)?, )*
            _lib: Library,
        }

        impl Shim {
            fn load(lib: Library) -> Result<Self, String> {
                Ok(Self {
                    $(
                        // SAFETY: the signature matches the C prototype in
                        // `istmo_webview_qt.cpp`; `Shim` keeps the library
                        // loaded for as long as the pointer.
                        $name: unsafe {
                            *lib.get::<unsafe extern "C" fn($($arg),*) $(-> $ret)?>(
                                concat!(stringify!($name), "\0").as_bytes(),
                            )
                            .map_err(|e| format!("{}: {e}", stringify!($name)))?
                        },
                    )*
                    _lib: lib,
                })
            }
        }
    };
}

shim! {
    fn istmo_qt_init(*const *const c_char, usize, *const c_char, Callbacks) -> c_int;
    fn istmo_qt_run();
    fn istmo_qt_post(Task, *mut c_void);
    fn istmo_qt_post_after(c_int, Task, *mut c_void);
    fn istmo_qt_version() -> *const c_char;
    fn istmo_qt_view_new(*const ViewSpec) -> c_int;
    fn istmo_qt_view_close(u64);
    fn istmo_qt_view_set_scripts(u64, *const Script, usize);
    fn istmo_qt_view_load(u64, *const c_char, *const *const c_char, *const *const c_char, usize, *const u8, usize);
    fn istmo_qt_view_load_html(u64, *const c_char, *const c_char);
    fn istmo_qt_view_navigate(u64, c_int, c_int);
    fn istmo_qt_view_history(u64, HistoryItem, *mut c_void) -> c_int;
    fn istmo_qt_view_geometry(u64, c_int, c_int, c_int, c_int, c_int);
    fn istmo_qt_view_focus(u64);
    fn istmo_qt_view_eval(u64, *const c_char, c_int, u64);
    fn istmo_qt_view_zoom(u64, f64);
    fn istmo_qt_view_find(u64, *const c_char, c_int, c_int, u64);
    fn istmo_qt_view_clear_data(u64, c_int, c_int);
    fn istmo_qt_view_set_cookie(u64, *const c_char, *const c_char, *const c_char, *const c_char, *const c_char, i64, c_int, c_int, c_int);
    fn istmo_qt_view_snapshot(u64, *mut usize, *mut c_int, *mut c_int) -> *mut u8;
    fn istmo_qt_free(*mut c_void);
    fn istmo_qt_view_inspector(u64, c_int) -> c_int;
    fn istmo_qt_response_head(u64, c_int, *const c_char, *const *const c_char, *const *const c_char, usize);
    fn istmo_qt_response_data(u64, *const u8, usize);
    fn istmo_qt_response_end(u64, c_int);
    fn istmo_qt_permission_reply(u64, c_int);
    fn istmo_qt_quit();
}

static SHIM: std::sync::OnceLock<&'static Shim> = std::sync::OnceLock::new();

fn shim() -> &'static Shim {
    SHIM.get()
        .expect("invariant: the Qt thread loaded the shim before any callback")
}

fn cstring(s: &str) -> CString {
    CString::new(s.replace('\0', "")).unwrap_or_default()
}

fn text(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: the shim passes NUL-terminated strings that live for the
    // duration of the callback.
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

/// Where the shim may be, in order: the app's explicit path, the
/// `ISTMO_WEBVIEW_QT_SHIM` environment variable, next to the executable
/// (and its `../lib`), the copy this crate's build produced, then the
/// loader's own search path.
fn shim_candidates(explicit: Option<&PathBuf>) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = explicit.cloned().into_iter().collect();
    out.extend(std::env::var_os("ISTMO_WEBVIEW_QT_SHIM").map(PathBuf::from));
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(std::path::Path::to_path_buf))
    {
        out.push(dir.join(SHIM_NAME));
        out.push(dir.join("../lib").join(SHIM_NAME));
    }
    out.extend(option_env!("ISTMO_WEBVIEW_QT_SHIM").map(PathBuf::from));
    out.push(PathBuf::from(SHIM_NAME));
    out
}

fn open_shim(explicit: Option<&PathBuf>) -> Result<Library, WebViewError> {
    let mut tried = Vec::new();
    for path in shim_candidates(explicit) {
        // Bare names go through the loader search path; others must exist.
        if path.components().count() > 1 && !path.exists() {
            continue;
        }
        // SAFETY: loading the shim runs Qt's static initialisers only.
        match unsafe { Library::new(&path) } {
            Ok(lib) => return Ok(lib),
            Err(err) => tried.push(err.to_string()),
        }
    }
    Err(WebViewError::RuntimeMissing(format!(
        "{SHIM_NAME} (Qt WebEngine shim) not loadable: {}",
        tried.join("; ")
    )))
}

// =====================================================================
// Engine
// =====================================================================

/// The Qt thread and the shim.
pub(crate) struct QtEngine {
    shim: &'static Shim,
    version: String,
    embedding: bool,
    frames: Mutex<HashMap<NativeHandleId, Arc<FrameState>>>,
}

impl std::fmt::Debug for QtEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QtEngine")
            .field("version", &self.version)
            .finish_non_exhaustive()
    }
}

type Started = Result<(&'static Shim, String), WebViewError>;

impl QtEngine {
    /// Start the Qt thread. `schemes` are registered once, before Qt
    /// starts: Qt cannot add schemes later. `x11` asks for the xcb
    /// platform (embedding).
    pub(crate) fn start(
        shim_path: Option<&PathBuf>,
        schemes: Vec<String>,
        x11: bool,
    ) -> Result<Arc<Self>, WebViewError> {
        let lib = open_shim(shim_path)?;
        let (tx, rx) = flume::bounded::<Started>(1);
        std::thread::Builder::new()
            .name("istmo-webview-qt".into())
            .spawn(move || run_qt_thread(lib, &schemes, x11, &tx))
            .map_err(|e| WebViewError::Backend(format!("spawn Qt thread: {e}")))?;
        let (shim, version) = rx
            .recv()
            .map_err(|_| WebViewError::Backend("Qt thread exited".into()))??;
        let engine = Arc::new(Self {
            shim,
            version,
            embedding: x11,
            frames: Mutex::default(),
        });
        GLOBAL.get_or_init(|| Arc::downgrade(&engine));
        Ok(engine)
    }
}

static GLOBAL: std::sync::OnceLock<std::sync::Weak<QtEngine>> = std::sync::OnceLock::new();

fn run_qt_thread(lib: Library, schemes: &[String], x11: bool, started: &flume::Sender<Started>) {
    let shim: &'static Shim = match Shim::load(lib) {
        Ok(shim) => Box::leak(Box::new(shim)),
        Err(err) => {
            drop(started.send(Err(WebViewError::RuntimeMissing(err))));
            return;
        }
    };
    let names: Vec<CString> = schemes.iter().map(|s| cstring(s)).collect();
    let list: Vec<*const c_char> = names.iter().map(|c| c.as_ptr()).collect();
    let platform = x11.then(|| cstring("xcb"));
    let callbacks = Callbacks {
        navigation: on_navigation,
        load_started: on_load_started,
        committed: on_committed,
        load_finished: on_load_finished,
        title: on_title,
        progress: on_progress,
        close_requested: on_close_requested,
        terminated: on_terminated,
        bridge: on_bridge,
        scheme: on_scheme,
        permission: on_permission,
        eval_done: on_eval_done,
        find_done: on_find_done,
        popup: on_popup,
    };
    // SAFETY: strings outlive the call (Qt copies them); first and only
    // init on this thread.
    let status = unsafe {
        (shim.istmo_qt_init)(
            list.as_ptr(),
            list.len(),
            platform.as_ref().map_or(ptr::null(), |p| p.as_ptr()),
            callbacks,
        )
    };
    if status != 0 {
        drop(started.send(Err(WebViewError::ToolkitBusy)));
        return;
    }
    SHIM.set(shim).ok();
    UI.with(|cell| {
        *cell.borrow_mut() = Some(Ui {
            views: HashMap::new(),
            evals: HashMap::new(),
            finds: HashMap::new(),
            next_token: 1,
            schemes: schemes.to_vec(),
        });
    });
    // SAFETY: static NUL-terminated string from Qt.
    let version = text(unsafe { (shim.istmo_qt_version)() });
    let (quit_tx, quit_rx) = flume::bounded::<()>(1);
    QUIT.set(quit_rx).ok();
    // SAFETY: `on_exit` is a plain `extern "C" fn()`.
    if unsafe { atexit(on_exit) } != 0 {
        tracing::warn!("no exit hook: Qt may crash if the process exits with views open");
    }
    drop(started.send(Ok((shim, version))));
    // SAFETY: the event loop of the application created above.
    unsafe { (shim.istmo_qt_run)() };
    quit_tx.send(()).ok();
}

unsafe extern "C" fn run_task(data: *mut c_void) {
    // SAFETY: `data` is the `Box<UiTask>` from `post`, taken once.
    let task = unsafe { Box::from_raw(data.cast::<UiTask>()) };
    task();
}

impl Engine for QtEngine {
    fn describe(&self) -> (String, String) {
        ("qtwebengine".into(), self.version.clone())
    }

    fn capabilities(&self, caps: &mut WebViewCapabilities) {
        caps.windowed = true;
        caps.embedded = self.embedding;
        caps.persistent_profiles = true;
        caps.ephemeral_profiles = true;
        caps.isolated_world = true;
        caps.bridge_reply = true;
        caps.find = true;
        caps.snapshot = true;
    }

    fn post(&self, task: UiTask) {
        let data = Box::into_raw(Box::new(task));
        // SAFETY: `istmo_qt_post` is thread-safe; `run_task` takes `data`
        // exactly once.
        unsafe { (self.shim.istmo_qt_post)(run_task, data.cast()) };
    }

    fn create(&self, core: Arc<ViewCore>, done: Done<()>) {
        self.post(Box::new(move || {
            let result = ui(|ui| ui.create(core, None))
                .unwrap_or_else(|| Err(WebViewError::Backend("Qt thread busy".into())));
            done(result);
        }));
    }

    fn with_view(&self, id: NativeHandleId, f: crate::desktop::core::ViewTask) {
        self.post(Box::new(move || {
            let taken = ui(|ui| ui.views.remove(&id)).flatten();
            match taken {
                Some(mut view) => {
                    f(Some(&mut view));
                    if view.closed {
                        view.destroy();
                    } else {
                        ui(|ui| ui.views.insert(id, view));
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
        Arc::new(QtFrameSink {
            id,
            state,
            engine: GLOBAL.get().cloned().unwrap_or_default(),
        })
    }
}

struct FrameState {
    slot: FrameSlot,
    scheduled: AtomicBool,
}

struct QtFrameSink {
    id: NativeHandleId,
    state: Arc<FrameState>,
    engine: std::sync::Weak<QtEngine>,
}

impl FrameSink for QtFrameSink {
    fn set_frame(&self, frame: ViewFrame) {
        // Coalesced: one geometry update per Qt tick, the latest frame.
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
                apply_frame(id, frame);
            }
        }));
    }
}

fn apply_frame(id: NativeHandleId, frame: ViewFrame) {
    let embedded = ui(|ui| ui.views.get(&id).map(|v| v.embedded))
        .flatten()
        .unwrap_or(false);
    if !embedded {
        return;
    }
    let clamp = |v: u32| c_int::try_from(v).unwrap_or(c_int::MAX);
    // SAFETY: Qt thread; unknown ids are ignored by the shim.
    unsafe {
        (shim().istmo_qt_view_geometry)(
            id.0,
            frame.x,
            frame.y,
            clamp(frame.width),
            clamp(frame.height),
            c_int::from(frame.visible),
        );
    }
}

// =====================================================================
// Qt-thread state
// =====================================================================

struct Ui {
    views: HashMap<NativeHandleId, QtView>,
    evals: HashMap<u64, PendingEval>,
    finds: HashMap<u64, Done<FindResult>>,
    next_token: u64,
    schemes: Vec<String>,
}

impl Ui {
    const fn token(&mut self) -> u64 {
        self.next_token += 1;
        self.next_token
    }
}

/// An `eval` whose value is a promise still settling.
struct PendingEval {
    view: NativeHandleId,
    isolated: bool,
    waited_ms: u32,
    done: Done<String>,
}

/// Per-view state the callbacks need while the view is taken out of
/// the table by `with_view`.
struct ViewHooks {
    core: Arc<ViewCore>,
    /// A load the app allowed after an `Ask`: let it through once.
    approved: Option<String>,
    /// A navigation refused here: its abort is not a second failure.
    refused: Option<String>,
    /// `load_html` in flight: Qt turns it into a `data:` navigation,
    /// which is the app's own load, not the page's.
    html_pending: bool,
}

thread_local! {
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
    static HOOKS: RefCell<HashMap<NativeHandleId, ViewHooks>> = RefCell::new(HashMap::new());
}

fn ui<R>(f: impl FnOnce(&mut Ui) -> R) -> Option<R> {
    UI.with(|cell| cell.try_borrow_mut().ok()?.as_mut().map(f))
}

fn hooks<R>(id: NativeHandleId, f: impl FnOnce(&mut ViewHooks) -> R) -> Option<R> {
    HOOKS.with(|h| h.try_borrow_mut().ok()?.get_mut(&id).map(f))
}

fn core_of(id: u64) -> Option<Arc<ViewCore>> {
    hooks(NativeHandleId(id), |h| Arc::clone(&h.core))
}

/// A native view on the Qt thread.
struct QtView {
    core: Arc<ViewCore>,
    embedded: bool,
    closed: bool,
}

impl Ui {
    fn create(
        &mut self,
        core: Arc<ViewCore>,
        opener: Option<NativeHandleId>,
    ) -> Result<(), WebViewError> {
        for scheme in &core.config.schemes {
            if !self.schemes.contains(&scheme.name) {
                tracing::warn!(
                    scheme = %scheme.name,
                    "Qt registers schemes once per process; this one was not in the first config"
                );
            }
        }
        let spec = &core.spec;
        let s = &spec.settings;
        let (embedded, parent_xid, title, width, height, resizable) = match &spec.target {
            RenderTarget::Embedded(target) => (true, parent_xid(target.window_id)?, "", 0, 0, true),
            RenderTarget::Window(w) => (
                false,
                0,
                w.title.as_str(),
                c_int::try_from(w.width).unwrap_or(0),
                c_int::try_from(w.height).unwrap_or(0),
                w.resizable,
            ),
            #[allow(
                unreachable_patterns,
                reason = "render targets are non-exhaustive on the wire"
            )]
            _ => return Err(WebViewError::RenderModeUnsupported),
        };
        let (profile_kind, profile_name, data_dir) = match (opener, &spec.profile) {
            (Some(_), _) => (3, String::new(), String::new()),
            (None, WebProfile::Shared) => (0, String::new(), String::new()),
            (None, WebProfile::Ephemeral) => (1, String::new(), String::new()),
            (None, WebProfile::Persistent(p)) => (2, p.name.clone(), super::profile_dir(&p.name)?),
        };
        let cstr = |s: &str| cstring(s);
        let opt = |s: Option<&String>| s.map(|s| cstring(s));
        let (title, profile_name, data_dir) = (cstr(title), cstr(&profile_name), cstr(&data_dir));
        let user_agent = opt(s.user_agent.as_ref());
        let suffix = opt(core.config.user_agent_suffix.as_ref());
        let language = opt(s.accept_language.as_ref());
        let bg = s.background;
        let alpha = if s.transparent { 0 } else { bg.a };
        let native = ViewSpec {
            id: core.id.0,
            profile_kind,
            profile_name: profile_name.as_ptr(),
            data_dir: data_dir.as_ptr(),
            opener: opener.map_or(0, |o| o.0),
            embedded: c_int::from(embedded),
            parent_xid,
            title: title.as_ptr(),
            width,
            height,
            resizable: c_int::from(resizable),
            user_agent: user_agent.as_ref().map_or(ptr::null(), |c| c.as_ptr()),
            user_agent_suffix: suffix.as_ref().map_or(ptr::null(), |c| c.as_ptr()),
            accept_language: language.as_ref().map_or(ptr::null(), |c| c.as_ptr()),
            javascript: c_int::from(s.javascript),
            local_storage: c_int::from(s.dom_storage),
            autoplay: c_int::from(s.media_autoplay),
            third_party_cookies: c_int::from(s.third_party_cookies),
            context_menu: c_int::from(s.context_menu != ContextMenuMode::Hidden),
            background_rgba: u32::from_be_bytes([bg.r, bg.g, bg.b, alpha]),
        };
        // SAFETY: every pointer in `native` outlives the call.
        let status = unsafe { (shim().istmo_qt_view_new)(&raw const native) };
        match status {
            0 => {}
            -2 => return Err(WebViewError::RenderModeUnsupported),
            _ => {
                return Err(WebViewError::Backend(format!(
                    "Qt view creation failed ({status})"
                )));
            }
        }
        install_scripts(&core);
        HOOKS.with(|h| {
            h.borrow_mut().insert(
                core.id,
                ViewHooks {
                    core: Arc::clone(&core),
                    approved: None,
                    refused: None,
                    html_pending: false,
                },
            );
        });
        self.views.insert(
            core.id,
            QtView {
                core,
                embedded,
                closed: false,
            },
        );
        Ok(())
    }
}

/// XID of the app window an embedded view goes into.
fn parent_xid(window_id: u64) -> Result<u64, WebViewError> {
    let parent = istmo_window::WindowRegistry::global()
        .get(istmo_window::WindowId(window_id))
        .ok_or_else(|| {
            WebViewError::InvalidArgument(format!("window {window_id} is not registered"))
        })?;
    match parent {
        istmo_window::NativeWindow::Xlib { window, .. } => Ok(window.get()),
        istmo_window::NativeWindow::Xcb { window, .. } => Ok(u64::from(window.get())),
        _ => Err(WebViewError::RenderModeUnsupported),
    }
}

fn install_scripts(core: &ViewCore) {
    let mut sources: Vec<(CString, &InjectionTime, bool, bool)> = Vec::new();
    for script in &core.spec.user_scripts {
        let source = if script.allowed_origins.is_empty() {
            cstring(&script.source)
        } else {
            cstring(&origin_guard(&script.allowed_origins, &script.source))
        };
        sources.push((
            source,
            &script.inject_at,
            !script.main_frame_only,
            script.world == ScriptWorld::Isolated,
        ));
    }
    if let Some(bridge) = &core.spec.bridge
        && !core.bridge_origins().is_empty()
    {
        sources.push((
            cstring(&origin_guard(
                core.bridge_origins(),
                &shim_js(bridge.allow_subframes),
            )),
            &InjectionTime::DocumentStart,
            bridge.allow_subframes,
            false,
        ));
    }
    let list: Vec<Script> = sources
        .iter()
        .map(|(source, at, subframes, isolated)| Script {
            source: source.as_ptr(),
            at_end: c_int::from(**at == InjectionTime::DocumentEnd),
            subframes: c_int::from(*subframes),
            isolated: c_int::from(*isolated),
        })
        .collect();
    // SAFETY: the sources outlive the call; Qt copies them.
    unsafe { (shim().istmo_qt_view_set_scripts)(core.id.0, list.as_ptr(), list.len()) };
}

/// Page side of the bridge: `fetch` to `istmo-bridge://<kind>/<channel>`.
/// The engine reports the initiating origin, so nothing the page says
/// about itself is trusted. `fetch` is captured before page scripts run.
fn shim_js(subframes: bool) -> String {
    format!(
        r#"(() => {{
  if (window.istmo{top}) return;
  const nativeFetch = window.fetch.bind(window);
  const listeners = new Set();
  const enc = (d) => typeof d === "string" ? new TextEncoder().encode(d) : d instanceof Uint8Array ? d : new Uint8Array(d ?? []);
  const send = (kind, channel, data) => nativeFetch("istmo-bridge://" + kind + "/" + encodeURIComponent(String(channel)), {{ method: "POST", body: enc(data), cache: "no-store" }});
  Object.defineProperty(window, "__istmoDeliver", {{
    value: (channel, b) => {{
      const data = Uint8Array.from(atob(b), (c) => c.charCodeAt(0));
      listeners.forEach((f) => {{ try {{ f({{ channel, data }}); }} catch (e) {{ console.error(e); }} }});
    }},
    configurable: false, writable: false, enumerable: false,
  }});
  Object.defineProperty(window, "istmo", {{
    value: Object.freeze({{
      postMessage(channel, data) {{ send("post", channel, data).catch(() => {{}}); }},
      async invoke(channel, data) {{
        const r = await send("invoke", channel, data);
        const bytes = new Uint8Array(await r.arrayBuffer());
        if (r.headers.get("X-Istmo-Error")) throw new Error(new TextDecoder().decode(bytes));
        return bytes;
      }},
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

// =====================================================================
// QtView
// =====================================================================

impl QtView {
    fn destroy(&self) {
        let id = self.core.id;
        HOOKS.with(|h| h.borrow_mut().remove(&id));
        // SAFETY: Qt thread; the shim forgets the id.
        unsafe { (shim().istmo_qt_view_close)(id.0) };
    }
}

/// Run `source` and report its completion value: `{d}` value, `{e}`
/// error, `{p}` promise parked under `window[key]`.
fn eval_script(source: &str, key: &str) -> String {
    format!(
        r#"(() => {{
  const j = (x) => JSON.stringify(x === undefined ? null : x) ?? "null";
  const m = (e) => String(e && e.message || e);
  try {{
    const v = (0, eval)("{src}");
    if (v && typeof v.then === "function") {{
      const k = "{key}";
      Object.defineProperty(window, k, {{ value: {{ p: 1 }}, writable: true, configurable: true, enumerable: false }});
      Promise.resolve(v).then((x) => {{ window[k] = {{ d: j(x) }}; }}, (e) => {{ window[k] = {{ e: m(e) }}; }});
      return {{ p: 1 }};
    }}
    return {{ d: j(v) }};
  }} catch (e) {{
    return {{ e: m(e) }};
  }}
}})()"#,
        src = json_escape(source),
    )
}

fn poll_script(key: &str) -> String {
    format!(
        r#"(() => {{ const k = "{key}"; if (!(k in window)) return {{ e: "document replaced" }}; const r = window[k]; if (r.p) return {{ p: 1 }}; delete window[k]; return r; }})()"#
    )
}

fn eval_key(token: u64) -> String {
    format!("__istmoEval{token}")
}

impl UiView for QtView {
    fn load(&mut self, request: &LoadRequest) {
        let url = cstring(&request.url);
        let names: Vec<CString> = request.headers.iter().map(|h| cstring(&h.name)).collect();
        let values: Vec<CString> = request.headers.iter().map(|h| cstring(&h.value)).collect();
        let names_p: Vec<*const c_char> = names.iter().map(|c| c.as_ptr()).collect();
        let values_p: Vec<*const c_char> = values.iter().map(|c| c.as_ptr()).collect();
        let (body, body_len) = request
            .post_body
            .as_ref()
            .map_or((ptr::null(), 0), |b| (b.as_ptr(), b.len()));
        // SAFETY: Qt thread; every buffer outlives the call (Qt copies).
        unsafe {
            (shim().istmo_qt_view_load)(
                self.core.id.0,
                url.as_ptr(),
                names_p.as_ptr(),
                values_p.as_ptr(),
                names_p.len(),
                body,
                body_len,
            );
        }
    }

    fn load_html(&mut self, html: &str, base_url: Option<&str>) {
        hooks(self.core.id, |h| h.html_pending = true);
        let (h, b) = (cstring(html), base_url.map(cstring));
        // SAFETY: Qt thread; strings copied.
        unsafe {
            (shim().istmo_qt_view_load_html)(
                self.core.id.0,
                h.as_ptr(),
                b.as_ref().map_or(ptr::null(), |b| b.as_ptr()),
            );
        }
    }

    fn navigate(&mut self, nav: HistoryNav) {
        let (action, offset) = match nav {
            HistoryNav::Back => (0, 0),
            HistoryNav::Forward => (1, 0),
            HistoryNav::Reload => (2, 0),
            HistoryNav::ReloadBypassCache => (3, 0),
            HistoryNav::Stop => (4, 0),
            HistoryNav::Offset(n) => (5, n),
        };
        // SAFETY: Qt thread.
        unsafe { (shim().istmo_qt_view_navigate)(self.core.id.0, action, offset) };
    }

    fn history(&mut self) -> WebHistory {
        unsafe extern "C" fn item(out: *mut c_void, url: *const c_char, title: *const c_char) {
            // SAFETY: `out` is the `Vec` passed below, alive for the call.
            let entries = unsafe { &mut *out.cast::<Vec<HistoryEntry>>() };
            entries.push(HistoryEntry {
                url: text(url),
                title: text(title),
            });
        }
        let mut entries: Vec<HistoryEntry> = Vec::new();
        // SAFETY: Qt thread; `item` only touches `entries`.
        let current = unsafe {
            (shim().istmo_qt_view_history)(self.core.id.0, item, (&raw mut entries).cast())
        };
        WebHistory {
            entries,
            current: u32::try_from(current).unwrap_or(0),
        }
    }

    fn focus(&mut self) {
        // SAFETY: Qt thread.
        unsafe { (shim().istmo_qt_view_focus)(self.core.id.0) };
    }

    fn eval(&mut self, call: &ScriptCall, done: Done<String>) {
        let isolated = call.world == ScriptWorld::Isolated;
        let Some(token) = ui(|ui| {
            let token = ui.token();
            ui.evals.insert(
                token,
                PendingEval {
                    view: self.core.id,
                    isolated,
                    waited_ms: 0,
                    done,
                },
            );
            token
        }) else {
            return;
        };
        let script = cstring(&eval_script(&call.source, &eval_key(token)));
        // SAFETY: Qt thread; the answer arrives through `on_eval_done`.
        unsafe {
            (shim().istmo_qt_view_eval)(
                self.core.id.0,
                script.as_ptr(),
                c_int::from(isolated),
                token,
            );
        }
    }

    fn deliver(&mut self, channel: &str, data: &[u8]) {
        let script = cstring(&format!(
            "window.__istmoDeliver && window.__istmoDeliver(\"{}\", \"{}\"), {{ d: \"null\" }}",
            json_escape(channel),
            base64(data)
        ));
        // SAFETY: Qt thread; token 0 has no pending entry, the answer is
        // dropped.
        unsafe { (shim().istmo_qt_view_eval)(self.core.id.0, script.as_ptr(), 0, 0) };
    }

    fn cookies(&mut self, _filter: &CookieFilter, done: Done<Vec<WebCookie>>) {
        done(Err(WebViewError::Unsupported(
            "listing cookies on Qt WebEngine".into(),
        )));
    }

    fn set_cookie(&mut self, cookie: &WebCookie, done: Done<()>) {
        let host = cookie.domain.trim_start_matches('.');
        if host.is_empty() {
            return done(Err(WebViewError::InvalidArgument("cookie domain".into())));
        }
        let scheme = if cookie.secure { "https" } else { "http" };
        let url = cstring(&format!("{scheme}://{host}{}", cookie.path));
        let (name, value, domain, path) = (
            cstring(&cookie.name),
            cstring(&cookie.value),
            cstring(&cookie.domain),
            cstring(&cookie.path),
        );
        let same_site = match cookie.same_site {
            SameSite::None => 0,
            SameSite::Lax => 1,
            SameSite::Strict => 2,
        };
        // SAFETY: Qt thread; strings copied.
        unsafe {
            (shim().istmo_qt_view_set_cookie)(
                self.core.id.0,
                url.as_ptr(),
                name.as_ptr(),
                value.as_ptr(),
                domain.as_ptr(),
                path.as_ptr(),
                cookie.expires_unix_ms.unwrap_or(-1),
                c_int::from(cookie.secure),
                c_int::from(cookie.http_only),
                same_site,
            );
        }
        // Qt's cookie store has no completion signal.
        done(Ok(()));
    }

    fn clear_data(&mut self, request: &ClearDataRequest, done: Done<()>) {
        let has =
            |k: DataKind| request.kinds.contains(&k) || request.kinds.contains(&DataKind::All);
        let (cookies, cache) = (has(DataKind::Cookies), has(DataKind::Cache));
        // SAFETY: Qt thread.
        unsafe {
            (shim().istmo_qt_view_clear_data)(
                self.core.id.0,
                c_int::from(cookies),
                c_int::from(cache),
            );
        }
        let storage = has(DataKind::LocalStorage)
            || has(DataKind::IndexedDb)
            || has(DataKind::ServiceWorkers);
        if storage || request.since_unix_ms.is_some() {
            return done(Err(WebViewError::Unsupported(
                "Qt WebEngine clears only all cookies and the HTTP cache; remove the profile directory for the rest"
                    .into(),
            )));
        }
        done(Ok(()));
    }

    fn set_zoom(&mut self, factor: f32) {
        // SAFETY: Qt thread.
        unsafe { (shim().istmo_qt_view_zoom)(self.core.id.0, f64::from(factor)) };
    }

    fn find(&mut self, query: &FindQuery, done: Done<FindResult>) {
        let Some(token) = ui(|ui| {
            let token = ui.token();
            ui.finds.insert(token, done);
            token
        }) else {
            return;
        };
        let text = cstring(&query.text);
        // SAFETY: Qt thread; the answer arrives through `on_find_done`.
        unsafe {
            (shim().istmo_qt_view_find)(
                self.core.id.0,
                text.as_ptr(),
                c_int::from(query.backwards),
                c_int::from(query.case_sensitive),
                token,
            );
        }
    }

    fn snapshot(&mut self, options: SnapshotOptions, done: Done<WebSnapshot>) {
        if !matches!(options.format, SnapshotFormat::Png) {
            return done(Err(WebViewError::Unsupported(
                "JPEG snapshots on Qt WebEngine".into(),
            )));
        }
        let (mut len, mut width, mut height) = (0_usize, 0, 0);
        // SAFETY: Qt thread; the buffer is copied and freed once.
        let bytes = unsafe {
            let data = (shim().istmo_qt_view_snapshot)(
                self.core.id.0,
                &raw mut len,
                &raw mut width,
                &raw mut height,
            );
            if data.is_null() {
                return done(Err(WebViewError::Backend("snapshot failed".into())));
            }
            let bytes = std::slice::from_raw_parts(data, len).to_vec();
            (shim().istmo_qt_free)(data.cast());
            bytes
        };
        done(Ok(WebSnapshot {
            width: u32::try_from(width).unwrap_or(0),
            height: u32::try_from(height).unwrap_or(0),
            format: SnapshotFormat::Png,
            bytes,
        }));
    }

    fn print(&mut self) -> Result<(), WebViewError> {
        Err(WebViewError::Unsupported("printing on Qt WebEngine".into()))
    }

    fn set_inspector(&mut self, open: bool) -> Result<(), WebViewError> {
        if open && !self.core.config.allow_inspector {
            return Err(WebViewError::Unsupported(
                "the inspector is off (allow_inspector)".into(),
            ));
        }
        // SAFETY: Qt thread.
        match unsafe { (shim().istmo_qt_view_inspector)(self.core.id.0, c_int::from(open)) } {
            0 => Ok(()),
            _ => Err(WebViewError::ViewClosed),
        }
    }

    fn close(&mut self) {
        self.closed = true;
    }
}

// =====================================================================
// Callbacks (Qt thread)
// =====================================================================

/// Answer of a synchronous navigation check: 0 pending, 1 allow, 2 deny.
struct NavVerdict {
    value: AtomicU8,
    returned: AtomicBool,
}

unsafe extern "C" fn on_navigation(view: u64, url: *const c_char, gesture: c_int) -> c_int {
    let id = NativeHandleId(view);
    let Some(core) = core_of(view) else {
        return 0;
    };
    let url = text(url);
    if hooks(id, |h| h.approved.take_if(|a| *a == url))
        .flatten()
        .is_some()
    {
        return 1;
    }
    if url.starts_with("data:")
        && hooks(id, |h| std::mem::take(&mut h.html_pending)).unwrap_or(false)
    {
        return 1;
    }
    let verdict = Arc::new(NavVerdict {
        value: AtomicU8::new(0),
        returned: AtomicBool::new(false),
    });
    let late = Arc::clone(&verdict);
    let target = url.clone();
    // Qt cannot defer the answer: an `Ask` refuses now and reloads the
    // URL once the app allows it.
    core.navigation(
        &url,
        gesture != 0,
        Box::new(move |allow| {
            if !late.returned.load(Ordering::Acquire) {
                late.value
                    .store(if allow { 1 } else { 2 }, Ordering::Release);
                return;
            }
            if allow {
                hooks(id, |h| h.approved = Some(target.clone()));
                let url = cstring(&target);
                // SAFETY: resolved on the Qt thread.
                unsafe {
                    (shim().istmo_qt_view_load)(
                        id.0,
                        url.as_ptr(),
                        ptr::null(),
                        ptr::null(),
                        0,
                        ptr::null(),
                        0,
                    );
                }
            }
        }),
    );
    verdict.returned.store(true, Ordering::Release);
    if verdict.value.load(Ordering::Acquire) == 1 {
        1
    } else {
        hooks(id, |h| h.refused = Some(url));
        0
    }
}

unsafe extern "C" fn on_load_started(view: u64, url: *const c_char) {
    if let Some(core) = core_of(view) {
        core.started(&text(url));
    }
}

unsafe extern "C" fn on_committed(view: u64, url: *const c_char) {
    if let Some(core) = core_of(view) {
        core.committed(&text(url));
    }
}

unsafe extern "C" fn on_load_finished(
    view: u64,
    url: *const c_char,
    status: c_int,
    error_domain: c_int,
    error_code: c_int,
    error: *const c_char,
    back: c_int,
    forward: c_int,
) {
    let Some(core) = core_of(view) else {
        return;
    };
    let url = text(url);
    if status == 0 {
        return core.finished(&url, back != 0, forward != 0);
    }
    let refused = hooks(NativeHandleId(view), |h| h.refused.take_if(|r| *r == url))
        .flatten()
        .is_some();
    if refused {
        return;
    }
    // `QWebEngineLoadingInfo::ErrorDomain`, Chromium `net::Error` codes.
    let kind = match (status, error_domain, error_code) {
        (2, ..) | (_, _, -3) => NavErrorKind::Cancelled,
        (_, 3, _) => NavErrorKind::Tls,
        (_, 6, _) | (_, _, -105) => NavErrorKind::Dns,
        (_, _, -7 | -118) => NavErrorKind::Timeout,
        (_, 2, _) => NavErrorKind::Network,
        _ => NavErrorKind::Other,
    };
    core.failed(&url, kind, &text(error));
}

unsafe extern "C" fn on_title(view: u64, title: *const c_char) {
    if let Some(core) = core_of(view) {
        core.title(&text(title));
    }
}

unsafe extern "C" fn on_progress(view: u64, percent: c_int) {
    if let Some(core) = core_of(view) {
        core.progress(f64::from(percent) / 100.0);
    }
}

unsafe extern "C" fn on_close_requested(view: u64) {
    if let Some(core) = core_of(view) {
        core.emit(crate::ControlEvent::CloseRequested);
    }
}

unsafe extern "C" fn on_terminated(view: u64, status: c_int, _exit_code: c_int) {
    if let Some(core) = core_of(view) {
        core.fail_pending();
        // `RenderProcessTerminationStatus`: normal, abnormal, crashed,
        // killed.
        core.emit(crate::ControlEvent::ProcessTerminated(TerminationInfo {
            reason: match status {
                2 => TerminationReason::Crashed,
                3 => TerminationReason::Killed,
                _ => TerminationReason::Other,
            },
        }));
    }
}

/// Send a whole response for `token` (scheme or bridge request).
fn respond(token: u64, status: u16, mime: &str, headers: &[crate::HttpHeader], body: &[u8]) {
    head(token, status, mime, headers);
    // SAFETY: Qt thread; data copied.
    unsafe {
        (shim().istmo_qt_response_data)(token, body.as_ptr(), body.len());
        (shim().istmo_qt_response_end)(token, 0);
    }
}

fn head(token: u64, status: u16, mime: &str, headers: &[crate::HttpHeader]) {
    let mime = cstring(mime);
    let names: Vec<CString> = headers.iter().map(|h| cstring(&h.name)).collect();
    let values: Vec<CString> = headers.iter().map(|h| cstring(&h.value)).collect();
    let names_p: Vec<*const c_char> = names.iter().map(|c| c.as_ptr()).collect();
    let values_p: Vec<*const c_char> = values.iter().map(|c| c.as_ptr()).collect();
    // SAFETY: Qt thread; strings copied.
    unsafe {
        (shim().istmo_qt_response_head)(
            token,
            c_int::from(status),
            mime.as_ptr(),
            names_p.as_ptr(),
            values_p.as_ptr(),
            names_p.len(),
        );
    }
}

fn bridge_reply(token: u64, result: ReplyResult) {
    match result {
        ReplyResult::Ok(bytes) => respond(token, 200, "application/octet-stream", &[], &bytes),
        ReplyResult::Err(message) => respond(token, 500, "text/plain", &[], message.as_bytes()),
    }
}

unsafe extern "C" fn on_bridge(
    view: u64,
    token: u64,
    initiator: *const c_char,
    kind: *const c_char,
    channel: *const c_char,
    data: *const u8,
    len: usize,
) {
    let Some(core) = core_of(view) else {
        return bridge_reply(token, ReplyResult::Err("view closed".into()));
    };
    let payload = if data.is_null() || len == 0 {
        Vec::new()
    } else {
        // SAFETY: the shim passes `len` readable bytes for the call.
        unsafe { std::slice::from_raw_parts(data, len) }.to_vec()
    };
    // The origin comes from the browser process, not from the page. A
    // document whose origin is the top document's counts as the main
    // frame (Qt does not say which frame sent the request).
    let origin = text(initiator);
    let main = core.committed_origin().as_deref() == Some(origin.as_str());
    let document = core.document_id();
    match text(kind).as_str() {
        "invoke" => core.bridge_message(
            text(channel),
            origin,
            main,
            document,
            payload,
            Some(Box::new(move |result| bridge_reply(token, result))),
        ),
        "post" => {
            core.bridge_message(text(channel), origin, main, document, payload, None);
            respond(token, 204, "text/plain", &[], b"");
        }
        _ => bridge_reply(token, ReplyResult::Err("unknown bridge call".into())),
    }
}

unsafe extern "C" fn on_scheme(
    view: u64,
    token: u64,
    url: *const c_char,
    method: *const c_char,
    initiator: *const c_char,
    _has_body: c_int,
) {
    let Some(core) = core_of(view) else {
        return respond(token, 404, "text/plain", &[], b"");
    };
    let uri = text(url);
    let Ok(parsed) = url::Url::parse(&uri) else {
        return respond(token, 400, "text/plain", &[], b"");
    };
    let Some(scheme) = core
        .config
        .schemes
        .iter()
        .find(|s| s.name == parsed.scheme())
    else {
        return respond(token, 404, "text/plain", &[], b"");
    };
    // Only the scheme's own documents, or loads with no document behind
    // them, may read app content.
    let initiator = text(initiator);
    let own_origin = format!(
        "{}://{}",
        parsed.scheme(),
        parsed.host_str().unwrap_or_default()
    );
    let from_other = (!initiator.is_empty() && initiator != own_origin)
        || core.committed_origin().is_some_and(|o| o != own_origin);
    match &scheme.source {
        SchemeSource::Assets(source) => {
            if from_other {
                return respond(token, 403, "text/plain", &[], b"");
            }
            match crate::desktop::read_asset(&source.dir, parsed.path()) {
                Ok((mime, bytes)) => respond(token, 200, mime, &[], &bytes),
                Err(status) => respond(token, status, "text/plain", &[], b""),
            }
        }
        SchemeSource::Dynamic => core.scheme_request(
            SchemeRequestInfo {
                request_id: 0,
                scheme: scheme.name.clone(),
                method: text(method),
                url: uri,
                headers: Vec::new(),
                // Request bodies are not forwarded on this engine.
                has_body: false,
                initiator_origin: None,
            },
            Box::new(QtResponder { token }),
        ),
    }
}

/// Streams a dynamic response into the shim's chunk device.
struct QtResponder {
    token: u64,
}

impl SchemeResponder for QtResponder {
    fn head(&mut self, response: &SchemeResponseHead) {
        head(
            self.token,
            response.status,
            &response.mime_type,
            &response.headers,
        );
    }

    fn body(&mut self, body: SchemeBody) {
        // SAFETY: Qt thread (the core posts responder calls there).
        unsafe {
            match body {
                SchemeBody::Data(data) => {
                    (shim().istmo_qt_response_data)(self.token, data.as_ptr(), data.len());
                }
                SchemeBody::End => (shim().istmo_qt_response_end)(self.token, 0),
                SchemeBody::Abort(_) => (shim().istmo_qt_response_end)(self.token, 1),
            }
        }
    }
}

unsafe extern "C" fn on_permission(view: u64, token: u64, origin: *const c_char, kind: c_int) {
    let deny = || {
        // SAFETY: Qt thread.
        unsafe { (shim().istmo_qt_permission_reply)(token, 0) };
    };
    let Some(core) = core_of(view) else {
        return deny();
    };
    let permissions = match kind {
        0 => vec![WebPermission::Camera],
        1 => vec![WebPermission::Microphone],
        2 => vec![WebPermission::Camera, WebPermission::Microphone],
        3 => vec![WebPermission::Geolocation],
        4 => vec![WebPermission::Notifications],
        5 => vec![WebPermission::Clipboard],
        _ => vec![WebPermission::Other],
    };
    let frame_origin = text(origin);
    let wanted = permissions.clone();
    core.ask(
        DecisionKind::Permission(PermissionAsk {
            permissions,
            frame_origin,
            top_origin: core.committed_origin().unwrap_or_default(),
        }),
        Box::new(move |response| {
            let allow = match response {
                DecisionResponse::Allow => true,
                DecisionResponse::Grant(granted) => wanted.iter().all(|p| granted.contains(p)),
                _ => false,
            };
            // SAFETY: answered once on the Qt thread.
            unsafe { (shim().istmo_qt_permission_reply)(token, c_int::from(allow)) };
        }),
        None,
    );
}

unsafe extern "C" fn on_eval_done(token: u64, status: c_int, value: *const c_char) {
    if status != 2 {
        let Some(Some(pending)) = ui(|ui| ui.evals.remove(&token)) else {
            return;
        };
        let value = text(value);
        return (pending.done)(if status == 1 {
            Ok(value)
        } else {
            Err(WebViewError::Backend(value))
        });
    }
    // A promise: poll with backoff (2, 4, … 64 ms) until it settles.
    let Some(Some(delay)) = ui(|ui| {
        let pending = ui.evals.get_mut(&token)?;
        let delay = (2_u32 << (pending.waited_ms / 64).min(5)).min(64);
        pending.waited_ms += delay;
        Some(delay)
    }) else {
        return;
    };
    let expired = ui(|ui| {
        ui.evals
            .get(&token)
            .is_some_and(|p| p.waited_ms > EVAL_PROMISE_MS)
    })
    .unwrap_or(true);
    if expired {
        if let Some(Some(pending)) = ui(|ui| ui.evals.remove(&token)) {
            (pending.done)(Err(WebViewError::Timeout));
        }
        return;
    }
    let data = Box::into_raw(Box::new(token));
    // SAFETY: Qt thread; `poll_eval` takes `data` once.
    unsafe {
        (shim().istmo_qt_post_after)(c_int::try_from(delay).unwrap_or(64), poll_eval, data.cast());
    }
}

unsafe extern "C" fn poll_eval(data: *mut c_void) {
    // SAFETY: the `Box<u64>` from `on_eval_done`, taken once.
    let token = *unsafe { Box::from_raw(data.cast::<u64>()) };
    let Some(Some((view, isolated))) = ui(|ui| ui.evals.get(&token).map(|p| (p.view, p.isolated)))
    else {
        return;
    };
    let script = cstring(&poll_script(&eval_key(token)));
    // SAFETY: Qt thread; a closed view answers through `on_eval_done`.
    unsafe {
        (shim().istmo_qt_view_eval)(view.0, script.as_ptr(), c_int::from(isolated), token);
    }
}

unsafe extern "C" fn on_find_done(token: u64, matches: c_int, active: c_int) {
    if let Some(Some(done)) = ui(|ui| ui.finds.remove(&token)) {
        done(Ok(FindResult {
            matches: u32::try_from(matches).unwrap_or(0),
            active: u32::try_from(active).unwrap_or(0),
        }));
    }
}

unsafe extern "C" fn on_popup(opener: u64, url: *const c_char, _gesture: c_int) -> u64 {
    let Some(core) = core_of(opener) else {
        return 0;
    };
    let url = text(url);
    // Qt wants the new view now: the rules decide.
    if !core.allows_popup(&url) {
        return 0;
    }
    let mut spec = core.spec.clone();
    spec.target = RenderTarget::Window(crate::WindowTarget {
        title: String::new(),
        width: 0,
        height: 0,
        parent_window_id: None,
        modal: false,
        resizable: true,
    });
    let Some((child, instance)) = core.new_popup(spec) else {
        return 0;
    };
    let id = child.id;
    let created =
        ui(|ui| ui.create(child, Some(NativeHandleId(opener)))).is_some_and(|r| r.is_ok());
    if !created {
        return 0;
    }
    core.emit(crate::ControlEvent::NewView(NewViewInfo { instance, url }));
    id.0
}

// =====================================================================
// Process exit
// =====================================================================

unsafe extern "C" {
    fn atexit(callback: extern "C" fn()) -> c_int;
}

/// Signalled when the Qt event loop has returned.
static QUIT: std::sync::OnceLock<flume::Receiver<()>> = std::sync::OnceLock::new();

/// Exit hook: the process is going down while the Qt thread may still be
/// rendering; close the views and stop the loop before libraries
/// (GL drivers) unload under it.
extern "C" fn on_exit() {
    let (Some(shim), Some(done)) = (SHIM.get(), QUIT.get()) else {
        return;
    };
    if std::thread::current().name() == Some("istmo-webview-qt") {
        return;
    }
    // SAFETY: thread-safe entry point of the shim.
    unsafe { (shim.istmo_qt_quit)() };
    done.recv_timeout(std::time::Duration::from_secs(2)).ok();
}
