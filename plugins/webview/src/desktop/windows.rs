//! `WebView2` engine for Windows.
//!
//! One STA thread owns the `WebView2` environment and every controller;
//! tasks reach it as window messages to a message-only window. One
//! environment (and one user data folder) serves every profile: named
//! profiles and `InPrivate` mode are controller options, so profiles share
//! the browser process instead of starting one each.
//!
//! Embedded controllers are children of the app's registered `HWND`,
//! which lives on another thread; Windows then attaches the two
//! threads' input queues, so neither thread should block for long.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

use istmo_core::NativeHandleId;
use istmo_core::sync::lock;
use webview2_com::Microsoft::Web::WebView2::Win32::{
    COREWEBVIEW2_BROWSING_DATA_KINDS, COREWEBVIEW2_BROWSING_DATA_KINDS_ALL_PROFILE,
    COREWEBVIEW2_BROWSING_DATA_KINDS_CACHE_STORAGE, COREWEBVIEW2_BROWSING_DATA_KINDS_COOKIES,
    COREWEBVIEW2_BROWSING_DATA_KINDS_DISK_CACHE, COREWEBVIEW2_BROWSING_DATA_KINDS_INDEXED_DB,
    COREWEBVIEW2_BROWSING_DATA_KINDS_LOCAL_STORAGE,
    COREWEBVIEW2_BROWSING_DATA_KINDS_SERVICE_WORKERS,
    COREWEBVIEW2_CAPTURE_PREVIEW_IMAGE_FORMAT_JPEG, COREWEBVIEW2_CAPTURE_PREVIEW_IMAGE_FORMAT_PNG,
    COREWEBVIEW2_COLOR, COREWEBVIEW2_COOKIE_SAME_SITE_KIND_LAX,
    COREWEBVIEW2_COOKIE_SAME_SITE_KIND_NONE, COREWEBVIEW2_COOKIE_SAME_SITE_KIND_STRICT,
    COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC, COREWEBVIEW2_PERMISSION_KIND,
    COREWEBVIEW2_PERMISSION_KIND_CAMERA, COREWEBVIEW2_PERMISSION_KIND_CLIPBOARD_READ,
    COREWEBVIEW2_PERMISSION_KIND_GEOLOCATION, COREWEBVIEW2_PERMISSION_KIND_MICROPHONE,
    COREWEBVIEW2_PERMISSION_KIND_NOTIFICATIONS, COREWEBVIEW2_PERMISSION_STATE_ALLOW,
    COREWEBVIEW2_PERMISSION_STATE_DENY, COREWEBVIEW2_PRINT_DIALOG_KIND_BROWSER,
    COREWEBVIEW2_SCRIPT_DIALOG_KIND, COREWEBVIEW2_SCRIPT_DIALOG_KIND_BEFOREUNLOAD,
    COREWEBVIEW2_SCRIPT_DIALOG_KIND_CONFIRM, COREWEBVIEW2_SCRIPT_DIALOG_KIND_PROMPT,
    COREWEBVIEW2_WEB_ERROR_STATUS, COREWEBVIEW2_WEB_ERROR_STATUS_CANNOT_CONNECT,
    COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_COMMON_NAME_IS_INCORRECT,
    COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_EXPIRED,
    COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_IS_INVALID,
    COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_REVOKED,
    COREWEBVIEW2_WEB_ERROR_STATUS_CLIENT_CERTIFICATE_CONTAINS_ERRORS,
    COREWEBVIEW2_WEB_ERROR_STATUS_CONNECTION_ABORTED,
    COREWEBVIEW2_WEB_ERROR_STATUS_CONNECTION_RESET, COREWEBVIEW2_WEB_ERROR_STATUS_DISCONNECTED,
    COREWEBVIEW2_WEB_ERROR_STATUS_HOST_NAME_NOT_RESOLVED,
    COREWEBVIEW2_WEB_ERROR_STATUS_OPERATION_CANCELED, COREWEBVIEW2_WEB_ERROR_STATUS_TIMEOUT,
    COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL, CreateCoreWebView2EnvironmentWithOptions,
    GetAvailableCoreWebView2BrowserVersionString, ICoreWebView2, ICoreWebView2_2, ICoreWebView2_13,
    ICoreWebView2_16, ICoreWebView2Controller, ICoreWebView2Controller2,
    ICoreWebView2ControllerOptions, ICoreWebView2CustomSchemeRegistration, ICoreWebView2Deferral,
    ICoreWebView2Environment, ICoreWebView2Environment2, ICoreWebView2Environment10,
    ICoreWebView2EnvironmentOptions, ICoreWebView2Profile2, ICoreWebView2Settings2,
    ICoreWebView2Settings4, ICoreWebView2WebResourceRequestedEventArgs,
};
use webview2_com::{
    CallDevToolsProtocolMethodCompletedHandler, CapturePreviewCompletedHandler,
    ClearBrowsingDataCompletedHandler, ContentLoadingEventHandler,
    CoreWebView2CustomSchemeRegistration, CoreWebView2EnvironmentOptions,
    CreateCoreWebView2ControllerCompletedHandler, CreateCoreWebView2EnvironmentCompletedHandler,
    DocumentTitleChangedEventHandler, ExecuteScriptCompletedHandler, GetCookiesCompletedHandler,
    NavigationCompletedEventHandler, NavigationStartingEventHandler,
    NewWindowRequestedEventHandler, PermissionRequestedEventHandler, ProcessFailedEventHandler,
    ScriptDialogOpeningEventHandler, WebMessageReceivedEventHandler,
    WebResourceRequestedEventHandler, WindowCloseRequestedEventHandler, take_pwstr,
};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, IStream};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::SHCreateMemStream;
use windows::Win32::UI::WindowsAndMessaging::{
    CW_USEDEFAULT, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect,
    GetMessageW, HWND_MESSAGE, IDC_ARROW, LoadCursorW, MSG, PostMessageW, RegisterClassW, SW_SHOW,
    ShowWindow, TranslateMessage, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_CLOSE, WM_SIZE,
    WNDCLASSW, WS_MAXIMIZEBOX, WS_OVERLAPPEDWINDOW, WS_THICKFRAME,
};
use windows::core::{BOOL, HSTRING, Interface, PCWSTR, PWSTR, w};

use super::core::{Done, Engine, FrameSlot, SchemeResponder, UiTask, UiView, ViewCore, ViewTask};
use crate::present::FrameSink;
use crate::{
    ClearDataRequest, CookieFilter, DataKind, DecisionKind, DecisionResponse, FindQuery,
    FindResult, HistoryNav, LoadRequest, NavErrorKind, NewViewInfo, PermissionAsk, RenderTarget,
    ReplyResult, SameSite, SchemeBody, SchemeRequestInfo, SchemeResponseHead, SchemeSource,
    ScriptCall, ScriptWorld, SnapshotFormat, SnapshotOptions, TerminationInfo, TerminationReason,
    ViewFrame, WebCookie, WebHistory, WebPermission, WebProfile, WebSnapshot, WebViewCapabilities,
    WebViewError, WebViewInitConfig,
};

const WM_TASK: u32 = WM_APP + 1;
const BOOTSTRAPPER: &str = "https://go.microsoft.com/fwlink/p/?LinkId=2124703";

fn hs(s: &str) -> HSTRING {
    HSTRING::from(s)
}

fn take(p: PWSTR) -> String {
    take_pwstr(p)
}

fn werr(e: &windows::core::Error) -> WebViewError {
    WebViewError::Backend(e.message())
}

/// Moves COM objects into UI-thread tasks; they never run elsewhere.
struct Ui<T>(T);

