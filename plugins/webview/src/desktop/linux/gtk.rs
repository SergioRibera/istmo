//! `WebKitGTK` 4.1 (GTK 3) and 6.0 (GTK 4) engine on a dedicated GTK thread.
//!
//! The thread calls `gtk_init` and iterates the default `GMainContext`
//! (GTK and `WebKit` attach their sources there); every engine call is
//! posted to it with `g_idle_add_full`, which is thread-safe. Signal
//! handlers get the view id as user data and look the view up, so a
//! late signal never touches freed state.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_int, c_uint, c_void};
use std::io::Write as _;
use std::os::fd::IntoRawFd as _;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use istmo_core::NativeHandleId;
use istmo_core::sync::lock;

use super::ffi::{
    Api, FALSE, G_IO_ERR, G_IO_HUP, G_IO_OUT, G_TYPE_OBJECT, G_TYPE_STRING, GError, GList, GType,
    GValue, GdkRGBA, Libs, TRUE, gboolean, gpointer,
};
use super::x11::X11;
use crate::desktop::core::{Done, Engine, FrameSlot, SchemeResponder, UiTask, UiView, ViewCore};
use crate::present::FrameSink;
use crate::{
    ClearDataRequest, ContextMenuMode, CookieFilter, DataKind, DecisionKind, DecisionResponse,
    DialogAsk, DialogKind, FileChooserAsk, FindQuery, FindResult, HistoryEntry, HistoryNav,
    HttpAuthAsk, InjectionTime, LoadRequest, NavErrorKind, NewViewInfo, PermissionAsk,
    RenderTarget, ReplyResult, SameSite, SchemeBody, SchemeRequestInfo, SchemeResponseHead,
    SchemeSource, ScriptCall, ScriptWorld, SnapshotFormat, SnapshotOptions, TerminationInfo,
    TerminationReason, ViewFrame, WebCookie, WebHistory, WebPermission, WebProfile, WebSnapshot,
    WebViewCapabilities, WebViewError, WebViewInitConfig,
};

/// Lowest `WebKitGTK` the backend accepts (script message replies,
/// `evaluate_javascript`, response policy main-frame checks).
const MIN_VERSION: (u32, u32) = (2, 40);

/// Name of the page-visible message handler.
const NATIVE: &str = "__istmoNative";

/// Which `WebKitGTK` the engine loaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Flavor {
    /// `webkit2gtk-4.1` on GTK 3.
    Gtk3,
    /// `webkitgtk-6.0` on GTK 4: windows only.
    Gtk4,
}

/// `gtk_window_new` differs between GTK 3 (takes a type) and 4.
#[derive(Clone, Copy)]
enum WindowNew {
    Gtk3(unsafe extern "C" fn(c_int) -> gpointer),
    Gtk4(unsafe extern "C" fn() -> gpointer),
}

/// The engine: a handle to the GTK thread.
pub(crate) struct GtkEngine {
    api: &'static Api,
    flavor: Flavor,
    version: String,
    x11: Option<&'static X11>,
    frames: Mutex<HashMap<NativeHandleId, Arc<FrameState>>>,
}

/// Coalesces frames: the latest one is applied on the next idle turn.
struct FrameState {
    slot: FrameSlot,
    scheduled: AtomicBool,
}

struct GtkFrameSink {
    id: NativeHandleId,
    state: Arc<FrameState>,
    engine: std::sync::Weak<GtkEngine>,
}

impl FrameSink for GtkFrameSink {
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

/// Mutable state of the GTK thread.
struct Ui {
    api: &'static Api,
    flavor: Flavor,
    window_new: WindowNew,
    /// GTK 4: network sessions per persistent profile.
    sessions: HashMap<String, gpointer>,
    x11: Option<&'static X11>,
    views: HashMap<NativeHandleId, GtkView>,
    /// Web contexts per persistent profile name (`""` = default).
    contexts: HashMap<String, gpointer>,
    config: Option<Arc<WebViewInitConfig>>,
    sandbox: bool,
}

thread_local! {
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
}

fn ui<R>(f: impl FnOnce(&mut Ui) -> R) -> Option<R> {
    UI.with(|cell| cell.try_borrow_mut().ok()?.as_mut().map(f))
}

/// Run `f` with view `id`. Re-entrancy (a signal fired while another
/// call holds the table) returns `None` instead of panicking.
fn with<R>(id: NativeHandleId, f: impl FnOnce(&mut GtkView) -> R) -> Option<R> {
    ui(|ui| ui.views.get_mut(&id).map(f)).flatten()
}

fn api() -> &'static Api {
    API.get()
        .expect("invariant: the GTK thread loaded the API before any callback")
}

static API: std::sync::OnceLock<&'static Api> = std::sync::OnceLock::new();

fn cstring(s: &str) -> CString {
    CString::new(s.replace('\0', "")).unwrap_or_default()
}

/// Copy a C string (`""` for null).
fn text(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: non-null strings handed out by GLib / WebKit are
    // NUL-terminated and live for the duration of the call.
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

/// Copy and free a GLib-allocated string.
fn take_text(p: *mut c_char) -> String {
    let s = text(p);
    if !p.is_null() {
        // SAFETY: `p` was allocated by GLib and is not used again.
        unsafe { (api().g_free)(p.cast()) };
    }
    s
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "Linux desktop targets are 64-bit: the id travels intact as signal user data"
)]
const fn id_ptr(id: NativeHandleId) -> gpointer {
    id.0 as usize as gpointer
}

fn ptr_id(data: gpointer) -> NativeHandleId {
    NativeHandleId(data as usize as u64)
}

/// Connect `handler` (any C signature, cast to `GCallback`) with the
/// view id as user data.
macro_rules! connect {
    ($api:expr, $obj:expr, $signal:literal, $handler:expr, $id:expr) => {{
        // SAFETY: `$handler` has the C signature of `$signal` (checked
        // against the WebKitGTK reference); GObject calls it with the
        // same arguments, and the user data is a plain integer.
        #[allow(
            unused_unsafe,
            reason = "expanded both inside and outside unsafe blocks"
        )]
        unsafe {
            ($api.g_signal_connect_data)(
                $obj,
                concat!($signal, "\0").as_ptr().cast(),
                std::mem::transmute::<*const (), super::ffi::GCallback>($handler as *const ()),
                id_ptr($id),
                None,
                0,
            )
        }
    }};
}

impl GtkEngine {
    /// Start the GTK thread with `WebKitGTK` 4.1.
    pub(crate) fn start(flavor: Flavor, x11_embedding: bool) -> Result<Arc<Self>, WebViewError> {
        let (tx, rx) = flume::bounded(1);
        std::thread::Builder::new()
            .name("istmo-webview-gtk".into())
            .spawn(move || run_ui_thread(flavor, x11_embedding, &tx))
            .map_err(|e| WebViewError::Backend(format!("spawn GTK thread: {e}")))?;
        let (api, version, x11) = rx
            .recv()
            .map_err(|_| WebViewError::Backend("GTK thread exited".into()))??;
        let engine = Arc::new(Self {
            api,
            flavor,
            version,
            x11,
            frames: Mutex::default(),
        });
        remember(&engine);
        Ok(engine)
    }
}

type Started = Result<(&'static Api, String, Option<&'static X11>), WebViewError>;

fn run_ui_thread(flavor: Flavor, x11_embedding: bool, started: &flume::Sender<Started>) {
    let mut window_new = None;
    let mut setup = || -> Started {
        let libs = match flavor {
            Flavor::Gtk3 => Libs::open_webkit2gtk_41(),
            Flavor::Gtk4 => Libs::open_webkitgtk_60(),
        }
        .map_err(WebViewError::RuntimeMissing)?;
        let (init, new) = toolkit_entry_points(&libs, flavor)?;
        window_new = Some(new);
        let api: &'static Api = Box::leak(Box::new(
            Api::load(&libs)
                .map_err(WebViewError::RuntimeMissing)?
                .with_libs(libs),
        ));
        API.set(api).ok();
        FLAVOR.set(flavor).ok();
        // SAFETY: version getters take no arguments.
        let version = unsafe {
            (
                (api.webkit_get_major_version)(),
                (api.webkit_get_minor_version)(),
                (api.webkit_get_micro_version)(),
            )
        };
        if (version.0, version.1) < MIN_VERSION {
            return Err(WebViewError::RuntimeMissing(format!(
                "WebKitGTK {}.{} or newer (found {}.{}.{})",
                MIN_VERSION.0, MIN_VERSION.1, version.0, version.1, version.2
            )));
        }
        // SAFETY: plain GLib / GTK initialisation calls on this thread.
        unsafe {
            if (api.g_main_context_acquire)((api.g_main_context_default)()) == FALSE {
                return Err(WebViewError::ToolkitBusy);
            }
            if x11_embedding {
                // Embedding needs X11 windows even when the session is
                // Wayland (the app's window then lives on Xwayland).
                (api.gdk_set_allowed_backends)(c"x11".as_ptr());
            }
            if init() == FALSE {
                return Err(WebViewError::Backend(
                    "gtk_init failed (no display?)".into(),
                ));
            }
        }
        // SAFETY: GDK is initialised; the type check reads the display.
        let on_x11 = unsafe {
            let display = (api.gdk_display_get_default)();
            !display.is_null()
                && (api.g_type_check_instance_is_a)(display, (api.gdk_x11_display_get_type)())
                    != FALSE
        };
        let x11 = if on_x11 && flavor == Flavor::Gtk3 {
            X11::open().ok().map(|x| &*Box::leak(Box::new(x)))
        } else {
            None
        };
        Ok((
            api,
            format!("{}.{}.{}", version.0, version.1, version.2),
            x11,
        ))
    };
    let result = setup();
    let Some(window_new) = window_new else {
        drop(
            started
                .send(result.and_then(|_| Err(WebViewError::Backend("GTK did not start".into())))),
        );
        return;
    };
    let (api, x11) = if let Ok((api, _, x11)) = &result {
        (*api, *x11)
    } else {
        drop(started.send(result));
        return;
    };
    UI.with(|cell| {
        *cell.borrow_mut() = Some(Ui {
            api,
            flavor,
            window_new,
            sessions: HashMap::new(),
            x11,
            views: HashMap::new(),
            contexts: HashMap::new(),
            config: None,
            sandbox: true,
        });
    });
    drop(started.send(result));
    // SAFETY: the loop runs the default context this thread acquired.
    unsafe {
        let main_loop = (api.g_main_loop_new)(ptr::null_mut(), FALSE);
        (api.g_main_loop_run)(main_loop);
    }
}

unsafe extern "C" fn run_task(data: gpointer) -> gboolean {
    // SAFETY: `data` is the `Box<UiTask>` leaked by `post`, run once.
    let task = unsafe { Box::from_raw(data.cast::<UiTask>()) };
    task();
    FALSE
}

impl Engine for GtkEngine {
    fn describe(&self) -> (String, String) {
        let name = match self.flavor {
            Flavor::Gtk3 => "webkit2gtk-4.1",
            Flavor::Gtk4 => "webkitgtk-6.0",
        };
        (name.into(), self.version.clone())
    }

    fn capabilities(&self, caps: &mut WebViewCapabilities) {
        caps.windowed = true;
        caps.embedded = self.x11.is_some();
        caps.persistent_profiles = true;
        caps.ephemeral_profiles = true;
        caps.isolated_world = true;
        caps.bridge_reply = true;
        caps.deferred_navigation = true;
        caps.find = true;
        caps.print = true;
        caps.snapshot = true;
    }