// SAFETY: every `Ui` is created on the WebView2 thread and only used by
// tasks posted back to it.
#[expect(
    clippy::non_send_fields_in_send_ty,
    reason = "the wrapper moves STA objects into STA tasks"
)]
unsafe impl<T> Send for Ui<T> {}

impl<T> Ui<T> {
    const fn get(&self) -> &T {
        &self.0
    }
}

// =====================================================================
// Engine
// =====================================================================

pub(crate) struct WinEngine {
    /// Message-only window of the `WebView2` thread.
    hwnd: isize,
    version: String,
    frames: Mutex<HashMap<NativeHandleId, Arc<FrameState>>>,
    me: Weak<Self>,
}

struct FrameState {
    slot: FrameSlot,
    scheduled: AtomicBool,
}

struct WinFrameSink {
    id: NativeHandleId,
    state: Arc<FrameState>,
    engine: Weak<WinEngine>,
}

impl FrameSink for WinFrameSink {
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

struct State {
    env: Option<ICoreWebView2Environment>,
    views: HashMap<NativeHandleId, WinView>,
    cores: HashMap<NativeHandleId, Arc<ViewCore>>,
    /// Opens waiting for the environment.
    waiting: Vec<(Arc<ViewCore>, Done<()>)>,
    config: Option<Arc<WebViewInitConfig>>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State {
        env: None,
        views: HashMap::new(),
        cores: HashMap::new(),
        waiting: Vec::new(),
        config: None,
    });
}

fn with<R>(id: NativeHandleId, f: impl FnOnce(&mut WinView) -> R) -> Option<R> {
    STATE.with(|s| s.try_borrow_mut().ok()?.views.get_mut(&id).map(f))
}

fn core_of(id: NativeHandleId) -> Option<Arc<ViewCore>> {
    STATE.with(|s| s.try_borrow().ok()?.cores.get(&id).cloned())
}

impl WinEngine {
    pub(crate) fn start() -> Result<Arc<Self>, WebViewError> {
        let (tx, rx) = flume::bounded(1);
        std::thread::Builder::new()
            .name("istmo-webview-win".into())
            .spawn(move || run(&tx))
            .map_err(|e| WebViewError::Backend(format!("spawn WebView2 thread: {e}")))?;
        let (hwnd, version) = rx
            .recv()
            .map_err(|_| WebViewError::Backend("WebView2 thread exited".into()))??;
        Ok(Arc::new_cyclic(|me| Self {
            hwnd,
            version,
            frames: Mutex::default(),
            me: me.clone(),
        }))
    }
}

fn run(started: &flume::Sender<Result<(isize, String), WebViewError>>) {
    let setup = || -> Result<(HWND, String), WebViewError> {
        // SAFETY: plain Win32 / COM initialisation of this thread.
        unsafe {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED)
                .ok()
                .map_err(|e| werr(&e))?;
            let mut version = PWSTR::null();
            GetAvailableCoreWebView2BrowserVersionString(PCWSTR::null(), &raw mut version)
                .map_err(|_| WebViewError::RuntimeMissing(BOOTSTRAPPER.into()))?;
            let version = take(version);
            if version.is_empty() {
                return Err(WebViewError::RuntimeMissing(BOOTSTRAPPER.into()));
            }
            let instance = GetModuleHandleW(None).map_err(|e| werr(&e))?;
            let class = WNDCLASSW {
                lpfnWndProc: Some(wndproc),
                hInstance: instance.into(),
                lpszClassName: w!("IstmoWebView"),
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                ..WNDCLASSW::default()
            };
            RegisterClassW(&raw const class);
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("IstmoWebView"),
                w!("istmo-webview"),
                WINDOW_STYLE::default(),
                0,
                0,
                0,
                0,
                Some(HWND_MESSAGE),
                None,
                Some(instance.into()),
                None,
            )
            .map_err(|e| werr(&e))?;
            Ok((hwnd, version))
        }
    };
    match setup() {
        Ok((hwnd, version)) => {
            drop(started.send(Ok((hwnd.0 as isize, version))));
        }
        Err(err) => {
            drop(started.send(Err(err)));
            return;
        }
    }
    let mut msg = MSG::default();
    // SAFETY: the standard message loop of this thread.
    unsafe {
        while GetMessageW(&raw mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&raw const msg);
            DispatchMessageW(&raw const msg);
        }
    }
}

/// Tasks for the message window; resizes and closes for view windows.
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_TASK => {
            // SAFETY: `lparam` is the `Box<UiTask>` leaked by `post`.
            let task = unsafe { Box::from_raw(lparam.0 as *mut UiTask) };
            task();
            LRESULT(0)
        }
        WM_SIZE => {
            STATE.with(|s| {
                if let Ok(state) = s.try_borrow() {
                    for view in state.views.values() {
                        if view.window == Some(hwnd.0 as isize) {
                            view.fit_window();
                        }
                    }
                }
            });
            LRESULT(0)
        }
        WM_CLOSE => {
            let id = STATE.with(|s| {
                s.try_borrow()
                    .ok()?
                    .views
                    .iter()
                    .find(|(_, v)| v.window == Some(hwnd.0 as isize))
                    .map(|(id, _)| *id)
            });
            if let Some(core) = id.and_then(core_of) {
                core.emit(crate::ControlEvent::CloseRequested);
            }
            // The app closes the view.
            LRESULT(0)
        }
        // SAFETY: default handling.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

impl Engine for WinEngine {
    fn describe(&self) -> (String, String) {
        ("webview2".into(), self.version.clone())
    }

    fn capabilities(&self, caps: &mut WebViewCapabilities) {
        caps.embedded = true;
        caps.windowed = true;
        caps.ephemeral_profiles = true;
        caps.persistent_profiles = true;
        caps.bridge_reply = true;
        caps.deferred_navigation = false;
        caps.snapshot = true;
        caps.print = true;
    }

    fn post(&self, task: UiTask) {
        let data = Box::into_raw(Box::new(task));
        // SAFETY: the window procedure takes ownership of `data`.
        let posted = unsafe {
            PostMessageW(
                Some(HWND(self.hwnd as *mut _)),
                WM_TASK,
                WPARAM(0),
                LPARAM(data as isize),
            )
        };
        if posted.is_err() {
            // SAFETY: the message was not queued; reclaim the task.
            drop(unsafe { Box::from_raw(data) });
        }
    }

    fn create(&self, core: Arc<ViewCore>, done: Done<()>) {
        let me = self.me.clone();
        self.post(Box::new(move || {
            let ready = STATE.with(|s| s.borrow().env.is_some());
            if ready {
                done(create(&core));
                return;
            }
            let first = STATE.with(|s| {
                let mut s = s.borrow_mut();
                s.waiting.push((Arc::clone(&core), done));
                s.config.get_or_insert_with(|| Arc::clone(&core.config));
                s.waiting.len() == 1
            });
            if first {
                start_environment(&core.config, me);
            }
        }));
    }