    fn post(&self, task: UiTask) {
        let data = Box::into_raw(Box::new(task));
        // SAFETY: `g_idle_add_full` is thread-safe; `run_task` takes
        // ownership of `data` exactly once.
        unsafe { (self.api.g_idle_add_full)(0, run_task, data.cast(), None) };
    }

    fn post_after(self: Arc<Self>, ms: u32, task: UiTask) {
        let data = Box::into_raw(Box::new(task));
        // SAFETY: `g_timeout_add_full` is thread-safe and attaches to the
        // default context the GTK thread iterates; `run_task` takes `data`
        // once.
        unsafe {
            (self.api.g_timeout_add_full)(0, c_uint::from(ms), run_task, data.cast(), None);
        };
    }

    fn create(&self, core: Arc<ViewCore>, done: Done<()>) {
        self.post(Box::new(move || {
            let result = ui(|ui| ui.create(core))
                .unwrap_or_else(|| Err(WebViewError::Backend("GTK thread busy".into())));
            done(result);
        }));
    }

    fn with_view(&self, id: NativeHandleId, f: crate::desktop::core::ViewTask) {
        self.post(Box::new(move || {
            // Take the view out so engine callbacks fired from inside
            // `f` can still reach the table.
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
        Arc::new(GtkFrameSink {
            id,
            state,
            engine: GLOBAL.get().cloned().unwrap_or_default(),
        })
    }
}

/// Remember the engine for frame sinks and page-opened views.
fn remember(engine: &Arc<GtkEngine>) {
    GLOBAL.get_or_init(|| Arc::downgrade(engine));
}

static GLOBAL: std::sync::OnceLock<std::sync::Weak<GtkEngine>> = std::sync::OnceLock::new();

// =====================================================================
// View creation
// =====================================================================

/// A native view on the GTK thread.
struct GtkView {
    core: Arc<ViewCore>,
    web_view: gpointer,
    window: gpointer,
    embedded: bool,
    closed: bool,
    find_done: Option<Done<FindResult>>,
}

struct GValueBox(GValue);

impl GValueBox {
    fn object(api: &Api, obj: gpointer) -> Self {
        let mut v = Self(GValue {
            g_type: 0,
            data: [0; 2],
        });
        // SAFETY: a zeroed `GValue` initialised to `G_TYPE_OBJECT`.
        unsafe {
            (api.g_value_init)(&raw mut v.0, G_TYPE_OBJECT);
            (api.g_value_set_object)(&raw mut v.0, obj);
        }
        v
    }

    fn string(api: &Api, s: &str) -> Self {
        let mut v = Self(GValue {
            g_type: 0,
            data: [0; 2],
        });
        let c = cstring(s);
        // SAFETY: as above; GLib copies the string.
        unsafe {
            (api.g_value_init)(&raw mut v.0, G_TYPE_STRING);
            (api.g_value_set_string)(&raw mut v.0, c.as_ptr());
        }
        v
    }
}

/// `g_object_new` with `(name, value)` properties.
fn new_object(api: &Api, ty: GType, props: Vec<(&'static CStr, GValueBox)>) -> gpointer {
    let names: Vec<*const c_char> = props.iter().map(|(n, _)| n.as_ptr()).collect();
    let mut values: Vec<GValueBox> = props.into_iter().map(|(_, v)| v).collect();
    let raw: Vec<GValue> = values
        .iter()
        .map(|v| GValue {
            g_type: v.0.g_type,
            data: v.0.data,
        })
        .collect();
    let n = c_uint::try_from(names.len()).unwrap_or(0);
    // SAFETY: `names` and `raw` have `n` entries each and outlive the call.
    let obj = unsafe { (api.g_object_new_with_properties)(ty, n, names.as_ptr(), raw.as_ptr()) };
    for v in &mut values {
        // SAFETY: each value was initialised above.
        unsafe { (api.g_value_unset)(&raw mut v.0) };
    }
    obj
}

impl Ui {
    /// The view's web context and (GTK 4) network session.
    fn context(
        &mut self,
        profile: &WebProfile,
        config: &WebViewInitConfig,
    ) -> Result<(gpointer, Option<gpointer>), WebViewError> {
        match self.flavor {
            Flavor::Gtk3 => self.context_gtk3(profile, config).map(|ctx| (ctx, None)),
            Flavor::Gtk4 => {
                // One context (schemes are registered there); storage is
                // the network session.
                let ctx = self.shared_context(config);
                Ok((ctx, Some(self.session(profile)?)))
            }
        }
    }

    fn shared_context(&mut self, config: &WebViewInitConfig) -> gpointer {
        if let Some(ctx) = self.contexts.get("") {
            return *ctx;
        }
        // SAFETY: argument-less getter.
        let ctx = unsafe { (self.api.webkit_web_context_get_default)() };
        self.setup_context(ctx, config);
        self.contexts.insert(String::new(), ctx);
        ctx
    }

    fn session(&mut self, profile: &WebProfile) -> Result<gpointer, WebViewError> {
        let api = self.api;
        let missing = || WebViewError::RuntimeMissing("WebKitNetworkSession".into());
        // SAFETY: WebKitGTK 6.0 constructors / getters.
        unsafe {
            match profile {
                WebProfile::Shared => Ok(api
                    .webkit_network_session_get_default
                    .ok_or_else(missing)?()),
                WebProfile::Ephemeral => Ok(api
                    .webkit_network_session_new_ephemeral
                    .ok_or_else(missing)?()),
                WebProfile::Persistent(p) => {
                    if let Some(session) = self.sessions.get(&p.name) {
                        return Ok(*session);
                    }
                    let dir = super::profile_dir(&p.name)?;
                    let (data, cache) = (
                        cstring(&format!("{dir}/data")),
                        cstring(&format!("{dir}/cache")),
                    );
                    let session = api.webkit_network_session_new.ok_or_else(missing)?(
                        data.as_ptr(),
                        cache.as_ptr(),
                    );
                    self.sessions.insert(p.name.clone(), session);
                    Ok(session)
                }
            }
        }
    }

    fn context_gtk3(
        &mut self,
        profile: &WebProfile,
        config: &WebViewInitConfig,
    ) -> Result<gpointer, WebViewError> {
        let api = self.api;
        match profile {
            WebProfile::Ephemeral => {
                let new = api.webkit_web_context_new_ephemeral.ok_or_else(|| {
                    WebViewError::RuntimeMissing("webkit_web_context_new_ephemeral".into())
                })?;
                // SAFETY: argument-less constructor.
                let ctx = unsafe { new() };
                self.setup_context(ctx, config);
                Ok(ctx)
            }
            WebProfile::Shared => Ok(self.shared_context(config)),
            WebProfile::Persistent(p) => {
                if let Some(ctx) = self.contexts.get(&p.name) {
                    return Ok(*ctx);
                }
                let dir = super::profile_dir(&p.name)?;
                // SAFETY: type getter.
                let manager_type = unsafe { (api.webkit_website_data_manager_get_type)() };
                let manager = new_object(
                    api,
                    manager_type,
                    vec![
                        (
                            c"base-data-directory",
                            GValueBox::string(api, &format!("{dir}/data")),
                        ),
                        (
                            c"base-cache-directory",
                            GValueBox::string(api, &format!("{dir}/cache")),
                        ),
                    ],
                );
                // SAFETY: type getter.
                let context_type = unsafe { (api.webkit_web_context_get_type)() };
                let ctx = new_object(
                    api,
                    context_type,
                    vec![(c"website-data-manager", GValueBox::object(api, manager))],
                );
                // SAFETY: the context holds its own reference.
                unsafe { (api.g_object_unref)(manager) };
                self.setup_context(ctx, config);
                self.contexts.insert(p.name.clone(), ctx);
                Ok(ctx)
            }
        }
    }

    /// Sandbox (4.1; always on in 6.0) and custom schemes, before the
    /// context spawns a process.
    fn setup_context(&self, ctx: gpointer, config: &WebViewInitConfig) {
        let api = self.api;
        // SAFETY: `ctx` is a live `WebKitWebContext`.
        unsafe {
            if self.sandbox
                && let Some(enable) = api.webkit_web_context_set_sandbox_enabled
            {
                enable(ctx, TRUE);
            }
            let security = (api.webkit_web_context_get_security_manager)(ctx);
            for scheme in &config.schemes {
                let name = cstring(&scheme.name);
                (api.webkit_web_context_register_uri_scheme)(
                    ctx,
                    name.as_ptr(),
                    on_scheme_request,
                    ptr::null_mut(),
                    None,
                );
                (api.webkit_security_manager_register_uri_scheme_as_secure)(
                    security,
                    name.as_ptr(),
                );
                (api.webkit_security_manager_register_uri_scheme_as_cors_enabled)(
                    security,
                    name.as_ptr(),
                );
            }
        }
    }

    fn create(&mut self, core: Arc<ViewCore>) -> Result<(), WebViewError> {
        let api = self.api;
        let config = Arc::clone(&core.config);
        self.config.get_or_insert_with(|| Arc::clone(&config));
        let embedded = matches!(core.spec.target, RenderTarget::Embedded(_));
        if embedded && self.x11.is_none() {
            return Err(WebViewError::RenderModeUnsupported);
        }
        let (ctx, session) = self.context(&core.spec.profile, &config)?;
        // SAFETY: plain constructors and setters on objects created here.
        unsafe {
            let ucm = (api.webkit_user_content_manager_new)();
            install_scripts(api, ucm, &core);
            let mut props = vec![
                (c"web-context", GValueBox::object(api, ctx)),
                (c"user-content-manager", GValueBox::object(api, ucm)),
            ];
            if let Some(session) = session {
                props.push((c"network-session", GValueBox::object(api, session)));
            }
            let web_view = new_object(api, (api.webkit_web_view_get_type)(), props);
            (api.g_object_unref)(ucm);
            if matches!(core.spec.profile, WebProfile::Ephemeral) {
                // The view keeps its ephemeral context / session alive.
                (api.g_object_unref)(session.unwrap_or(ctx));
            }
            self.attach(core, web_view, embedded)
        }
    }

    /// Settings, signals and the container for a fresh `web_view`.
    fn attach(
        &mut self,
        core: Arc<ViewCore>,
        web_view: gpointer,
        embedded: bool,
    ) -> Result<(), WebViewError> {
        let api = self.api;
        let id = core.id;
        let s = &core.spec.settings;
        // SAFETY: `web_view` is a live `WebKitWebView`.
        unsafe {
            apply_settings(api, web_view, &core);
            connect_signals(
                api,
                self.flavor,
                web_view,
                id,
                s.context_menu == ContextMenuMode::Hidden,
            );
            let window = toolkit::window(self.window_new, embedded);
            if embedded {
                (api.gtk_window_set_decorated)(window, FALSE);
            } else {
                if let RenderTarget::Window(target) = &core.spec.target {
                    let title = cstring(&target.title);
                    (api.gtk_window_set_title)(window, title.as_ptr());
                    let w = if target.width == 0 {
                        800
                    } else {
                        c_int::try_from(target.width).unwrap_or(800)
                    };
                    let h = if target.height == 0 {
                        600
                    } else {
                        c_int::try_from(target.height).unwrap_or(600)
                    };
                    (api.gtk_window_set_default_size)(window, w, h);
                    (api.gtk_window_set_resizable)(window, i32::from(target.resizable));
                    (api.gtk_window_set_modal)(window, i32::from(target.modal));
                }
                match self.flavor {
                    Flavor::Gtk3 => {
                        connect!(
                            api,
                            window,
                            "delete-event",
                            on_delete
                                as unsafe extern "C" fn(gpointer, gpointer, gpointer) -> gboolean,
                            id
                        );
                    }
                    Flavor::Gtk4 => {
                        connect!(
                            api,
                            window,
                            "close-request",
                            on_close_request
                                as unsafe extern "C" fn(gpointer, gpointer) -> gboolean,
                            id
                        );
                    }
                }
            }
            toolkit::set_child(api, window, web_view);
            let view = GtkView {
                core,
                web_view,
                window,
                embedded,
                closed: false,
                find_done: None,
            };
            if embedded {
                (api.gtk_widget_realize)(window);
                view.reparent()?;
            } else {
                toolkit::show(api, window);
                (api.gtk_window_present)(window);
            }
            CORES.with(|c| c.borrow_mut().insert(id, Arc::clone(&view.core)));
            self.views.insert(id, view);
        }
        Ok(())
    }
}

/// Engine settings from the spec.
///
/// # Safety
/// `web_view` must be a live `WebKitWebView`.
unsafe fn apply_settings(api: &Api, web_view: gpointer, core: &ViewCore) {
    let s = &core.spec.settings;
    // SAFETY: forwarded precondition.
    unsafe {
        let settings = (api.webkit_web_view_get_settings)(web_view);
        (api.webkit_settings_set_enable_javascript)(settings, i32::from(s.javascript));
        (api.webkit_settings_set_enable_html5_local_storage)(settings, i32::from(s.dom_storage));
        if let Some(set) = api.webkit_settings_set_enable_html5_database {
            set(settings, i32::from(s.dom_storage));
        }
        (api.webkit_settings_set_media_playback_requires_user_gesture)(
            settings,
            i32::from(!s.media_autoplay),
        );
        (api.webkit_settings_set_allow_file_access_from_file_urls)(settings, FALSE);
        (api.webkit_settings_set_allow_universal_access_from_file_urls)(settings, FALSE);
        (api.webkit_settings_set_javascript_can_open_windows_automatically)(settings, FALSE);
        (api.webkit_settings_set_enable_developer_extras)(
            settings,
            i32::from(core.config.allow_inspector),
        );
        let ua = match (&s.user_agent, &core.config.user_agent_suffix) {
            (Some(ua), _) => Some(ua.clone()),
            (None, Some(suffix)) => Some(format!(
                "{} {suffix}",
                text((api.webkit_settings_get_user_agent)(settings))
            )),
            (None, None) => None,
        };
        if let Some(ua) = ua {
            let c = cstring(&ua);
            (api.webkit_settings_set_user_agent)(settings, c.as_ptr());
        }
        let bg = s.background;
        let rgba = GdkRGBA {
            red: f64::from(bg.r) / 255.0,
            green: f64::from(bg.g) / 255.0,
            blue: f64::from(bg.b) / 255.0,
            alpha: if s.transparent {
                0.0
            } else {
                f64::from(bg.a) / 255.0
            },
        };
        (api.webkit_web_view_set_background_color)(web_view, &raw const rgba);
    }
}

/// Every signal of a view, with its id as user data.
///
/// # Safety
/// `web_view` must be a live `WebKitWebView`.
#[expect(
    clippy::too_many_lines,
    reason = "a flat list, one connection per signal"
)]
unsafe fn connect_signals(
    api: &Api,
    flavor: Flavor,
    web_view: gpointer,
    id: NativeHandleId,
    hide_context_menu: bool,
) {
    let s_hidden = hide_context_menu;
    // SAFETY: forwarded precondition.
    unsafe {
        connect!(
            api,
            web_view,
            "decide-policy",
            on_decide_policy
                as unsafe extern "C" fn(gpointer, gpointer, c_int, gpointer) -> gboolean,
            id
        );
        connect!(
            api,
            web_view,
            "load-changed",
            on_load_changed as unsafe extern "C" fn(gpointer, c_int, gpointer),
            id
        );
        connect!(
            api,
            web_view,
            "load-failed",
            on_load_failed
                as unsafe extern "C" fn(
                    gpointer,
                    c_int,
                    *const c_char,
                    *mut GError,
                    gpointer,
                ) -> gboolean,
            id
        );
        connect!(
            api,
            web_view,
            "notify::estimated-load-progress",
            on_progress as unsafe extern "C" fn(gpointer, gpointer, gpointer),
            id
        );
        connect!(
            api,
            web_view,
            "notify::title",
            on_title as unsafe extern "C" fn(gpointer, gpointer, gpointer),
            id
        );
        connect!(
            api,
            web_view,
            "web-process-terminated",
            on_terminated as unsafe extern "C" fn(gpointer, c_int, gpointer),
            id
        );
        connect!(
            api,
            web_view,
            "close",
            on_close as unsafe extern "C" fn(gpointer, gpointer),
            id
        );
        connect!(
            api,
            web_view,
            "create",
            on_create as unsafe extern "C" fn(gpointer, gpointer, gpointer) -> gpointer,
            id
        );
        connect!(
            api,
            web_view,
            "permission-request",
            on_permission as unsafe extern "C" fn(gpointer, gpointer, gpointer) -> gboolean,
            id
        );
        connect!(
            api,
            web_view,
            "script-dialog",
            on_script_dialog as unsafe extern "C" fn(gpointer, gpointer, gpointer) -> gboolean,
            id
        );
        connect!(
            api,
            web_view,
            "authenticate",
            on_authenticate as unsafe extern "C" fn(gpointer, gpointer, gpointer) -> gboolean,
            id
        );
        connect!(
            api,
            web_view,
            "run-file-chooser",
            on_file_chooser as unsafe extern "C" fn(gpointer, gpointer, gpointer) -> gboolean,
            id
        );
        if s_hidden {
            match flavor {
                Flavor::Gtk3 => {
                    connect!(
                        api,
                        web_view,
                        "context-menu",
                        on_context_menu
                            as unsafe extern "C" fn(
                                gpointer,
                                gpointer,
                                gpointer,
                                gpointer,
                                gpointer,
                            ) -> gboolean,
                        id
                    );
                }
                Flavor::Gtk4 => {
                    connect!(
                        api,
                        web_view,
                        "context-menu",
                        on_context_menu4
                            as unsafe extern "C" fn(
                                gpointer,
                                gpointer,
                                gpointer,
                                gpointer,
                            ) -> gboolean,
                        id
                    );
                }
            }
        }
        let find = (api.webkit_web_view_get_find_controller)(web_view);
        connect!(
            api,
            find,
            "found-text",
            on_found_text as unsafe extern "C" fn(gpointer, c_uint, gpointer),
            id
        );
        connect!(
            api,
            find,
            "failed-to-find-text",
            on_find_failed as unsafe extern "C" fn(gpointer, gpointer),
            id
        );
    }
}

/// User scripts and the bridge shim, limited to their origins.
///
/// # Safety
/// `ucm` must be a live `WebKitUserContentManager`.
unsafe fn install_scripts(api: &Api, ucm: gpointer, core: &ViewCore) {
    let patterns = |origins: &[String]| -> Vec<CString> {
        origins.iter().map(|o| cstring(&format!("{o}/*"))).collect()
    };
    let add = |source: &str, top_only: bool, start: bool, allow: &[CString]| {
        let src = cstring(source);
        let mut list: Vec<*const c_char> = allow.iter().map(|c| c.as_ptr()).collect();
        list.push(ptr::null());
        let allow_ptr = if allow.is_empty() {
            ptr::null()
        } else {
            list.as_ptr()
        };
        // SAFETY: NUL-terminated strings and a NULL-terminated list that
        // outlive the call (WebKit copies them).
        unsafe {
            let script = (api.webkit_user_script_new)(
                src.as_ptr(),
                i32::from(top_only),
                i32::from(!start),
                allow_ptr,
                ptr::null(),
            );
            (api.webkit_user_content_manager_add_script)(ucm, script);
            (api.webkit_user_script_unref)(script);
        }
    };
    for script in &core.spec.user_scripts {
        let allow = patterns(&script.allowed_origins);
        add(
            &script.source,
            script.main_frame_only,
            script.inject_at == InjectionTime::DocumentStart,
            &allow,
        );
    }
    if let Some(bridge) = &core.spec.bridge {
        if core.bridge_origins().is_empty() {
            return;
        }
        let name = cstring(NATIVE);
        // SAFETY: registers a handler in the page world.
        unsafe {
            (api.webkit_user_content_manager_register_script_message_handler_with_reply)(
                ucm,
                name.as_ptr(),
                ptr::null(),
            );
            connect!(
                api,
                ucm,
                "script-message-with-reply-received::__istmoNative",
                on_bridge_message
                    as unsafe extern "C" fn(gpointer, gpointer, gpointer, gpointer) -> gboolean,
                core.id
            );
        }
        let allow = patterns(core.bridge_origins());
        add(
            &shim(bridge.allow_subframes),
            !bridge.allow_subframes,
            true,
            &allow,
        );
    }
}

/// Page side: `window.istmo` over the reply-capable message handler.
/// The page reports its `location.origin`; the engine side checks it
/// against the committed origin (`WebKitGTK` does not report senders).
fn shim(subframes: bool) -> String {
    format!(
        r#"(() => {{
  const handler = window.webkit && window.webkit.messageHandlers && window.webkit.messageHandlers.{NATIVE};
  if (!handler || window.istmo{top}) return;
  const listeners = new Set();
  const toB64 = (d) => {{
    const bytes = typeof d === "string" ? new TextEncoder().encode(d) : d instanceof Uint8Array ? d : new Uint8Array(d);
    let bin = "";
    for (let i = 0; i < bytes.length; i += 0x8000) bin += String.fromCharCode.apply(null, bytes.subarray(i, i + 0x8000));
    return btoa(bin);
  }};
  const fromB64 = (b) => Uint8Array.from(atob(b), (c) => c.charCodeAt(0));
  const send = (t, channel, data) => handler.postMessage(JSON.stringify({{ t, c: String(channel), b: toB64(data ?? ""), o: location.origin, m: window === window.top }}));
  Object.defineProperty(window, "__istmoDeliver", {{
    value: (channel, b) => listeners.forEach((f) => {{ try {{ f({{ channel, data: fromB64(b) }}); }} catch (e) {{ console.error(e); }} }}),
    configurable: false, writable: false, enumerable: false,
  }});
  Object.defineProperty(window, "istmo", {{
    value: Object.freeze({{
      postMessage(channel, data) {{ send("m", channel, data); }},
      invoke(channel, data) {{ return send("i", channel, data).then((b) => fromB64(b)); }},
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
// GtkView
// =====================================================================

/// XID of a realised GTK 3 window and GDK's X display.
fn x11_ids(window: gpointer) -> Option<(std::ffi::c_ulong, gpointer)> {
    let api = api();
    let (get_window, xid_of, xdisplay) = (
        api.gtk_widget_get_window?,
        api.gdk_x11_window_get_xid?,
        api.gdk_x11_display_get_xdisplay?,
    );
    // SAFETY: GTK 3 getters on a live, realised window.
    unsafe {
        let gdk_window = get_window(window);
        if gdk_window.is_null() {
            return None;
        }
        Some((
            xid_of(gdk_window),
            xdisplay((api.gdk_display_get_default)()),
        ))
    }
}

impl GtkView {
    /// Put the popup window inside the app's registered X11 window.
    fn reparent(&self) -> Result<(), WebViewError> {
        let RenderTarget::Embedded(target) = &self.core.spec.target else {
            return Ok(());
        };
        let parent = istmo_window::WindowRegistry::global()
            .get(istmo_window::WindowId(target.window_id))
            .ok_or_else(|| {
                WebViewError::InvalidArgument(format!(
                    "window {} is not registered",
                    target.window_id
                ))
            })?;
        let parent_xid = match parent {
            istmo_window::NativeWindow::Xlib { window, .. } => window.get(),
            istmo_window::NativeWindow::Xcb { window, .. } => std::ffi::c_ulong::from(window.get()),
            _ => return Err(WebViewError::RenderModeUnsupported),
        };
        let x11 = ui(|ui| ui.x11)
            .flatten()
            .ok_or(WebViewError::RenderModeUnsupported)?;
        let (xid, display) = x11_ids(self.window).ok_or(WebViewError::RenderModeUnsupported)?;
        // SAFETY: X calls on GDK's own display for our realised window.
        unsafe {
            (x11.XReparentWindow)(display, xid, parent_xid, 0, 0);
            (x11.XFlush)(display);
        }
        Ok(())
    }

    fn apply_frame(&mut self, frame: ViewFrame) {
        if !self.embedded {
            return;
        }
        let api = api();
        let Some(x11) = ui(|ui| ui.x11).flatten() else {
            return;
        };
        // SAFETY: live window; X calls on GDK's display.
        unsafe {
            if !frame.visible || frame.width == 0 || frame.height == 0 {
                toolkit::hide(api, self.window);
                return;
            }
            toolkit::show(api, self.window);
            let Some((xid, display)) = x11_ids(self.window) else {
                return;
            };
            if let Some(resize) = api.gtk_window_resize {
                let w = c_int::try_from(frame.width).unwrap_or(c_int::MAX);
                let h = c_int::try_from(frame.height).unwrap_or(c_int::MAX);
                resize(self.window, w, h);
            }
            (x11.XMoveResizeWindow)(display, xid, frame.x, frame.y, frame.width, frame.height);
            (x11.XFlush)(display);
        }
    }

    fn destroy(&mut self) {
        let api = api();
        CORES.with(|c| c.borrow_mut().remove(&self.core.id));
        // SAFETY: destroying the toplevel destroys the web view inside it.
        unsafe {
            (api.webkit_web_view_stop_loading)(self.web_view);
            toolkit::destroy(api, self.window);
        }
    }

    fn eval_in(&self, source: &str, world: Option<&str>, done: Option<EvalDone>) {
        let api = api();
        let src = cstring(source);
        let world = world.map(cstring);
        let data = done.map_or(ptr::null_mut(), |d| {
            Box::into_raw(Box::new(d)).cast::<c_void>()
        });
        // SAFETY: strings outlive the call; `on_eval_done` takes `data`.
        unsafe {
            (api.webkit_web_view_evaluate_javascript)(
                self.web_view,
                src.as_ptr(),
                -1,
                world.as_ref().map_or(ptr::null(), |w| w.as_ptr()),
                ptr::null(),
                ptr::null_mut(),
                if data.is_null() {
                    None
                } else {
                    Some(on_eval_done)
                },
                data,
            );
        }
    }
}

type EvalDone = Box<dyn FnOnce(Result<String, WebViewError>)>;

unsafe extern "C" fn on_call_done(source: gpointer, result: gpointer, data: gpointer) {
    // SAFETY: `data` is the `Box<EvalDone>` from `eval`.
    let done = unsafe { Box::from_raw(data.cast::<EvalDone>()) };
    let mut error: *mut GError = ptr::null_mut();
    // SAFETY: finish call for the matching async call.
    let value = unsafe {
        (api().webkit_web_view_call_async_javascript_function_finish)(
            source,
            result,
            &raw mut error,
        )
    };
    js_result(*done, value, error);
}

unsafe extern "C" fn on_eval_done(source: gpointer, result: gpointer, data: gpointer) {
    // SAFETY: `data` is the `Box<EvalDone>` from `eval_in`.
    let done = unsafe { Box::from_raw(data.cast::<EvalDone>()) };
    let api = api();
    let mut error: *mut GError = ptr::null_mut();
    // SAFETY: finish call for the matching async call.
    let value =
        unsafe { (api.webkit_web_view_evaluate_javascript_finish)(source, result, &raw mut error) };
    js_result(*done, value, error);
}

fn js_result(done: EvalDone, value: gpointer, error: *mut GError) {
    let api = api();
    if value.is_null() {
        // SAFETY: on failure GLib set `error`.
        let message = unsafe { error.as_ref().map(|e| text(e.message)).unwrap_or_default() };
        if !error.is_null() {
            // SAFETY: owned error, freed once.
            unsafe { (api.g_error_free)(error) };
        }
        return done(Err(WebViewError::Backend(message)));
    }
    // SAFETY: `value` is a live `JSCValue`.
    let json = unsafe { take_text((api.jsc_value_to_json)(value, 0)) };
    // SAFETY: the finish call returned a new reference.
    unsafe { (api.g_object_unref)(value) };
    done(Ok(if json.is_empty() { "null".into() } else { json }));
}

impl UiView for GtkView {
    fn load(&mut self, request: &LoadRequest) {
        let api = api();
        let url = cstring(&request.url);
        // SAFETY: live view; WebKit copies the URI and headers.
        unsafe {
            if request.headers.is_empty() && request.post_body.is_none() {
                (api.webkit_web_view_load_uri)(self.web_view, url.as_ptr());
            } else {
                let req = (api.webkit_uri_request_new)(url.as_ptr());
                let headers = (api.webkit_uri_request_get_http_headers)(req);
                if !headers.is_null() {
                    for h in &request.headers {
                        let (n, v) = (cstring(&h.name), cstring(&h.value));
                        (api.soup_message_headers_append)(headers, n.as_ptr(), v.as_ptr());
                    }
                }
                // WebKitGTK has no public POST load: the body is dropped.
                (api.webkit_web_view_load_request)(self.web_view, req);
                (api.g_object_unref)(req);
            }
        }
    }

    fn load_html(&mut self, html: &str, base_url: Option<&str>) {
        let api = api();
        let (h, b) = (cstring(html), base_url.map(cstring));
        // SAFETY: live view; strings copied.
        unsafe {
            (api.webkit_web_view_load_html)(
                self.web_view,
                h.as_ptr(),
                b.as_ref().map_or(ptr::null(), |b| b.as_ptr()),
            );
        };
    }

    fn navigate(&mut self, nav: HistoryNav) {
        let api = api();
        let v = self.web_view;
        // SAFETY: live view.
        unsafe {
            match nav {
                HistoryNav::Back => (api.webkit_web_view_go_back)(v),
                HistoryNav::Forward => (api.webkit_web_view_go_forward)(v),
                HistoryNav::Reload => (api.webkit_web_view_reload)(v),
                HistoryNav::ReloadBypassCache => (api.webkit_web_view_reload_bypass_cache)(v),
                HistoryNav::Stop => (api.webkit_web_view_stop_loading)(v),
                HistoryNav::Offset(n) => {
                    let list = (api.webkit_web_view_get_back_forward_list)(v);
                    let item = (api.webkit_back_forward_list_get_nth_item)(list, n);
                    if !item.is_null() {
                        (api.webkit_web_view_go_to_back_forward_list_item)(v, item);
                    }
                }
            }
        }
    }

    fn history(&mut self) -> WebHistory {
        let api = api();
        let entry = |item: gpointer| HistoryEntry {
            // SAFETY: items of the view's live list.
            url: text(unsafe { (api.webkit_back_forward_list_item_get_uri)(item) }),
            title: text(unsafe { (api.webkit_back_forward_list_item_get_title)(item) }),
        };
        let collect = |list: *mut GList| {
            let mut out = Vec::new();
            let mut node = list;
            while !node.is_null() {
                // SAFETY: walking a GList returned by WebKit.
                let n = unsafe { &*node };
                out.push(entry(n.data));
                node = n.next;
            }
            if !list.is_null() {
                // SAFETY: the container list is ours to free.
                unsafe { (api.g_list_free)(list) };
            }
            out
        };
        // SAFETY: live view and list.
        unsafe {
            let list = (api.webkit_web_view_get_back_forward_list)(self.web_view);
            let mut back = collect((api.webkit_back_forward_list_get_back_list)(list));
            back.reverse();
            let current = u32::try_from(back.len()).unwrap_or(u32::MAX);
            let cur = (api.webkit_back_forward_list_get_current_item)(list);
            if !cur.is_null() {
                back.push(entry(cur));
            }
            back.extend(collect((api.webkit_back_forward_list_get_forward_list)(
                list,
            )));
            WebHistory {
                entries: back,
                current,
            }
        }
    }

    fn focus(&mut self) {
        // SAFETY: live widget.
        unsafe { (api().gtk_widget_grab_focus)(self.web_view) };
    }

    fn eval(&mut self, call: &ScriptCall, done: Done<String>) {
        // A function body: promises are awaited, the result comes back
        // as a JS value turned into JSON.
        let body = cstring(&format!(
            "return await eval(\"{}\");",
            crate::desktop::text::json_escape(&call.source)
        ));
        let world = (call.world == ScriptWorld::Isolated).then(|| cstring("istmo"));
        let state: Box<EvalDone> = Box::new(Box::new(done));
        let api = api();
        // SAFETY: strings outlive the call; `on_call_done` takes `state`.
        unsafe {
            (api.webkit_web_view_call_async_javascript_function)(
                self.web_view,
                body.as_ptr(),
                -1,
                ptr::null_mut(),
                world.as_ref().map_or(ptr::null(), |w| w.as_ptr()),
                ptr::null(),
                ptr::null_mut(),
                Some(on_call_done),
                Box::into_raw(state).cast(),
            );
        }
    }

    fn deliver(&mut self, channel: &str, data: &[u8]) {
        let script = format!(
            "window.__istmoDeliver && window.__istmoDeliver(\"{}\", \"{}\");",
            crate::desktop::text::json_escape(channel),
            crate::desktop::text::base64(data)
        );
        self.eval_in(&script, None, None);
    }

    fn cookies(&mut self, filter: &CookieFilter, done: Done<Vec<WebCookie>>) {
        let api = api();
        let Some(url) = filter.url.clone() else {
            return done(Err(WebViewError::Unsupported(
                "listing cookies needs a URL on WebKitGTK".into(),
            )));
        };
        let name = filter.name.clone();
        let c = cstring(&url);
        // SAFETY: live view.
        let Some(manager) = (unsafe { cookie_manager(api, self.web_view) }) else {
            return done(Err(WebViewError::Unsupported("cookie manager".into())));
        };
        let state: Box<CookieDone> = Box::new((name, done));
        // SAFETY: live view; `on_cookies` takes `state`.
        unsafe {
            (api.webkit_cookie_manager_get_cookies)(
                manager,
                c.as_ptr(),
                ptr::null_mut(),
                Some(on_cookies),
                Box::into_raw(state).cast(),
            );
        }
    }

    fn set_cookie(&mut self, cookie: &WebCookie, done: Done<()>) {
        let api = api();
        let max_age = cookie.expires_unix_ms.map_or(-1, |ms| {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
            c_int::try_from((ms - now) / 1000)
                .unwrap_or(c_int::MAX)
                .max(0)
        });
        let (n, v, d, p) = (
            cstring(&cookie.name),
            cstring(&cookie.value),
            cstring(&cookie.domain),
            cstring(if cookie.path.is_empty() {
                "/"
            } else {
                &cookie.path
            }),
        );
        // SAFETY: libsoup copies the strings; WebKit copies the cookie.
        unsafe {
            let soup =
                (api.soup_cookie_new)(n.as_ptr(), v.as_ptr(), d.as_ptr(), p.as_ptr(), max_age);
            (api.soup_cookie_set_secure)(soup, i32::from(cookie.secure));
            (api.soup_cookie_set_http_only)(soup, i32::from(cookie.http_only));
            (api.soup_cookie_set_same_site_policy)(
                soup,
                match cookie.same_site {
                    SameSite::None => 0,
                    SameSite::Lax => 1,
                    SameSite::Strict => 2,
                },
            );
            let Some(manager) = cookie_manager(api, self.web_view) else {
                (api.soup_cookie_free)(soup);
                return done(Err(WebViewError::Unsupported("cookie manager".into())));
            };
            let state: Box<UnitDone> = Box::new(done);
            (api.webkit_cookie_manager_add_cookie)(
                manager,
                soup,
                ptr::null_mut(),
                Some(on_cookie_added),
                Box::into_raw(state).cast(),
            );
            (api.soup_cookie_free)(soup);
        }
    }

    fn clear_data(&mut self, request: &ClearDataRequest, done: Done<()>) {
        let api = api();
        // WebKitWebsiteDataTypes bits.
        let mut types = 0;
        for kind in &request.kinds {
            types |= match kind {
                DataKind::Cookies => 1 << 8,
                DataKind::Cache => (1 << 0) | (1 << 1) | (1 << 2),
                DataKind::LocalStorage => (1 << 4) | (1 << 3),
                DataKind::IndexedDb => 1 << 6,
                DataKind::ServiceWorkers => 1 << 11,
                DataKind::All => 0x3fff,
            };
        }
        let since = request.since_unix_ms.map_or(0, |ms| {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| i64::try_from(d.as_micros()).unwrap_or(i64::MAX));
            (now - ms * 1000).max(0)
        });
        // SAFETY: live view.
        let Some(manager) = (unsafe { data_manager(api, self.web_view) }) else {
            return done(Err(WebViewError::Unsupported(
                "website data manager".into(),
            )));
        };
        let state: Box<UnitDone> = Box::new(done);
        // SAFETY: live context; `on_cleared` takes `state`.
        unsafe {
            (api.webkit_website_data_manager_clear)(
                manager,
                types,
                since,
                ptr::null_mut(),
                Some(on_cleared),
                Box::into_raw(state).cast(),
            );
        }
    }

    fn set_zoom(&mut self, factor: f32) {
        // SAFETY: live view.
        unsafe { (api().webkit_web_view_set_zoom_level)(self.web_view, f64::from(factor)) };
    }

    fn find(&mut self, query: &FindQuery, done: Done<FindResult>) {
        let api = api();
        // SAFETY: live view and find controller.
        unsafe {
            let fc = (api.webkit_web_view_get_find_controller)(self.web_view);
            if query.text.is_empty() {
                (api.webkit_find_controller_search_finish)(fc);
                return done(Ok(FindResult::default()));
            }
            // WebKitFindOptions: CASE_INSENSITIVE 1, BACKWARDS 8, WRAP_AROUND 16.
            let mut options = 0;
            if !query.case_sensitive {
                options |= 1;
            }
            if query.backwards {
                options |= 8;
            }
            if query.wrap {
                options |= 16;
            }
            if let Some(previous) = self.find_done.replace(done) {
                previous(Ok(FindResult::default()));
            }
            let t = cstring(&query.text);
            (api.webkit_find_controller_search)(fc, t.as_ptr(), options, 1000);
        }
    }

    fn snapshot(&mut self, options: SnapshotOptions, done: Done<WebSnapshot>) {
        if !matches!(options.format, SnapshotFormat::Png) {
            return done(Err(WebViewError::Unsupported(
                "JPEG snapshots on WebKitGTK".into(),
            )));
        }
        let state: Box<SnapshotDone> = Box::new(done);
        // SAFETY: live view; region VISIBLE (0), options NONE (0).
        unsafe {
            (api().webkit_web_view_get_snapshot)(
                self.web_view,
                0,
                0,
                ptr::null_mut(),
                Some(on_snapshot),
                Box::into_raw(state).cast(),
            );
        };
    }

    fn print(&mut self) -> Result<(), WebViewError> {
        let api = api();
        // SAFETY: live view and window.
        unsafe {
            let op = (api.webkit_print_operation_new)(self.web_view);
            (api.webkit_print_operation_run_dialog)(op, self.window);
            (api.g_object_unref)(op);
        }
        Ok(())
    }

    fn set_inspector(&mut self, open: bool) -> Result<(), WebViewError> {
        let api = api();
        // SAFETY: live view.
        unsafe {
            let inspector = (api.webkit_web_view_get_inspector)(self.web_view);
            if open {
                (api.webkit_web_inspector_show)(inspector);
            } else {
                (api.webkit_web_inspector_close)(inspector);
            }
        }
        Ok(())
    }

    fn close(&mut self) {
        self.closed = true;
    }
}

/// Cookie manager of the view's storage: its web context (4.1) or
/// network session (6.0).
///
/// # Safety
/// `web_view` must be a live `WebKitWebView`.
unsafe fn cookie_manager(api: &Api, web_view: gpointer) -> Option<gpointer> {
    // SAFETY: forwarded precondition; getters of the loaded version.
    unsafe {
        if let (Some(session_of), Some(get)) = (
            api.webkit_web_view_get_network_session,
            api.webkit_network_session_get_cookie_manager,
        ) {
            return Some(get(session_of(web_view)));
        }
        api.webkit_web_context_get_cookie_manager
            .map(|get| get((api.webkit_web_view_get_context)(web_view)))
    }
}

/// Website data manager of the view's storage.
///
/// # Safety
/// `web_view` must be a live `WebKitWebView`.
unsafe fn data_manager(api: &Api, web_view: gpointer) -> Option<gpointer> {
    // SAFETY: forwarded precondition; getters of the loaded version.
    unsafe {
        if let (Some(session_of), Some(get)) = (
            api.webkit_web_view_get_network_session,
            api.webkit_network_session_get_website_data_manager,
        ) {
            return Some(get(session_of(web_view)));
        }
        api.webkit_web_context_get_website_data_manager
            .map(|get| get((api.webkit_web_view_get_context)(web_view)))
    }
}

type CookieDone = (Option<String>, Done<Vec<WebCookie>>);
type UnitDone = Done<()>;
type SnapshotDone = Done<WebSnapshot>;

unsafe extern "C" fn on_cookies(source: gpointer, result: gpointer, data: gpointer) {
    // SAFETY: `data` is the `Box<CookieDone>` passed with the call.
    let (name, done) = *unsafe { Box::from_raw(data.cast::<CookieDone>()) };
    let api = api();
    let mut error: *mut GError = ptr::null_mut();
    // SAFETY: matching finish call.
    let list =
        unsafe { (api.webkit_cookie_manager_get_cookies_finish)(source, result, &raw mut error) };
    if !error.is_null() {
        // SAFETY: owned error.
        let message = unsafe { text((*error).message) };
        // SAFETY: freed once.
        unsafe { (api.g_error_free)(error) };
        return done(Err(WebViewError::Backend(message)));
    }
    let mut out = Vec::new();
    let mut node = list;
    while !node.is_null() {
        // SAFETY: a GList of owned `SoupCookie`s.
        let n = unsafe { &*node };
        let c = n.data;
        // SAFETY: live cookie.
        let cookie = unsafe {
            let expires = (api.soup_cookie_get_expires)(c);
            WebCookie {
                name: text((api.soup_cookie_get_name)(c)),
                value: text((api.soup_cookie_get_value)(c)),
                domain: text((api.soup_cookie_get_domain)(c)),
                path: text((api.soup_cookie_get_path)(c)),
                expires_unix_ms: (!expires.is_null())
                    .then(|| (api.g_date_time_to_unix)(expires) * 1000),
                secure: (api.soup_cookie_get_secure)(c) != FALSE,
                http_only: (api.soup_cookie_get_http_only)(c) != FALSE,
                same_site: match (api.soup_cookie_get_same_site_policy)(c) {
                    2 => SameSite::Strict,
                    1 => SameSite::Lax,
                    _ => SameSite::None,
                },
            }
        };
        // SAFETY: each cookie is ours to free.
        unsafe { (api.soup_cookie_free)(c) };
        if name.as_ref().is_none_or(|want| *want == cookie.name) {
            out.push(cookie);
        }
        node = n.next;
    }
    if !list.is_null() {
        // SAFETY: the list container is ours.
        unsafe { (api.g_list_free)(list) };
    }
    done(Ok(out));
}

unsafe extern "C" fn on_cookie_added(source: gpointer, result: gpointer, data: gpointer) {
    // SAFETY: `data` is the `Box<UnitDone>` passed with the call.
    let done = *unsafe { Box::from_raw(data.cast::<UnitDone>()) };
    let mut error: *mut GError = ptr::null_mut();
    // SAFETY: matching finish call.
    unsafe { (api().webkit_cookie_manager_add_cookie_finish)(source, result, &raw mut error) };
    finish_unit(done, error);
}

unsafe extern "C" fn on_cleared(_source: gpointer, _result: gpointer, data: gpointer) {
    // SAFETY: `data` is the `Box<UnitDone>` passed with the call.
    let done = *unsafe { Box::from_raw(data.cast::<UnitDone>()) };
    done(Ok(()));
}

fn finish_unit(done: UnitDone, error: *mut GError) {
    if error.is_null() {
        done(Ok(()));
    } else {
        // SAFETY: owned error, read then freed once.
        let message = unsafe { text((*error).message) };
        // SAFETY: as above.
        unsafe { (api().g_error_free)(error) };
        done(Err(WebViewError::Backend(message)));
    }
}

unsafe extern "C" fn png_sink(closure: gpointer, data: *const u8, len: c_uint) -> c_int {
    // SAFETY: `closure` is the `Vec<u8>` of `on_snapshot`; cairo hands
    // `len` readable bytes.
    unsafe {
        (*closure.cast::<Vec<u8>>())
            .extend_from_slice(std::slice::from_raw_parts(data, len as usize));
    }
    0
}

unsafe extern "C" fn on_snapshot(source: gpointer, result: gpointer, data: gpointer) {
    // SAFETY: `data` is the `Box<SnapshotDone>` passed with the call.
    let done = *unsafe { Box::from_raw(data.cast::<SnapshotDone>()) };
    let api = api();
    let mut error: *mut GError = ptr::null_mut();
    // SAFETY: matching finish call.
    let surface =
        unsafe { (api.webkit_web_view_get_snapshot_finish)(source, result, &raw mut error) };
    if surface.is_null() {
        return finish_unit(
            Box::new(move |r| {
                done(r.and_then(|()| Err(WebViewError::Backend("snapshot failed".into()))));
            }),
            error,
        );
    }
    let encoded = if FLAVOR.get() == Some(&Flavor::Gtk4) {
        texture_png(api, surface)
    } else {
        surface_png(api, surface)
    };
    let Some((png, w, h)) = encoded else {
        return done(Err(WebViewError::Backend("snapshot encoding".into())));
    };
    done(Ok(WebSnapshot {
        width: u32::try_from(w).unwrap_or(0),
        height: u32::try_from(h).unwrap_or(0),
        format: SnapshotFormat::Png,
        bytes: png,
    }));
}

// =====================================================================
// Signal handlers (GTK thread)
// =====================================================================

thread_local! {
    /// Cores by view id, readable while a view is lent to an engine call.
    static CORES: RefCell<HashMap<NativeHandleId, Arc<ViewCore>>> = RefCell::new(HashMap::new());
}

fn core_of(id: NativeHandleId) -> Option<Arc<ViewCore>> {
    CORES.with(|c| c.borrow().get(&id).cloned())
}

/// `WebKitPolicyDecision` kept alive until the app answers.
struct Decision(gpointer);

// SAFETY: the decision is only touched on the GTK thread: answers are
// posted there by the core before `use`/`ignore` run.
unsafe impl Send for Decision {}

impl Decision {
    fn answer(self, allow: bool) {
        let api = api();
        // SAFETY: a referenced decision, answered and released once.
        unsafe {
            if allow {
                (api.webkit_policy_decision_use)(self.0);
            } else {
                (api.webkit_policy_decision_ignore)(self.0);
            }
            (api.g_object_unref)(self.0);
        }
    }
}

unsafe extern "C" fn on_decide_policy(
    _view: gpointer,
    decision: gpointer,
    kind: c_int,
    data: gpointer,
) -> gboolean {
    let api = api();
    let Some(core) = core_of(ptr_id(data)) else {
        return FALSE;
    };
    match kind {
        // NAVIGATION_ACTION
        0 => {
            // SAFETY: a navigation decision and its action / request.
            let (url, gesture) = unsafe {
                let action =
                    (api.webkit_navigation_policy_decision_get_navigation_action)(decision);
                let request = (api.webkit_navigation_action_get_request)(action);
                (
                    text((api.webkit_uri_request_get_uri)(request)),
                    (api.webkit_navigation_action_is_user_gesture)(action) != FALSE,
                )
            };
            // SAFETY: keep the decision until it is answered.
            let held = Decision(unsafe { (api.g_object_ref)(decision) });
            core.navigation(&url, gesture, Box::new(move |allow| held.answer(allow)));
            TRUE
        }
        // NEW_WINDOW_ACTION: the `create` signal decides.
        _ => FALSE,
    }
}

unsafe extern "C" fn on_load_changed(view: gpointer, event: c_int, data: gpointer) {
    let api = api();
    let Some(core) = core_of(ptr_id(data)) else {
        return;
    };
    // SAFETY: live view.
    let url = text(unsafe { (api.webkit_web_view_get_uri)(view) });
    match event {
        0 => core.started(&url),
        2 => core.committed(&url),
        3 => {
            // SAFETY: live view.
            let (back, forward) = unsafe {
                (
                    (api.webkit_web_view_can_go_back)(view) != FALSE,
                    (api.webkit_web_view_can_go_forward)(view) != FALSE,
                )
            };
            core.finished(&url, back, forward);
        }
        _ => {}
    }
}

unsafe extern "C" fn on_load_failed(
    _view: gpointer,
    _event: c_int,
    uri: *const c_char,
    error: *mut GError,
    data: gpointer,
) -> gboolean {
    let Some(core) = core_of(ptr_id(data)) else {
        return FALSE;
    };
    // SAFETY: the signal passes a live error.
    let (error_code, message) = unsafe {
        error
            .as_ref()
            .map_or((0, String::new()), |e| (e.code, text(e.message)))
    };
    // WebKitNetworkError / PolicyError codes.
    let kind = match error_code {
        302 => NavErrorKind::Cancelled,
        399 | 499 => NavErrorKind::Other,
        _ if message.contains("resolve") => NavErrorKind::Dns,
        _ if message.to_ascii_lowercase().contains("tls") || message.contains("certificate") => {
            NavErrorKind::Tls
        }
        _ => NavErrorKind::Network,
    };
    core.failed(&text(uri), kind, &message);
    FALSE
}

unsafe extern "C" fn on_progress(view: gpointer, _pspec: gpointer, data: gpointer) {
    if let Some(core) = core_of(ptr_id(data)) {
        // SAFETY: live view.
        core.progress(unsafe { (api().webkit_web_view_get_estimated_load_progress)(view) });
    }
}

unsafe extern "C" fn on_title(view: gpointer, _pspec: gpointer, data: gpointer) {
    if let Some(core) = core_of(ptr_id(data)) {
        // SAFETY: live view.
        core.title(&text(unsafe { (api().webkit_web_view_get_title)(view) }));
    }
}

unsafe extern "C" fn on_terminated(_view: gpointer, reason: c_int, data: gpointer) {
    if let Some(core) = core_of(ptr_id(data)) {
        core.fail_pending();
        core.emit(crate::ControlEvent::ProcessTerminated(TerminationInfo {
            reason: if reason == 0 {
                TerminationReason::Crashed
            } else {
                TerminationReason::Killed
            },
        }));
    }
}

unsafe extern "C" fn on_close(_view: gpointer, data: gpointer) {
    if let Some(core) = core_of(ptr_id(data)) {
        core.emit(crate::ControlEvent::CloseRequested);
    }
}

unsafe extern "C" fn on_delete(_window: gpointer, _event: gpointer, data: gpointer) -> gboolean {
    if let Some(core) = core_of(ptr_id(data)) {
        core.emit(crate::ControlEvent::CloseRequested);
    }
    // Keep the window: the app closes the view.
    TRUE
}

const unsafe extern "C" fn on_context_menu4(
    _v: gpointer,
    _m: gpointer,
    _h: gpointer,
    _d: gpointer,
) -> gboolean {
    TRUE
}

unsafe extern "C" fn on_close_request(_window: gpointer, data: gpointer) -> gboolean {
    if let Some(core) = core_of(ptr_id(data)) {
        core.emit(crate::ControlEvent::CloseRequested);
    }
    // Keep the window: the app closes the view.
    TRUE
}

const unsafe extern "C" fn on_context_menu(
    _v: gpointer,
    _m: gpointer,
    _e: gpointer,
    _h: gpointer,
    _d: gpointer,
) -> gboolean {
    TRUE
}

unsafe extern "C" fn on_create(view: gpointer, action: gpointer, data: gpointer) -> gpointer {
    let api = api();
    let Some(opener) = core_of(ptr_id(data)) else {
        return ptr::null_mut();
    };
    // SAFETY: the signal's navigation action.
    let url = unsafe {
        text((api.webkit_uri_request_get_uri)((api
            .webkit_navigation_action_get_request)(
            action
        )))
    };
    // WebKit wants the new view now: the rules decide.
    if !opener.allows_popup(&url) {
        return ptr::null_mut();
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
    let Some((core, instance)) = opener.new_popup(spec) else {
        return ptr::null_mut();
    };
    // A related view shares the opener's process, context and storage.
    // SAFETY: type getter.
    let ty = unsafe { (api.webkit_web_view_get_type)() };
    let child = new_object(
        api,
        ty,
        vec![(c"related-view", GValueBox::object(api, view))],
    );
    let attached = ui(|ui| ui.attach(Arc::clone(&core), child, false)).is_some_and(|r| r.is_ok());
    if !attached {
        return ptr::null_mut();
    }
    opener.emit(crate::ControlEvent::NewView(NewViewInfo { instance, url }));
    child
}

/// A pending engine object resolved later on the GTK thread.
struct Held(gpointer);

// SAFETY: only used on the GTK thread (answers are posted there).
unsafe impl Send for Held {}

impl Held {
    const fn ptr(&self) -> gpointer {
        self.0
    }
}

unsafe extern "C" fn on_permission(_view: gpointer, request: gpointer, data: gpointer) -> gboolean {
    let api = api();
    let Some(core) = core_of(ptr_id(data)) else {
        return FALSE;
    };
    // SAFETY: type checks on the signal's request object.
    let permissions = unsafe {
        if (api.g_type_check_instance_is_a)(
            request,
            (api.webkit_user_media_permission_request_get_type)(),
        ) != FALSE
        {
            let mut p = Vec::new();
            if (api.webkit_user_media_permission_is_for_video_device)(request) != FALSE {
                p.push(WebPermission::Camera);
            }
            if (api.webkit_user_media_permission_is_for_audio_device)(request) != FALSE {
                p.push(WebPermission::Microphone);
            }
            p
        } else if (api.g_type_check_instance_is_a)(
            request,
            (api.webkit_geolocation_permission_request_get_type)(),
        ) != FALSE
        {
            vec![WebPermission::Geolocation]
        } else if (api.g_type_check_instance_is_a)(
            request,
            (api.webkit_notification_permission_request_get_type)(),
        ) != FALSE
        {
            vec![WebPermission::Notifications]
        } else {
            vec![WebPermission::Other]
        }
    };
    let origin = core.committed_origin().unwrap_or_default();
    // SAFETY: held until answered.
    let held = Held(unsafe { (api.g_object_ref)(request) });
    let wanted = permissions.clone();
    core.ask(
        DecisionKind::Permission(PermissionAsk {
            permissions,
            frame_origin: origin.clone(),
            top_origin: origin,
        }),
        Box::new(move |response| {
            let allow = match response {
                DecisionResponse::Allow => true,
                DecisionResponse::Grant(granted) => wanted.iter().all(|p| granted.contains(p)),
                _ => false,
            };
            // SAFETY: answered and released once on the GTK thread.
            unsafe {
                if allow {
                    (api.webkit_permission_request_allow)(held.ptr());
                } else {
                    (api.webkit_permission_request_deny)(held.ptr());
                }
                (api.g_object_unref)(held.ptr());
            }
        }),
        None,
    );
    TRUE
}

unsafe extern "C" fn on_script_dialog(
    _view: gpointer,
    dialog: gpointer,
    data: gpointer,
) -> gboolean {
    let api = api();
    let Some(core) = core_of(ptr_id(data)) else {
        return FALSE;
    };
    // SAFETY: live dialog; referenced until answered.
    let (kind, message, default_text, held) = unsafe {
        let kind = match (api.webkit_script_dialog_get_dialog_type)(dialog) {
            1 => DialogKind::Confirm,
            2 => DialogKind::Prompt,
            3 => DialogKind::BeforeUnload,
            _ => DialogKind::Alert,
        };
        let default = if kind == DialogKind::Prompt {
            text((api.webkit_script_dialog_prompt_get_default_text)(dialog))
        } else {
            String::new()
        };
        (
            kind,
            text((api.webkit_script_dialog_get_message)(dialog)),
            default,
            Held((api.webkit_script_dialog_ref)(dialog)),
        )
    };
    let origin = core.committed_origin().unwrap_or_default();
    core.ask(
        DecisionKind::Dialog(DialogAsk {
            kind,
            message,
            default_text,
            origin,
        }),
        Box::new(move |response| {
            // SAFETY: answered, closed and released once on the GTK thread.
            unsafe {
                match (&response, kind) {
                    (DecisionResponse::DialogAccept(text), DialogKind::Prompt) => {
                        let t = cstring(text.as_deref().unwrap_or(""));
                        (api.webkit_script_dialog_prompt_set_text)(held.ptr(), t.as_ptr());
                    }
                    (r, DialogKind::Confirm | DialogKind::BeforeUnload) => {
                        (api.webkit_script_dialog_confirm_set_confirmed)(
                            held.ptr(),
                            i32::from(!matches!(r, DecisionResponse::Deny)),
                        );
                    }
                    _ => {}
                }
                (api.webkit_script_dialog_close)(held.ptr());
                (api.webkit_script_dialog_unref)(held.ptr());
            }
        }),
        None,
    );
    TRUE
}

unsafe extern "C" fn on_authenticate(
    _view: gpointer,
    request: gpointer,
    data: gpointer,
) -> gboolean {
    let api = api();
    let Some(core) = core_of(ptr_id(data)) else {
        return FALSE;
    };
    // SAFETY: live request; referenced until answered.
    let (host, realm, proxy, held) = unsafe {
        (
            text((api.webkit_authentication_request_get_host)(request)),
            text((api.webkit_authentication_request_get_realm)(request)),
            (api.webkit_authentication_request_is_for_proxy)(request) != FALSE,
            Held((api.g_object_ref)(request)),
        )
    };
    core.ask(
        DecisionKind::HttpAuth(HttpAuthAsk {
            host,
            realm,
            is_proxy: proxy,
        }),
        Box::new(move |response| {
            // SAFETY: answered and released once on the GTK thread.
            unsafe {
                if let DecisionResponse::Credentials(c) = response {
                    let (u, p) = (cstring(&c.user), cstring(&c.password));
                    // WEBKIT_CREDENTIAL_PERSISTENCE_FOR_SESSION = 1.
                    let credential = (api.webkit_credential_new)(u.as_ptr(), p.as_ptr(), 1);
                    (api.webkit_authentication_request_authenticate)(held.ptr(), credential);
                    (api.webkit_credential_free)(credential);
                } else {
                    (api.webkit_authentication_request_cancel)(held.ptr());
                }
                (api.g_object_unref)(held.ptr());
            }
        }),
        None,
    );
    TRUE
}

unsafe extern "C" fn on_file_chooser(
    _view: gpointer,
    request: gpointer,
    data: gpointer,
) -> gboolean {
    let api = api();
    let Some(core) = core_of(ptr_id(data)) else {
        return FALSE;
    };
    // SAFETY: live request; referenced until answered.
    let (multiple, held) = unsafe {
        (
            (api.webkit_file_chooser_request_get_select_multiple)(request) != FALSE,
            Held((api.g_object_ref)(request)),
        )
    };
    core.ask(
        DecisionKind::FileChooser(FileChooserAsk {
            multiple,
            accept: Vec::new(),
            capture: false,
        }),
        Box::new(move |response| {
            // SAFETY: answered and released once on the GTK thread.
            unsafe {
                if let DecisionResponse::Files(paths) = response {
                    let owned: Vec<CString> = paths.iter().map(|p| cstring(p)).collect();
                    let mut list: Vec<*const c_char> = owned.iter().map(|c| c.as_ptr()).collect();
                    list.push(ptr::null());
                    (api.webkit_file_chooser_request_select_files)(held.ptr(), list.as_ptr());
                } else {
                    (api.webkit_file_chooser_request_cancel)(held.ptr());
                }
                (api.g_object_unref)(held.ptr());
            }
        }),
        None,
    );
    TRUE
}

unsafe extern "C" fn on_found_text(_fc: gpointer, count: c_uint, data: gpointer) {
    if let Some(Some(done)) = with(ptr_id(data), |v| v.find_done.take()) {
        done(Ok(FindResult {
            matches: count,
            active: u32::from(count > 0),
        }));
    }
}

unsafe extern "C" fn on_find_failed(_fc: gpointer, data: gpointer) {
    if let Some(Some(done)) = with(ptr_id(data), |v| v.find_done.take()) {
        done(Ok(FindResult::default()));
    }
}

unsafe extern "C" fn on_bridge_message(
    _ucm: gpointer,
    value: gpointer,
    reply: gpointer,
    data: gpointer,
) -> gboolean {
    let api = api();
    let Some(core) = core_of(ptr_id(data)) else {
        return FALSE;
    };
    // SAFETY: the message value is a live `JSCValue` holding a string.
    let raw = unsafe { take_text((api.jsc_value_to_string)(value)) };
    let parsed = parse_bridge(&raw);
    // SAFETY: the reply object is referenced while it is pending.
    let reply = Held(unsafe { (api.webkit_script_message_reply_ref)(reply) });
    // SAFETY: the context is borrowed from the value; keep it alive
    // until the reply is sent.
    let context = Held(unsafe { (api.g_object_ref)((api.jsc_value_get_context)(value)) });
    let Some((kind, channel, payload, origin, main)) = parsed else {
        reply_error(reply, "malformed message");
        return TRUE;
    };
    // The page-reported origin must be the committed one: WebKitGTK does
    // not report the sender, and the shim only runs in allowed origins.
    if core.committed_origin().as_deref() != Some(origin.as_str()) {
        reply_error(reply, "not allowed");
        return TRUE;
    }
    let document = core.document_id();
    if kind == "i" {
        core.bridge_message(
            channel,
            origin,
            main,
            document,
            payload,
            Some(Box::new(move |result| {
                // SAFETY: replies are settled once on the GTK thread.
                unsafe {
                    match result {
                        ReplyResult::Ok(bytes) => {
                            let s = cstring(&crate::desktop::text::base64(&bytes));
                            let js = (api.jsc_value_new_string)(context.ptr(), s.as_ptr());
                            (api.webkit_script_message_reply_return_value)(reply.ptr(), js);
                            (api.g_object_unref)(js);
                        }
                        ReplyResult::Err(message) => {
                            let m = cstring(&message);
                            (api.webkit_script_message_reply_return_error_message)(
                                reply.ptr(),
                                m.as_ptr(),
                            );
                        }
                    }
                    (api.webkit_script_message_reply_unref)(reply.ptr());
                    (api.g_object_unref)(context.ptr());
                }
            })),
        );
    } else {
        core.bridge_message(channel, origin, main, document, payload, None);
        reply_error(reply, "");
    }
    TRUE
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "settles and releases the held reply"
)]
fn reply_error(reply: Held, message: &str) {
    let api = api();
    let m = cstring(message);
    // SAFETY: settled and released once.
    unsafe {
        (api.webkit_script_message_reply_return_error_message)(reply.ptr(), m.as_ptr());
        (api.webkit_script_message_reply_unref)(reply.ptr());
    }
}

/// `{"t":…,"c":…,"b":…,"o":…,"m":…}` from the shim; a tiny parser for
/// that one shape (keys and values are JSON strings, `m` a boolean).
fn parse_bridge(raw: &str) -> Option<(String, String, Vec<u8>, String, bool)> {
    let field = |key: &str| -> Option<String> {
        let needle = format!("\"{key}\":\"");
        let start = raw.find(&needle)? + needle.len();
        let mut out = String::new();
        let mut chars = raw[start..].chars();
        while let Some(c) = chars.next() {
            match c {
                '"' => return Some(out),
                '\\' => match chars.next()? {
                    'n' => out.push('\n'),
                    'u' => {
                        let hex: String = chars.by_ref().take(4).collect();
                        out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
                    }
                    other => out.push(other),
                },
                c => out.push(c),
            }
        }
        None
    };
    let main = raw.contains("\"m\":true");
    Some((
        field("t")?,
        field("c")?,
        crate::desktop::text::unbase64(&field("b")?)?,
        field("o")?,
        main,
    ))
}

// =====================================================================
// Custom schemes
// =====================================================================

unsafe extern "C" fn on_scheme_request(request: gpointer, _data: gpointer) {
    let api = api();
    // SAFETY: live scheme request.
    let (uri, method, web_view) = unsafe {
        (
            text((api.webkit_uri_scheme_request_get_uri)(request)),
            text((api.webkit_uri_scheme_request_get_http_method)(request)),
            (api.webkit_uri_scheme_request_get_web_view)(request),
        )
    };
    let owner = ui(|ui| {
        ui.views
            .iter()
            .find(|(_, v)| v.web_view == web_view)
            .map(|(_, v)| Arc::clone(&v.core))
    })
    .flatten();
    // SAFETY: referenced until finished.
    let held = Held(unsafe { (api.g_object_ref)(request) });
    let Some(core) = owner else {
        return finish_status(held, 404, "text/plain", b"");
    };
    let Ok(url) = url::Url::parse(&uri) else {
        return finish_status(held, 400, "text/plain", b"");
    };
    let Some(scheme) = core.config.schemes.iter().find(|s| s.name == url.scheme()) else {
        return finish_status(held, 404, "text/plain", b"");
    };
    // Only the view's own documents on that scheme, or its main-frame
    // loads of it, may read app content.
    let origin = core.committed_origin();
    let own_origin = format!("{}://{}", url.scheme(), url.host_str().unwrap_or_default());
    let from_page = origin.as_deref().is_some_and(|o| o != own_origin);
    match &scheme.source {
        SchemeSource::Assets(source) => {
            if from_page {
                return finish_status(held, 403, "text/plain", b"");
            }
            serve_asset(held, &source.dir, url.path());
        }
        SchemeSource::Dynamic => {
            core.scheme_request(
                SchemeRequestInfo {
                    request_id: 0,
                    scheme: scheme.name.clone(),
                    method,
                    url: uri,
                    headers: Vec::new(),
                    has_body: false,
                    initiator_origin: None,
                },
                Box::new(GtkResponder {
                    request: Some(held),
                    pipe: None,
                }),
            );
        }
    }
}

fn serve_asset(held: Held, dir: &str, path: &str) {
    match crate::desktop::read_asset(dir, path) {
        Ok((mime, bytes)) => finish_status(held, 200, mime, &bytes),
        Err(status) => finish_status(held, status, "text/plain", b""),
    }
}

/// Response headers with `nosniff`, plus the app's (never `ACAO: *`).
fn headers(api: &Api, extra: &[crate::HttpHeader]) -> gpointer {
    // SAFETY: SOUP_MESSAGE_HEADERS_RESPONSE = 1; strings copied.
    unsafe {
        let headers = (api.soup_message_headers_new)(1);
        (api.soup_message_headers_append)(
            headers,
            c"X-Content-Type-Options".as_ptr(),
            c"nosniff".as_ptr(),
        );
        (api.soup_message_headers_append)(headers, c"Cache-Control".as_ptr(), c"no-cache".as_ptr());
        for h in extra {
            if h.name.eq_ignore_ascii_case("access-control-allow-origin") && h.value == "*" {
                continue;
            }
            let (n, v) = (cstring(&h.name), cstring(&h.value));
            (api.soup_message_headers_append)(headers, n.as_ptr(), v.as_ptr());
        }
        headers
    }
}

fn finish_with(
    held: &Held,
    status: u16,
    mime: &str,
    stream: gpointer,
    length: i64,
    extra: &[crate::HttpHeader],
) {
    let api = api();
    let m = cstring(mime);
    // SAFETY: the request is live; the response takes the stream.
    unsafe {
        let response = (api.webkit_uri_scheme_response_new)(stream, length);
        (api.webkit_uri_scheme_response_set_status)(response, c_uint::from(status), ptr::null());
        (api.webkit_uri_scheme_response_set_content_type)(response, m.as_ptr());
        (api.webkit_uri_scheme_response_set_http_headers)(response, headers(api, extra));
        (api.webkit_uri_scheme_request_finish_with_response)(held.ptr(), response);
        (api.g_object_unref)(response);
        (api.g_object_unref)(stream);
    }
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "finishes and releases the held request"
)]
fn finish_status(held: Held, status: u16, mime: &str, body: &[u8]) {
    let api = api();
    // SAFETY: GLib copies `body`.
    let stream = unsafe {
        let bytes = (api.g_bytes_new)(body.as_ptr().cast(), body.len());
        let stream = (api.g_memory_input_stream_new_from_bytes)(bytes);
        (api.g_bytes_unref)(bytes);
        stream
    };
    finish_with(
        &held,
        status,
        mime,
        stream,
        i64::try_from(body.len()).unwrap_or(-1),
        &[],
    );
    // SAFETY: release the reference taken with the request.
    unsafe { (api.g_object_unref)(held.ptr()) };
}

/// Streams a dynamic response through a non-blocking pipe: `WebKit` reads
/// one end as a `GUnixInputStream`; the GTK loop writes the other
/// whenever it is writable. No thread per response, and neither a slow
/// page nor a fast app blocks the GTK thread.
struct GtkResponder {
    request: Option<Held>,
    pipe: Option<u64>,
}

/// Write end of a response pipe and the bytes it has not taken yet.
struct PipeOut {
    file: std::fs::File,
    queue: std::collections::VecDeque<Vec<u8>>,
    offset: usize,
    watching: bool,
    ended: bool,
}

impl PipeOut {
    /// Write what the pipe takes now; `Err` once the reader is gone.
    fn flush(&mut self) -> std::io::Result<()> {
        while let Some(chunk) = self.queue.front() {
            match self.file.write(&chunk[self.offset..]) {
                Ok(n) => {
                    self.offset += n;
                    if self.offset == chunk.len() {
                        self.queue.pop_front();
                        self.offset = 0;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    fn drained(&self) -> bool {
        self.ended && self.queue.is_empty()
    }
}

thread_local! {
    static PIPES: RefCell<HashMap<u64, PipeOut>> = RefCell::new(HashMap::new());
    static NEXT_PIPE: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Flush pipe `key`; close it once ended and drained; watch it for
/// writability while bytes are left.
fn pump(key: u64) {
    let watch = PIPES.with(|p| {
        let mut pipes = p.borrow_mut();
        let pipe = pipes.get_mut(&key)?;
        if pipe.flush().is_err() || pipe.drained() {
            pipes.remove(&key);
            return None;
        }
        if pipe.queue.is_empty() || pipe.watching {
            return None;
        }
        pipe.watching = true;
        Some(std::os::fd::AsRawFd::as_raw_fd(&pipe.file))
    });
    if let Some(fd) = watch {
        // SAFETY: the fd stays open while its `PipeOut` exists; the
        // callback looks the pipe up by key and ignores a removed one.
        unsafe {
            (api().g_unix_fd_add_full)(
                0,
                fd,
                G_IO_OUT | G_IO_ERR | G_IO_HUP,
                on_pipe_writable,
                id_ptr(NativeHandleId(key)),
                None,
            );
        }
    }
}

unsafe extern "C" fn on_pipe_writable(_fd: c_int, _condition: c_uint, data: gpointer) -> gboolean {
    let key = ptr_id(data).0;
    PIPES.with(|p| {
        let mut pipes = p.borrow_mut();
        let Some(pipe) = pipes.get_mut(&key) else {
            return FALSE;
        };
        if pipe.flush().is_err() || pipe.drained() {
            pipes.remove(&key);
            return FALSE;
        }
        if pipe.queue.is_empty() {
            pipe.watching = false;
            return FALSE;
        }
        TRUE
    })
}

impl SchemeResponder for GtkResponder {
    fn head(&mut self, head: &SchemeResponseHead) {
        let Some(request) = self.request.take() else {
            return;
        };
        let Ok((reader, writer)) = std::io::pipe() else {
            return finish_status(request, 500, "text/plain", b"");
        };
        let api = api();
        let fd = std::os::fd::OwnedFd::from(writer);
        // SAFETY: a live fd we own; a failure leaves it blocking, which
        // the flush loop still handles (it just may stall the thread).
        unsafe {
            (api.g_unix_set_fd_nonblocking)(
                std::os::fd::AsRawFd::as_raw_fd(&fd),
                TRUE,
                ptr::null_mut(),
            );
        }
        // SAFETY: WebKit owns the read end from here (close_fd = TRUE).
        let stream = unsafe { (api.g_unix_input_stream_new)(reader.into_raw_fd(), TRUE) };
        let length = head
            .content_length
            .and_then(|l| i64::try_from(l).ok())
            .unwrap_or(-1);
        finish_with(
            &request,
            head.status,
            &head.mime_type,
            stream,
            length,
            &head.headers,
        );
        // SAFETY: release the reference taken with the request.
        unsafe { (api.g_object_unref)(request.ptr()) };
        let key = NEXT_PIPE.with(|n| {
            n.set(n.get() + 1);
            n.get()
        });
        PIPES.with(|p| {
            p.borrow_mut().insert(
                key,
                PipeOut {
                    file: std::fs::File::from(fd),
                    queue: std::collections::VecDeque::new(),
                    offset: 0,
                    watching: false,
                    ended: false,
                },
            );
        });
        self.pipe = Some(key);
    }

    fn body(&mut self, body: SchemeBody) {
        if let Some(held) = self.request.take() {
            // A body without a head: nothing was sent yet.
            return finish_status(held, 502, "text/plain", b"");
        }
        let Some(key) = self.pipe else { return };
        let abort = PIPES.with(|p| {
            let mut pipes = p.borrow_mut();
            let pipe = pipes.get_mut(&key)?;
            match body {
                SchemeBody::Data(data) => {
                    if !data.is_empty() {
                        pipe.queue.push_back(data);
                    }
                }
                SchemeBody::End => pipe.ended = true,
                // Closing early truncates the body for the page.
                SchemeBody::Abort(_) => return Some(()),
            }
            None
        });
        if abort.is_some() {
            PIPES.with(|p| p.borrow_mut().remove(&key));
            self.pipe = None;
            return;
        }
        pump(key);
    }
}

/// `gtk_init_check` and `gtk_window_new`, whose C signatures differ
/// between GTK 3 and GTK 4.
#[expect(clippy::type_complexity, reason = "two raw C entry points")]
fn toolkit_entry_points(
    libs: &Libs,
    flavor: Flavor,
) -> Result<(Box<dyn Fn() -> gboolean>, WindowNew), WebViewError> {
    let missing = |e: libloading::Error| WebViewError::RuntimeMissing(e.to_string());
    // SAFETY: each lookup uses the prototype of the GTK version loaded.
    unsafe {
        match flavor {
            Flavor::Gtk3 => {
                let init = *libs
                    .gtk
                    .get::<unsafe extern "C" fn(*mut c_int, *mut *mut *mut c_char) -> gboolean>(
                        b"gtk_init_check\0",
                    )
                    .map_err(missing)?;
                let new = *libs
                    .gtk
                    .get::<unsafe extern "C" fn(c_int) -> gpointer>(b"gtk_window_new\0")
                    .map_err(missing)?;
                // GTK 3 accepts null argc / argv.
                Ok((
                    Box::new(move || init(ptr::null_mut(), ptr::null_mut())),
                    WindowNew::Gtk3(new),
                ))
            }
            Flavor::Gtk4 => {
                let init = *libs
                    .gtk
                    .get::<unsafe extern "C" fn() -> gboolean>(b"gtk_init_check\0")
                    .map_err(missing)?;
                let new = *libs
                    .gtk
                    .get::<unsafe extern "C" fn() -> gpointer>(b"gtk_window_new\0")
                    .map_err(missing)?;
                Ok((Box::new(move || init()), WindowNew::Gtk4(new)))
            }
        }
    }
}

/// Toolkit calls whose name or shape differs between GTK 3 and 4.
mod toolkit {
    use super::{Api, FALSE, TRUE, WindowNew, gpointer};

    /// A new top-level (or, GTK 3 only, popup) window.
    pub(super) fn window(new: WindowNew, popup: bool) -> gpointer {
        // SAFETY: constructors with the version's own prototype.
        unsafe {
            match new {
                WindowNew::Gtk3(f) => f(i32::from(popup)),
                WindowNew::Gtk4(f) => f(),
            }
        }
    }

    pub(super) fn set_child(api: &Api, window: gpointer, child: gpointer) {
        // SAFETY: live widgets; exactly one of the two exists.
        unsafe {
            if let Some(f) = api.gtk_window_set_child {
                f(window, child);
            } else if let Some(f) = api.gtk_container_add {
                f(window, child);
            }
        }
    }

    pub(super) fn show(api: &Api, window: gpointer) {
        // SAFETY: live widget.
        unsafe {
            if let Some(f) = api.gtk_widget_show_all {
                f(window);
            } else if let Some(f) = api.gtk_widget_set_visible {
                f(window, TRUE);
            }
        }
    }

    pub(super) fn hide(api: &Api, window: gpointer) {
        // SAFETY: live widget.
        unsafe {
            if let Some(f) = api.gtk_widget_set_visible {
                f(window, FALSE);
            } else {
                (api.gtk_widget_hide)(window);
            }
        }
    }

    pub(super) fn destroy(api: &Api, window: gpointer) {
        // SAFETY: live window, destroyed once.
        unsafe {
            if let Some(f) = api.gtk_window_destroy {
                f(window);
            } else if let Some(f) = api.gtk_widget_destroy {
                f(window);
            }
        }
    }
}

/// Loaded version, for callbacks that cannot reach the UI state.
static FLAVOR: std::sync::OnceLock<Flavor> = std::sync::OnceLock::new();

/// 4.1 snapshots are cairo surfaces.
fn surface_png(api: &Api, surface: gpointer) -> Option<(Vec<u8>, c_int, c_int)> {
    let (write, width, height, destroy) = (
        api.cairo_surface_write_to_png_stream?,
        api.cairo_image_surface_get_width?,
        api.cairo_image_surface_get_height?,
        api.cairo_surface_destroy?,
    );
    let mut png = Vec::new();
    // SAFETY: live surface; `png_sink` appends to `png`; destroyed once.
    unsafe {
        write(surface, png_sink, (&raw mut png).cast());
        let size = (width(surface), height(surface));
        destroy(surface);
        Some((png, size.0, size.1))
    }
}

/// 6.0 snapshots are `GdkTexture`s.
fn texture_png(api: &Api, texture: gpointer) -> Option<(Vec<u8>, c_int, c_int)> {
    let (save, width, height, data_of) = (
        api.gdk_texture_save_to_png_bytes?,
        api.gdk_texture_get_width?,
        api.gdk_texture_get_height?,
        api.g_bytes_get_data?,
    );
    // SAFETY: live texture; the bytes object is read then released.
    unsafe {
        let bytes = save(texture);
        let mut len = 0_usize;
        let data = data_of(bytes, &raw mut len).cast::<u8>();
        let png = if data.is_null() {
            Vec::new()
        } else {
            std::slice::from_raw_parts(data, len).to_vec()
        };
        let size = (width(texture), height(texture));
        (api.g_bytes_unref)(bytes);
        (api.g_object_unref)(texture);
        Some((png, size.0, size.1))
    }
}