    fn with_view(&self, id: NativeHandleId, f: ViewTask) {
        self.post(Box::new(move || {
            let taken = STATE.with(|s| s.borrow_mut().views.remove(&id));
            match taken {
                Some(mut view) => {
                    f(Some(&mut view));
                    if view.closed {
                        view.destroy();
                        STATE.with(|s| s.borrow_mut().cores.remove(&id));
                    } else {
                        STATE.with(|s| s.borrow_mut().views.insert(id, view));
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
        Arc::new(WinFrameSink {
            id,
            state,
            engine: self.me.clone(),
        })
    }
}

fn data_folder() -> String {
    let base =
        std::env::var_os("LOCALAPPDATA").map_or_else(std::env::temp_dir, std::path::PathBuf::from);
    let app = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "istmo-app".into());
    base.join(app)
        .join("istmo-webview")
        .to_string_lossy()
        .into_owned()
}

/// Create the environment with the app's custom schemes registered.
fn start_environment(config: &WebViewInitConfig, _engine: Weak<WinEngine>) {
    let options = CoreWebView2EnvironmentOptions::default();
    let schemes: Vec<Option<ICoreWebView2CustomSchemeRegistration>> = config
        .schemes
        .iter()
        .map(|s| {
            let reg = CoreWebView2CustomSchemeRegistration::new(s.name.clone());
            // SAFETY: plain setters on a fresh registration.
            unsafe {
                reg.set_treat_as_secure(true);
                reg.set_has_authority_component(true);
            }
            Some(reg.into())
        })
        .collect();
    // SAFETY: setter on a fresh options object.
    unsafe { options.set_scheme_registrations(schemes) };
    let options: ICoreWebView2EnvironmentOptions = options.into();
    let handler =
        CreateCoreWebView2EnvironmentCompletedHandler::create(Box::new(move |result, env| {
            let waiting = STATE.with(|s| {
                let mut s = s.borrow_mut();
                s.env.clone_from(&env);
                std::mem::take(&mut s.waiting)
            });
            for (core, done) in waiting {
                done(match (&result, &env) {
                    (Ok(()), Some(_)) => create(&core),
                    (Err(e), _) => Err(werr(e)),
                    _ => Err(WebViewError::RuntimeMissing(BOOTSTRAPPER.into())),
                });
            }
            Ok(())
        }));
    let folder = hs(&data_folder());
    // SAFETY: arguments outlive the call; the handler is reference counted.
    let started = unsafe {
        CreateCoreWebView2EnvironmentWithOptions(PCWSTR::null(), &folder, &options, &handler)
    };
    if let Err(e) = started {
        let waiting = STATE.with(|s| std::mem::take(&mut s.borrow_mut().waiting));
        for (_, done) in waiting {
            done(Err(werr(&e)));
        }
    }
}

// =====================================================================
// Views
// =====================================================================

struct WinView {
    controller: ICoreWebView2Controller,
    web_view: ICoreWebView2,
    window: Option<isize>,
    closed: bool,
}

fn create(core: &Arc<ViewCore>) -> Result<(), WebViewError> {
    let env = STATE
        .with(|s| s.borrow().env.clone())
        .ok_or_else(|| WebViewError::RuntimeMissing(BOOTSTRAPPER.into()))?;
    let (parent, window) = match &core.spec.target {
        RenderTarget::Embedded(target) => {
            let native = istmo_window::WindowRegistry::global()
                .get(istmo_window::WindowId(target.window_id))
                .ok_or_else(|| {
                    WebViewError::InvalidArgument(format!(
                        "window {} is not registered",
                        target.window_id
                    ))
                })?;
            let istmo_window::NativeWindow::Win32 { hwnd } = native else {
                return Err(WebViewError::RenderModeUnsupported);
            };
            (HWND(hwnd.get() as *mut _), None)
        }
        RenderTarget::Window(target) => {
            let title = hs(&target.title);
            let w = if target.width == 0 {
                800
            } else {
                i32::try_from(target.width).unwrap_or(800)
            };
            let h = if target.height == 0 {
                600
            } else {
                i32::try_from(target.height).unwrap_or(600)
            };
            let mut style = WS_OVERLAPPEDWINDOW;
            if !target.resizable {
                style &= !(WS_THICKFRAME | WS_MAXIMIZEBOX);
            }
            // SAFETY: a top-level window of the class registered at start.
            let hwnd = unsafe {
                CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    w!("IstmoWebView"),
                    &title,
                    style,
                    CW_USEDEFAULT,
                    CW_USEDEFAULT,
                    w,
                    h,
                    None,
                    None,
                    GetModuleHandleW(None).ok().map(Into::into),
                    None,
                )
            }
            .map_err(|e| werr(&e))?;
            // SAFETY: freshly created window.
            unsafe {
                let _ = ShowWindow(hwnd, SW_SHOW);
            }
            (hwnd, Some(hwnd.0 as isize))
        }
    };
    let profile = core.spec.profile.clone();
    let owner = Arc::clone(core);
    let handler = CreateCoreWebView2ControllerCompletedHandler::create(Box::new(
        move |result, controller| {
            let attached = result.map_err(|e| werr(&e)).and_then(|()| {
                controller.ok_or_else(|| WebViewError::Backend("no controller".into()))
            });
            match attached.and_then(|c| attach(&owner, c, window)) {
                Ok(()) => {}
                Err(err) => {
                    owner.failed("", NavErrorKind::Other, &err.to_string());
                }
            }
            Ok(())
        },
    ));
    // SAFETY: COM calls on the environment's thread.
    unsafe {
        if let Ok(env10) = env.cast::<ICoreWebView2Environment10>() {
            let options = env10
                .CreateCoreWebView2ControllerOptions()
                .map_err(|e| werr(&e))?;
            apply_profile(&options, &profile)?;
            env10
                .CreateCoreWebView2ControllerWithOptions(parent, &options, &handler)
                .map_err(|e| werr(&e))?;
        } else {
            env.CreateCoreWebView2Controller(parent, &handler)
                .map_err(|e| werr(&e))?;
        }
    }
    Ok(())
}

fn apply_profile(
    options: &ICoreWebView2ControllerOptions,
    profile: &WebProfile,
) -> Result<(), WebViewError> {
    // SAFETY: setters on fresh controller options.
    unsafe {
        match profile {
            WebProfile::Shared => {}
            WebProfile::Ephemeral => {
                // A fresh InPrivate profile: nothing persists or is shared.
                let name = format!("istmo-ephemeral-{:016x}", crate::host::random_u64());
                options.SetProfileName(&hs(&name)).map_err(|e| werr(&e))?;
                options
                    .SetIsInPrivateModeEnabled(true)
                    .map_err(|e| werr(&e))?;
            }
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
                options.SetProfileName(&hs(&p.name)).map_err(|e| werr(&e))?;
            }
        }
    }
    Ok(())
}

fn attach(
    core: &Arc<ViewCore>,
    controller: ICoreWebView2Controller,
    window: Option<isize>,
) -> Result<(), WebViewError> {
    let id = core.id;
    // SAFETY: COM calls on the WebView2 thread.
    unsafe {
        let web_view = controller.CoreWebView2().map_err(|e| werr(&e))?;
        let settings = web_view.Settings().map_err(|e| werr(&e))?;
        let s = &core.spec.settings;
        settings
            .SetIsScriptEnabled(s.javascript)
            .map_err(|e| werr(&e))?;
        settings
            .SetIsWebMessageEnabled(core.spec.bridge.is_some())
            .map_err(|e| werr(&e))?;
        settings
            .SetAreDefaultContextMenusEnabled(s.context_menu == crate::ContextMenuMode::Native)
            .map_err(|e| werr(&e))?;
        settings
            .SetAreDevToolsEnabled(core.config.allow_inspector)
            .map_err(|e| werr(&e))?;
        settings
            .SetAreHostObjectsAllowed(false)
            .map_err(|e| werr(&e))?;
        settings
            .SetIsZoomControlEnabled(s.zoom_gestures)
            .map_err(|e| werr(&e))?;
        // Dialogs go to the app as decisions.
        settings
            .SetAreDefaultScriptDialogsEnabled(false)
            .map_err(|e| werr(&e))?;
        if let Ok(settings4) = settings.cast::<ICoreWebView2Settings4>() {
            settings4.SetIsPasswordAutosaveEnabled(s.autofill).ok();
            settings4.SetIsGeneralAutofillEnabled(s.autofill).ok();
        }
        if let Some(ua) = &s.user_agent
            && let Ok(settings2) = settings.cast::<ICoreWebView2Settings2>()
        {
            settings2.SetUserAgent(&hs(ua)).ok();
        }
        if s.transparent
            && let Ok(c2) = controller.cast::<ICoreWebView2Controller2>()
        {
            c2.SetDefaultBackgroundColor(COREWEBVIEW2_COLOR {
                A: 0,
                R: 0,
                G: 0,
                B: 0,
            })
            .ok();
        }
        for script in &core.spec.user_scripts {
            let source = if script.allowed_origins.is_empty() {
                script.source.clone()
            } else {
                super::text::origin_guard(&script.allowed_origins, &script.source)
            };
            let source = if script.main_frame_only {
                format!("if (window === window.top) {{\n{source}\n}}")
            } else {
                source
            };
            web_view
                .AddScriptToExecuteOnDocumentCreated(&hs(&source), None)
                .ok();
        }
        if let Some(bridge) = &core.spec.bridge
            && !core.bridge_origins().is_empty()
        {
            web_view
                .AddScriptToExecuteOnDocumentCreated(
                    &hs(&shim(core.bridge_origins(), bridge.allow_subframes)),
                    None,
                )
                .ok();
        }
        for scheme in &core.config.schemes {
            web_view
                .AddWebResourceRequestedFilter(
                    &hs(&format!("{}:*", scheme.name)),
                    COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL,
                )
                .ok();
        }
        connect(&web_view, id)?;
        let view = WinView {
            controller,
            web_view,
            window,
            closed: false,
        };
        if window.is_some() {
            view.fit_window();
        } else {
            view.controller.SetIsVisible(false).ok();
        }
        STATE.with(|s| {
            let mut s = s.borrow_mut();
            s.cores.insert(id, Arc::clone(core));
            s.views.insert(id, view);
        });
    }
    Ok(())
}

fn shim(origins: &[String], subframes: bool) -> String {
    let list = origins
        .iter()
        .map(|o| format!("\"{}\"", super::text::json_escape(o)))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        r#"(() => {{
  const port = window.chrome && window.chrome.webview;
  if (!port || window.istmo || ![{list}].includes(location.origin){top}) return;
  const listeners = new Set();
  const waiting = new Map();
  let next = 1;
  const toB64 = (d) => {{
    const bytes = typeof d === "string" ? new TextEncoder().encode(d) : d instanceof Uint8Array ? d : new Uint8Array(d);
    let bin = "";
    for (let i = 0; i < bytes.length; i += 0x8000) bin += String.fromCharCode.apply(null, bytes.subarray(i, i + 0x8000));
    return btoa(bin);
  }};
  const fromB64 = (b) => Uint8Array.from(atob(b), (c) => c.charCodeAt(0));
  port.addEventListener("message", (e) => {{
    const m = e.data;
    if (!m || typeof m !== "object") return;
    if (m.t === "r") {{
      const w = waiting.get(m.id);
      if (!w) return;
      waiting.delete(m.id);
      if (m.ok) w.resolve(fromB64(m.b)); else w.reject(new Error(m.e));
    }} else if (m.t === "m") {{
      listeners.forEach((f) => {{ try {{ f({{ channel: m.c, data: fromB64(m.b) }}); }} catch (err) {{ console.error(err); }} }});
    }}
  }});
  Object.defineProperty(window, "istmo", {{
    value: Object.freeze({{
      postMessage(channel, data) {{ port.postMessage(JSON.stringify({{ t: "m", c: String(channel), b: toB64(data ?? "") }})); }},
      invoke(channel, data) {{
        const id = next++;
        return new Promise((resolve, reject) => {{
          waiting.set(id, {{ resolve, reject }});
          port.postMessage(JSON.stringify({{ t: "i", id, c: String(channel), b: toB64(data ?? "") }}));
        }});
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

/// URL of the main document.
fn source(web_view: &ICoreWebView2) -> String {
    let mut uri = PWSTR::null();
    // SAFETY: out-parameter getter; WebView2 allocates the string.
    unsafe { web_view.Source(&raw mut uri).ok() };
    take(uri)
}

#[expect(clippy::too_many_lines, reason = "one registration per WebView2 event")]
fn connect(web_view: &ICoreWebView2, id: NativeHandleId) -> Result<(), WebViewError> {
    let mut token = 0_i64;
    // SAFETY: event registrations on the WebView2 thread; handlers only
    // run on this thread.
    unsafe {
        web_view
            .add_NavigationStarting(
                &NavigationStartingEventHandler::create(Box::new(move |_sender, args| {
                    let (Some(args), Some(core)) = (args, core_of(id)) else {
                        return Ok(());
                    };
                    let mut uri = PWSTR::null();
                    args.Uri(&raw mut uri)?;
                    let url = take(uri);
                    let mut gesture = BOOL::default();
                    args.IsUserInitiated(&raw mut gesture)?;
                    // WebView2 cannot defer a navigation: cancel now, and
                    // re-issue it if the app allows (`Ask`).
                    let decided = Arc::new(Mutex::new(None::<bool>));
                    let sync = Arc::clone(&decided);
                    let retry = url.clone();
                    core.navigation(
                        &url,
                        gesture.as_bool(),
                        Box::new(move |allow| {
                            let first = {
                                let mut slot = lock(&sync);
                                let first = slot.is_none();
                                if first {
                                    *slot = Some(allow);
                                }
                                first
                            };
                            if !first && allow {
                                with(id, |v| {
                                    v.web_view.Navigate(&hs(&retry)).ok();
                                });
                            }
                        }),
                    );
                    // Answered synchronously: use it. Not yet: cancel, and let
                    // the late answer re-issue the load.
                    let allowed = lock(&decided).get_or_insert(false).to_owned();
                    if !allowed {
                        args.SetCancel(true)?;
                    }
                    Ok(())
                })),
                &raw mut token,
            )
            .map_err(|e| werr(&e))?;
        web_view
            .add_ContentLoading(
                &ContentLoadingEventHandler::create(Box::new(move |sender, _args| {
                    if let (Some(sender), Some(core)) = (sender, core_of(id)) {
                        let url = source(&sender);
                        core.started(&url);
                        core.committed(&url);
                    }
                    Ok(())
                })),
                &raw mut token,
            )
            .map_err(|e| werr(&e))?;
        web_view
            .add_NavigationCompleted(
                &NavigationCompletedEventHandler::create(Box::new(move |sender, args| {
                    let (Some(sender), Some(args), Some(core)) = (sender, args, core_of(id)) else {
                        return Ok(());
                    };
                    let url = source(&sender);
                    let mut ok = BOOL::default();
                    args.IsSuccess(&raw mut ok)?;
                    if ok.as_bool() {
                        let (mut back, mut forward) = (BOOL::default(), BOOL::default());
                        sender.CanGoBack(&raw mut back)?;
                        sender.CanGoForward(&raw mut forward)?;
                        core.finished(&url, back.as_bool(), forward.as_bool());
                    } else {
                        let mut status = COREWEBVIEW2_WEB_ERROR_STATUS::default();
                        args.WebErrorStatus(&raw mut status)?;
                        let kind = match status {
                            COREWEBVIEW2_WEB_ERROR_STATUS_HOST_NAME_NOT_RESOLVED => {
                                NavErrorKind::Dns
                            }
                            COREWEBVIEW2_WEB_ERROR_STATUS_TIMEOUT => NavErrorKind::Timeout,
                            COREWEBVIEW2_WEB_ERROR_STATUS_OPERATION_CANCELED => {
                                NavErrorKind::Cancelled
                            }
                            COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_COMMON_NAME_IS_INCORRECT
                            | COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_EXPIRED
                            | COREWEBVIEW2_WEB_ERROR_STATUS_CLIENT_CERTIFICATE_CONTAINS_ERRORS
                            | COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_REVOKED
                            | COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_IS_INVALID => {
                                NavErrorKind::Tls
                            }
                            COREWEBVIEW2_WEB_ERROR_STATUS_DISCONNECTED
                            | COREWEBVIEW2_WEB_ERROR_STATUS_CANNOT_CONNECT
                            | COREWEBVIEW2_WEB_ERROR_STATUS_CONNECTION_ABORTED
                            | COREWEBVIEW2_WEB_ERROR_STATUS_CONNECTION_RESET => {
                                NavErrorKind::Network
                            }
                            _ => NavErrorKind::Other,
                        };
                        core.failed(&url, kind, "navigation failed");
                    }
                    Ok(())
                })),
                &raw mut token,
            )
            .map_err(|e| werr(&e))?;
        web_view
            .add_DocumentTitleChanged(
                &DocumentTitleChangedEventHandler::create(Box::new(move |sender, _| {
                    if let (Some(sender), Some(core)) = (sender, core_of(id)) {
                        let mut title = PWSTR::null();
                        sender.DocumentTitle(&raw mut title)?;
                        core.title(&take(title));
                    }
                    Ok(())
                })),
                &raw mut token,
            )
            .map_err(|e| werr(&e))?;
        web_view
            .add_WebMessageReceived(
                &WebMessageReceivedEventHandler::create(Box::new(move |_sender, args| {
                    let (Some(args), Some(core)) = (args, core_of(id)) else {
                        return Ok(());
                    };
                    let mut src = PWSTR::null();
                    args.Source(&raw mut src)?;
                    let origin = url::Url::parse(&take(src))
                        .ok()
                        .as_ref()
                        .and_then(crate::rules::origin_of)
                        .unwrap_or_default();
                    let mut raw = PWSTR::null();
                    if args.TryGetWebMessageAsString(&raw mut raw).is_err() {
                        return Ok(());
                    }
                    bridge_message(&core, id, &take(raw), origin);
                    Ok(())
                })),
                &raw mut token,
            )
            .map_err(|e| werr(&e))?;
        web_view
            .add_NewWindowRequested(
                &NewWindowRequestedEventHandler::create(Box::new(move |_sender, args| {
                    let (Some(args), Some(opener)) = (args, core_of(id)) else {
                        return Ok(());
                    };
                    let mut uri = PWSTR::null();
                    args.Uri(&raw mut uri)?;
                    let url = take(uri);
                    if !opener.allows_popup(&url) {
                        args.SetHandled(true)?;
                        return Ok(());
                    }
                    // Open as a new view with the opener's config.
                    args.SetHandled(true)?;
                    let mut spec = opener.spec.clone();
                    spec.target = RenderTarget::Window(crate::WindowTarget {
                        title: String::new(),
                        width: 0,
                        height: 0,
                        parent_window_id: None,
                        modal: false,
                        resizable: true,
                    });
                    if let Some((core, instance)) = opener.new_popup(spec)
                        && create(&core).is_ok()
                    {
                        let target = url.clone();
                        core.emit(crate::ControlEvent::NavigationStarted(crate::NavInfo {
                            navigation_id: 0,
                            url: target.clone(),
                            is_redirect: false,
                            user_gesture: false,
                        }));
                        opener.emit(crate::ControlEvent::NewView(NewViewInfo { instance, url }));
                        pending_load(core.id, &target);
                    }
                    Ok(())
                })),
                &raw mut token,
            )
            .map_err(|e| werr(&e))?;
        web_view
            .add_PermissionRequested(
                &PermissionRequestedEventHandler::create(Box::new(move |_sender, args| {
                    let (Some(args), Some(core)) = (args, core_of(id)) else { return Ok(()) };
                    let mut kind = COREWEBVIEW2_PERMISSION_KIND::default();
                    args.PermissionKind(&raw mut kind)?;
                    let permission = match kind {
                        COREWEBVIEW2_PERMISSION_KIND_CAMERA => WebPermission::Camera,
                        COREWEBVIEW2_PERMISSION_KIND_MICROPHONE => WebPermission::Microphone,
                        COREWEBVIEW2_PERMISSION_KIND_GEOLOCATION => WebPermission::Geolocation,
                        COREWEBVIEW2_PERMISSION_KIND_NOTIFICATIONS => WebPermission::Notifications,
                        COREWEBVIEW2_PERMISSION_KIND_CLIPBOARD_READ => WebPermission::Clipboard,
                        _ => WebPermission::Other,
                    };
                    let mut uri = PWSTR::null();
                    args.Uri(&raw mut uri)?;
                    let origin = url::Url::parse(&take(uri)).ok().as_ref().and_then(crate::rules::origin_of).unwrap_or_default();
                    let deferral = Ui(args.GetDeferral()?);
                    let held = Ui(args);
                    core.ask(
                        DecisionKind::Permission(PermissionAsk { permissions: vec![permission], frame_origin: origin.clone(), top_origin: core.committed_origin().unwrap_or(origin) }),
                        Box::new(move |response| {
                            let allow = matches!(response, DecisionResponse::Allow)
                                || matches!(&response, DecisionResponse::Grant(g) if g.contains(&permission));
                            // WebView2 thread (inside the handlers' unsafe context).
                            {
                                held.get().SetState(if allow { COREWEBVIEW2_PERMISSION_STATE_ALLOW } else { COREWEBVIEW2_PERMISSION_STATE_DENY }).ok();
                                deferral.get().Complete().ok();
                            }
                        }),
                        None,
                    );
                    Ok(())
                })),
                &raw mut token,
            )
            .map_err(|e| werr(&e))?;
        web_view
            .add_ScriptDialogOpening(
                &ScriptDialogOpeningEventHandler::create(Box::new(move |_sender, args| {
                    let (Some(args), Some(core)) = (args, core_of(id)) else {
                        return Ok(());
                    };
                    let mut kind = COREWEBVIEW2_SCRIPT_DIALOG_KIND::default();
                    args.Kind(&raw mut kind)?;
                    let kind = match kind {
                        COREWEBVIEW2_SCRIPT_DIALOG_KIND_CONFIRM => crate::DialogKind::Confirm,
                        COREWEBVIEW2_SCRIPT_DIALOG_KIND_PROMPT => crate::DialogKind::Prompt,
                        COREWEBVIEW2_SCRIPT_DIALOG_KIND_BEFOREUNLOAD => {
                            crate::DialogKind::BeforeUnload
                        }
                        _ => crate::DialogKind::Alert,
                    };
                    let (mut message, mut default, mut uri) =
                        (PWSTR::null(), PWSTR::null(), PWSTR::null());
                    args.Message(&raw mut message)?;
                    args.DefaultText(&raw mut default)?;
                    args.Uri(&raw mut uri)?;
                    let origin = url::Url::parse(&take(uri))
                        .ok()
                        .as_ref()
                        .and_then(crate::rules::origin_of)
                        .unwrap_or_default();
                    let deferral = Ui(args.GetDeferral()?);
                    let held = Ui(args);
                    core.ask(
                        DecisionKind::Dialog(crate::DialogAsk {
                            kind,
                            message: take(message),
                            default_text: take(default),
                            origin,
                        }),
                        Box::new(move |response| {
                            // WebView2 thread (inside the handlers' unsafe context).
                            {
                                match response {
                                    DecisionResponse::DialogAccept(text) => {
                                        if let Some(text) = text {
                                            held.get().SetResultText(&hs(&text)).ok();
                                        }
                                        held.get().Accept().ok();
                                    }
                                    DecisionResponse::Allow => {
                                        held.get().Accept().ok();
                                    }
                                    _ => {}
                                }
                                deferral.get().Complete().ok();
                            }
                        }),
                        None,
                    );
                    Ok(())
                })),
                &raw mut token,
            )
            .map_err(|e| werr(&e))?;
        web_view
            .add_WebResourceRequested(
                &WebResourceRequestedEventHandler::create(Box::new(move |_sender, args| {
                    let (Some(args), Some(core)) = (args, core_of(id)) else {
                        return Ok(());
                    };
                    scheme_request(&core, &args)?;
                    Ok(())
                })),
                &raw mut token,
            )
            .map_err(|e| werr(&e))?;
        web_view
            .add_ProcessFailed(
                &ProcessFailedEventHandler::create(Box::new(move |_sender, _args| {
                    if let Some(core) = core_of(id) {
                        core.fail_pending();
                        core.emit(crate::ControlEvent::ProcessTerminated(TerminationInfo {
                            reason: TerminationReason::Crashed,
                        }));
                    }
                    Ok(())
                })),
                &raw mut token,
            )
            .map_err(|e| werr(&e))?;
        web_view
            .add_WindowCloseRequested(
                &WindowCloseRequestedEventHandler::create(Box::new(move |_sender, _args| {
                    if let Some(core) = core_of(id) {
                        core.emit(crate::ControlEvent::CloseRequested);
                    }
                    Ok(())
                })),
                &raw mut token,
            )
            .map_err(|e| werr(&e))?;
    }
    Ok(())
}

/// A page-opened view loads its URL once its controller exists.
fn pending_load(id: NativeHandleId, url: &str) {
    with(id, |v| {
        // SAFETY: WebView2 thread.
        unsafe { v.web_view.Navigate(&hs(url)).ok() };
    });
}

/// Parse a shim message (`{"t","c","b"[,"id"]}`) and hand it to the core.
fn bridge_message(core: &Arc<ViewCore>, id: NativeHandleId, raw: &str, origin: String) {
    let field = |key: &str| -> Option<String> { super::text::json_field(raw, key) };
    let (Some(kind), Some(channel), Some(b64)) = (field("t"), field("c"), field("b")) else {
        return;
    };
    let Some(data) = super::text::unbase64(&b64) else {
        return;
    };
    let document = core.document_id();
    if kind == "i" {
        let Some(call) = super::text::json_number(raw, "id") else {
            return;
        };
        core.bridge_message(
            channel,
            origin,
            true,
            document,
            data,
            Some(Box::new(move |result| {
                let json = match result {
                    ReplyResult::Ok(bytes) => format!(
                        "{{\"t\":\"r\",\"id\":{call},\"ok\":true,\"b\":\"{}\"}}",
                        super::text::base64(&bytes)
                    ),
                    ReplyResult::Err(message) => format!(
                        "{{\"t\":\"r\",\"id\":{call},\"ok\":false,\"e\":\"{}\"}}",
                        super::text::json_escape(&message)
                    ),
                };
                with(id, |v| {
                    // SAFETY: WebView2 thread.
                    unsafe { v.web_view.PostWebMessageAsJson(&hs(&json)).ok() };
                });
            })),
        );
    } else {
        core.bridge_message(channel, origin, true, document, data, None);
    }
}

// =====================================================================
// Custom schemes (responses are buffered: WebView2 takes one IStream)
// =====================================================================

fn memory_stream(bytes: &[u8]) -> Option<IStream> {
    // SAFETY: `SHCreateMemStream` copies `bytes`.
    unsafe { SHCreateMemStream(Some(bytes)) }
}

fn headers_text(mime: &str, extra: &[crate::HttpHeader]) -> String {
    let mut out = format!(
        "Content-Type: {mime}\r\nX-Content-Type-Options: nosniff\r\nCache-Control: no-cache"
    );
    for h in extra {
        if h.name.eq_ignore_ascii_case("access-control-allow-origin") && h.value == "*" {
            continue;
        }
        write!(out, "\r\n{}: {}", h.name, h.value).ok();
    }
    out
}

fn respond(
    args: &ICoreWebView2WebResourceRequestedEventArgs,
    status: u16,
    mime: &str,
    headers: &[crate::HttpHeader],
    body: &[u8],
) {
    let Some(env) = STATE.with(|s| s.borrow().env.clone()) else {
        return;
    };
    let stream = memory_stream(body);
    // SAFETY: WebView2 thread; the response takes the stream.
    unsafe {
        if let Ok(response) = env.CreateWebResourceResponse(
            stream.as_ref(),
            i32::from(status),
            &hs("OK"),
            &hs(&headers_text(mime, headers)),
        ) {
            args.SetResponse(&response).ok();
        }
    }
}

fn scheme_request(
    core: &Arc<ViewCore>,
    args: &ICoreWebView2WebResourceRequestedEventArgs,
) -> windows::core::Result<()> {
    // SAFETY: WebView2 thread.
    let (url, method) = unsafe {
        let request = args.Request()?;
        let (mut uri, mut method) = (PWSTR::null(), PWSTR::null());
        request.Uri(&raw mut uri)?;
        request.Method(&raw mut method)?;
        (take(uri), take(method))
    };
    let Ok(parsed) = url::Url::parse(&url) else {
        return Ok(());
    };
    let Some(scheme) = core
        .config
        .schemes
        .iter()
        .find(|s| s.name == parsed.scheme())
    else {
        return Ok(());
    };
    let own_origin = format!(
        "{}://{}",
        parsed.scheme(),
        parsed.host_str().unwrap_or_default()
    );
    if core.committed_origin().is_some_and(|o| o != own_origin) {
        respond(args, 403, "text/plain", &[], b"");
        return Ok(());
    }
    match &scheme.source {
        SchemeSource::Assets(source) => match super::read_asset(&source.dir, parsed.path()) {
            Ok((mime, bytes)) => respond(args, 200, mime, &[], &bytes),
            Err(status) => respond(args, status, "text/plain", &[], b""),
        },
        SchemeSource::Dynamic => {
            // SAFETY: WebView2 thread.
            let deferral = unsafe { args.GetDeferral()? };
            core.scheme_request(
                SchemeRequestInfo {
                    request_id: 0,
                    scheme: scheme.name.clone(),
                    method,
                    url,
                    headers: Vec::new(),
                    has_body: false,
                    initiator_origin: None,
                },
                Box::new(WinResponder {
                    args: Some(Ui((args.clone(), deferral))),
                    head: None,
                    body: Vec::new(),
                }),
            );
        }
    }
    Ok(())
}

struct WinResponder {
    args: Option<
        Ui<(
            ICoreWebView2WebResourceRequestedEventArgs,
            ICoreWebView2Deferral,
        )>,
    >,
    head: Option<SchemeResponseHead>,
    body: Vec<u8>,
}

impl SchemeResponder for WinResponder {
    fn head(&mut self, head: &SchemeResponseHead) {
        self.head = Some(head.clone());
    }

    fn body(&mut self, body: SchemeBody) {
        match body {
            SchemeBody::Data(data) => self.body.extend_from_slice(&data),
            SchemeBody::End | SchemeBody::Abort(_) => {
                let Some(request) = self.args.take() else {
                    return;
                };
                let (args, deferral) = request.get();
                let aborted = matches!(body, SchemeBody::Abort(_));
                match (&self.head, aborted) {
                    (Some(head), false) => respond(
                        args,
                        head.status,
                        &head.mime_type,
                        &head.headers,
                        &self.body,
                    ),
                    _ => respond(args, 502, "text/plain", &[], b""),
                }
                // SAFETY: WebView2 thread.
                unsafe { deferral.Complete().ok() };
            }
        }
    }
}

// =====================================================================
// UiView
// =====================================================================

impl WinView {
    fn fit_window(&self) {
        let Some(hwnd) = self.window else { return };
        let mut rect = RECT::default();
        // SAFETY: our own window.
        unsafe {
            if GetClientRect(HWND(hwnd as *mut _), &raw mut rect).is_ok() {
                self.controller.SetBounds(rect).ok();
            }
        }
    }

    fn apply_frame(&self, frame: ViewFrame) {
        if self.window.is_some() {
            return;
        }
        // SAFETY: WebView2 thread.
        unsafe {
            if !frame.visible || frame.width == 0 || frame.height == 0 {
                self.controller.SetIsVisible(false).ok();
                return;
            }
            let right = frame
                .x
                .saturating_add(i32::try_from(frame.width).unwrap_or(i32::MAX));
            let bottom = frame
                .y
                .saturating_add(i32::try_from(frame.height).unwrap_or(i32::MAX));
            self.controller
                .SetBounds(RECT {
                    left: frame.x,
                    top: frame.y,
                    right,
                    bottom,
                })
                .ok();
            self.controller.SetIsVisible(true).ok();
        }
    }

    fn destroy(&mut self) {
        // SAFETY: WebView2 thread.
        unsafe {
            self.controller.Close().ok();
            if let Some(hwnd) = self.window.take() {
                DestroyWindow(HWND(hwnd as *mut _)).ok();
            }
        }
    }

    fn script(&self, source: &str, done: Option<Done<String>>) {
        let done = Ui(RefCell::new(done));
        let handler = ExecuteScriptCompletedHandler::create(Box::new(move |result, json| {
            if let Some(done) = done.get().borrow_mut().take() {
                done(result.map(|()| json).map_err(|e| werr(&e)));
            }
            Ok(())
        }));
        // SAFETY: WebView2 thread.
        unsafe { self.web_view.ExecuteScript(&hs(source), &handler).ok() };
    }
}

impl UiView for WinView {
    fn load(&mut self, request: &LoadRequest) {
        // SAFETY: WebView2 thread.
        unsafe {
            if request.headers.is_empty() && request.post_body.is_none() {
                self.web_view.Navigate(&hs(&request.url)).ok();
                return;
            }
            let (Some(env), Ok(wv2)) = (
                STATE.with(|s| s.borrow().env.clone()),
                self.web_view.cast::<ICoreWebView2_2>(),
            ) else {
                self.web_view.Navigate(&hs(&request.url)).ok();
                return;
            };
            let Ok(env2) = env.cast::<ICoreWebView2Environment2>() else {
                return;
            };
            let headers = request
                .headers
                .iter()
                .map(|h| format!("{}: {}", h.name, h.value))
                .collect::<Vec<_>>()
                .join("\r\n");
            let method = if request.post_body.is_some() {
                "POST"
            } else {
                "GET"
            };
            let body = request.post_body.as_deref().and_then(memory_stream);
            if let Ok(req) = env2.CreateWebResourceRequest(
                &hs(&request.url),
                &hs(method),
                body.as_ref(),
                &hs(&headers),
            ) {
                wv2.NavigateWithWebResourceRequest(&req).ok();
            }
        }
    }

    fn load_html(&mut self, html: &str, _base_url: Option<&str>) {
        // WebView2 shows strings at `about:blank`-like origins only.
        // SAFETY: WebView2 thread.
        unsafe { self.web_view.NavigateToString(&hs(html)).ok() };
    }

    fn navigate(&mut self, nav: HistoryNav) {
        // SAFETY: WebView2 thread.
        unsafe {
            match nav {
                HistoryNav::Back => drop(self.web_view.GoBack()),
                HistoryNav::Forward => drop(self.web_view.GoForward()),
                HistoryNav::Reload | HistoryNav::ReloadBypassCache => drop(self.web_view.Reload()),
                HistoryNav::Stop => drop(self.web_view.Stop()),
                HistoryNav::Offset(n) => {
                    let step = if n < 0 {
                        "history.back()"
                    } else {
                        "history.forward()"
                    };
                    for _ in 0..n.unsigned_abs() {
                        self.script(step, None);
                    }
                }
            }
        }
    }

    fn history(&mut self) -> WebHistory {
        // WebView2 does not expose the list; report the current entry.
        WebHistory {
            entries: vec![crate::HistoryEntry {
                url: source(&self.web_view),
                title: String::new(),
            }],
            current: 0,
        }
    }

    fn focus(&mut self) {
        // SAFETY: WebView2 thread.
        unsafe {
            self.controller
                .MoveFocus(COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC)
                .ok()
        };
    }

    fn eval(&mut self, call: &ScriptCall, done: Done<String>) {
        if call.world == ScriptWorld::Isolated {
            return done(Err(WebViewError::Unsupported(
                "isolated script worlds on WebView2".into(),
            )));
        }
        // `ExecuteScript` does not await promises: the DevTools
        // `Runtime.evaluate` call does, and returns the value as JSON.
        let params = format!(
            "{{\"expression\":\"{}\",\"awaitPromise\":true,\"returnByValue\":true}}",
            super::text::json_escape(&call.source)
        );
        let done = Ui(RefCell::new(Some(done)));
        let handler =
            CallDevToolsProtocolMethodCompletedHandler::create(Box::new(move |result, json| {
                if let Some(done) = done.get().borrow_mut().take() {
                    done(match result {
                        Ok(()) => super::text::json_value_of(&json, "value").ok_or_else(|| {
                            WebViewError::Backend(
                                super::text::json_field(&json, "description")
                                    .unwrap_or_else(|| "script failed".into()),
                            )
                        }),
                        Err(e) => Err(werr(&e)),
                    });
                }
                Ok(())
            }));
        // SAFETY: WebView2 thread.
        unsafe {
            self.web_view
                .CallDevToolsProtocolMethod(w!("Runtime.evaluate"), &hs(&params), &handler)
                .ok()
        };
    }

    fn deliver(&mut self, channel: &str, data: &[u8]) {
        let json = format!(
            "{{\"t\":\"m\",\"c\":\"{}\",\"b\":\"{}\"}}",
            super::text::json_escape(channel),
            super::text::base64(data)
        );
        // SAFETY: WebView2 thread.
        unsafe { self.web_view.PostWebMessageAsJson(&hs(&json)).ok() };
    }

    fn cookies(&mut self, filter: &CookieFilter, done: Done<Vec<WebCookie>>) {
        let Ok(wv2) = self.web_view.cast::<ICoreWebView2_2>() else {
            return done(Err(WebViewError::Unsupported(
                "cookies need a newer WebView2".into(),
            )));
        };
        let name = filter.name.clone();
        let done = Ui(RefCell::new(Some(done)));
        let handler = GetCookiesCompletedHandler::create(Box::new(move |result, list| {
            let Some(done) = done.get().borrow_mut().take() else {
                return Ok(());
            };
            if let Err(e) = result {
                done(Err(werr(&e)));
                return Ok(());
            }
            let mut out = Vec::new();
            if let Some(list) = list {
                let mut count = 0_u32;
                // SAFETY: WebView2 thread.
                unsafe {
                    list.Count(&raw mut count)?;
                    for i in 0..count {
                        let cookie = list.GetValueAtIndex(i)?;
                        let (mut n, mut v, mut d, mut p) =
                            (PWSTR::null(), PWSTR::null(), PWSTR::null(), PWSTR::null());
                        cookie.Name(&raw mut n)?;
                        cookie.Value(&raw mut v)?;
                        cookie.Domain(&raw mut d)?;
                        cookie.Path(&raw mut p)?;
                        let (mut secure, mut http_only, mut session) =
                            (BOOL::default(), BOOL::default(), BOOL::default());
                        cookie.IsSecure(&raw mut secure)?;
                        cookie.IsHttpOnly(&raw mut http_only)?;
                        cookie.IsSession(&raw mut session)?;
                        let mut expires = 0.0_f64;
                        cookie.Expires(&raw mut expires)?;
                        let item = WebCookie {
                            name: take(n),
                            value: take(v),
                            domain: take(d),
                            path: take(p),
                            #[expect(
                                clippy::cast_possible_truncation,
                                reason = "cookie expiry seconds fit an i64 in milliseconds"
                            )]
                            expires_unix_ms: (!session.as_bool())
                                .then_some((expires * 1000.0) as i64),
                            secure: secure.as_bool(),
                            http_only: http_only.as_bool(),
                            same_site: SameSite::Lax,
                        };
                        if name.as_ref().is_none_or(|n| *n == item.name) {
                            out.push(item);
                        }
                    }
                }
            }
            done(Ok(out));
            Ok(())
        }));
        let uri = filter.url.clone().unwrap_or_default();
        // SAFETY: WebView2 thread.
        unsafe {
            if let Ok(manager) = wv2.CookieManager() {
                manager.GetCookies(&hs(&uri), &handler).ok();
            }
        }
    }

    fn set_cookie(&mut self, cookie: &WebCookie, done: Done<()>) {
        let Ok(wv2) = self.web_view.cast::<ICoreWebView2_2>() else {
            return done(Err(WebViewError::Unsupported(
                "cookies need a newer WebView2".into(),
            )));
        };
        // SAFETY: WebView2 thread.
        let result = unsafe {
            wv2.CookieManager().and_then(|manager| {
                let c = manager.CreateCookie(
                    &hs(&cookie.name),
                    &hs(&cookie.value),
                    &hs(&cookie.domain),
                    &hs(if cookie.path.is_empty() {
                        "/"
                    } else {
                        &cookie.path
                    }),
                )?;
                c.SetIsSecure(cookie.secure)?;
                c.SetIsHttpOnly(cookie.http_only)?;
                if let Some(ms) = cookie.expires_unix_ms {
                    #[expect(
                        clippy::cast_precision_loss,
                        reason = "millisecond timestamps fit a double"
                    )]
                    c.SetExpires(ms as f64 / 1000.0)?;
                }
                c.SetSameSite(match cookie.same_site {
                    SameSite::Lax => COREWEBVIEW2_COOKIE_SAME_SITE_KIND_LAX,
                    SameSite::Strict => COREWEBVIEW2_COOKIE_SAME_SITE_KIND_STRICT,
                    SameSite::None => COREWEBVIEW2_COOKIE_SAME_SITE_KIND_NONE,
                })?;
                manager.AddOrUpdateCookie(&c)
            })
        };
        done(result.map_err(|e| werr(&e)));
    }

    fn clear_data(&mut self, request: &ClearDataRequest, done: Done<()>) {
        let mut kinds = COREWEBVIEW2_BROWSING_DATA_KINDS(0);
        for kind in &request.kinds {
            kinds.0 |= match kind {
                DataKind::Cookies => COREWEBVIEW2_BROWSING_DATA_KINDS_COOKIES.0,
                DataKind::Cache => {
                    COREWEBVIEW2_BROWSING_DATA_KINDS_DISK_CACHE.0
                        | COREWEBVIEW2_BROWSING_DATA_KINDS_CACHE_STORAGE.0
                }
                DataKind::LocalStorage => COREWEBVIEW2_BROWSING_DATA_KINDS_LOCAL_STORAGE.0,
                DataKind::IndexedDb => COREWEBVIEW2_BROWSING_DATA_KINDS_INDEXED_DB.0,
                DataKind::ServiceWorkers => COREWEBVIEW2_BROWSING_DATA_KINDS_SERVICE_WORKERS.0,
                DataKind::All => COREWEBVIEW2_BROWSING_DATA_KINDS_ALL_PROFILE.0,
            };
        }
        let profile = self
            .web_view
            .cast::<ICoreWebView2_13>()
            .ok()
            .and_then(|wv| {
                // SAFETY: WebView2 thread.
                unsafe { wv.Profile() }.ok()
            })
            .and_then(|p| p.cast::<ICoreWebView2Profile2>().ok());
        let Some(profile) = profile else {
            return done(Err(WebViewError::Unsupported(
                "clearing data needs a newer WebView2".into(),
            )));
        };
        let done = Ui(RefCell::new(Some(done)));
        let handler = ClearBrowsingDataCompletedHandler::create(Box::new(move |result| {
            if let Some(done) = done.get().borrow_mut().take() {
                done(result.map_err(|e| werr(&e)));
            }
            Ok(())
        }));
        // SAFETY: WebView2 thread.
        unsafe { profile.ClearBrowsingData(kinds, &handler).ok() };
    }

    fn set_zoom(&mut self, factor: f32) {
        // SAFETY: WebView2 thread.
        unsafe { self.controller.SetZoomFactor(f64::from(factor)).ok() };
    }

    fn find(&mut self, _query: &FindQuery, done: Done<FindResult>) {
        done(Err(WebViewError::Unsupported("find on WebView2".into())));
    }

    fn snapshot(&mut self, options: SnapshotOptions, done: Done<WebSnapshot>) {
        let format = match options.format {
            SnapshotFormat::Png => COREWEBVIEW2_CAPTURE_PREVIEW_IMAGE_FORMAT_PNG,
            SnapshotFormat::Jpeg(_) => COREWEBVIEW2_CAPTURE_PREVIEW_IMAGE_FORMAT_JPEG,
        };
        let Some(stream) = memory_stream(&[]) else {
            return done(Err(WebViewError::Backend("no stream".into())));
        };
        let held = Ui(stream.clone());
        let done = Ui(RefCell::new(Some(done)));
        let handler = CapturePreviewCompletedHandler::create(Box::new(move |result| {
            let Some(done) = done.get().borrow_mut().take() else {
                return Ok(());
            };
            done(
                result
                    .map_err(|e| werr(&e))
                    .and_then(|()| read_stream(held.get()))
                    .map(|bytes| WebSnapshot {
                        width: 0,
                        height: 0,
                        format: options.format,
                        bytes,
                    }),
            );
            Ok(())
        }));
        // SAFETY: WebView2 thread.
        unsafe { self.web_view.CapturePreview(format, &stream, &handler).ok() };
    }

    fn print(&mut self) -> Result<(), WebViewError> {
        let wv16 = self
            .web_view
            .cast::<ICoreWebView2_16>()
            .map_err(|_| WebViewError::Unsupported("printing needs a newer WebView2".into()))?;
        // SAFETY: WebView2 thread.
        unsafe { wv16.ShowPrintUI(COREWEBVIEW2_PRINT_DIALOG_KIND_BROWSER) }.map_err(|e| werr(&e))
    }

    fn set_inspector(&mut self, open: bool) -> Result<(), WebViewError> {
        if open {
            // SAFETY: WebView2 thread.
            unsafe { self.web_view.OpenDevToolsWindow() }.map_err(|e| werr(&e))?;
        }
        Ok(())
    }

    fn close(&mut self) {
        self.closed = true;
    }
}

fn read_stream(stream: &IStream) -> Result<Vec<u8>, WebViewError> {
    use windows::Win32::System::Com::STREAM_SEEK_SET;
    let mut out = Vec::new();
    let mut buf = [0_u8; 16 * 1024];
    // SAFETY: reading our own memory stream.
    unsafe {
        stream
            .Seek(0, STREAM_SEEK_SET, None)
            .map_err(|e| werr(&e))?;
        loop {
            let mut read = 0_u32;
            let hr = stream.Read(
                buf.as_mut_ptr().cast(),
                u32::try_from(buf.len()).unwrap_or(u32::MAX),
                Some(&raw mut read),
            );
            if hr.is_err() || read == 0 {
                break;
            }
            out.extend_from_slice(&buf[..read as usize]);
        }
    }
    Ok(out)
}
