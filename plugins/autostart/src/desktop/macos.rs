//! macOS: `SMAppService.mainApp` (macOS 13+, inside an `.app` bundle),
//! a `LaunchAgent` plist otherwise; login starts recognised by the
//! `oapp` Apple Event's `keyAELaunchedAsLogInItem`.
//!
//! The Apple Event is only readable while it is being handled, which is
//! when `NSApplication` posts `NSApplicationDidFinishLaunchingNotification`.
//! The publisher therefore observes that notification and publishes the
//! reason from there.

#![expect(
    unsafe_code,
    reason = "Objective-C messages to Foundation and ServiceManagement"
)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, Weak};

use istmo_core::Runtime;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, NSObject};
use objc2::{ClassType, define_class, msg_send, sel};
use objc2_service_management::{SMAppService, SMAppServiceStatus};

use super::{
    LaunchCommand, Signal, decide, ensure_marker, io_error, plist, remove_if_exists, write_atomic,
};
use crate::{AutostartConfig, AutostartError, AutostartState, Detection, LaunchReason};

/// `kAEOpenApplication` (`'oapp'`).
const OPEN_APPLICATION: u32 = u32::from_be_bytes(*b"oapp");
/// `keyAEPropData` (`'prdt'`).
const PROP_DATA: u32 = u32::from_be_bytes(*b"prdt");
/// `keyAELaunchedAsLogInItem` (`'lgit'`).
const LAUNCHED_AS_LOGIN_ITEM: u32 = u32::from_be_bytes(*b"lgit");

/// Login Items in System Settings (macOS 13+) and the Users & Groups
/// pane of System Preferences before.
const LOGIN_ITEMS_URL: &str = "x-apple.systempreferences:com.apple.LoginItems-Settings.extension";
const USERS_PANE: &str = "/System/Library/PreferencePanes/Accounts.prefPane";

#[derive(Debug, Clone)]
pub(crate) enum Backend {
    /// `SMAppService.mainApp`.
    AppService,
    /// `~/Library/LaunchAgents/<label>.plist`.
    LaunchAgent { plist: PathBuf, label: String },
    /// No home directory.
    Unsupported,
}

impl Backend {
    pub(crate) fn detect(config: &AutostartConfig) -> Self {
        if has_app_service() && in_app_bundle() {
            return Self::AppService;
        }
        let Some(home) = std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|home| home.is_absolute())
        else {
            return Self::Unsupported;
        };
        let label = format!("{}.istmo-autostart", config.app_id());
        let mut plist = home.join("Library/LaunchAgents");
        plist.push(format!("{label}.plist"));
        Self::LaunchAgent { plist, label }
    }

    pub(crate) fn status(
        &self,
        _config: &AutostartConfig,
    ) -> Result<AutostartState, AutostartError> {
        match self {
            Self::AppService => Ok(app_service_state()),
            Self::LaunchAgent { plist, .. } => match std::fs::read_to_string(plist) {
                Ok(text) if plist::program_arguments(&text).is_some() => {
                    Ok(AutostartState::Enabled)
                }
                Ok(_) => Ok(AutostartState::Disabled),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                    Ok(AutostartState::Disabled)
                }
                Err(err) => Err(io_error("read", plist, &err)),
            },
            Self::Unsupported => Ok(AutostartState::Unsupported),
        }
    }

    pub(crate) fn enable(
        &self,
        _config: &AutostartConfig,
        command: &LaunchCommand,
    ) -> Result<AutostartState, AutostartError> {
        match self {
            Self::AppService => {
                match app_service_state() {
                    AutostartState::RequiresApproval => {
                        self.open_settings()?;
                        return Ok(AutostartState::RequiresApproval);
                    }
                    AutostartState::Enabled => return Ok(AutostartState::Enabled),
                    _ => {}
                }
                // SAFETY: `+mainAppService` returns the shared instance;
                // `-registerAndReturnError:` takes no arguments besides
                // the error out-pointer the binding manages.
                let registered = unsafe { SMAppService::mainAppService().registerAndReturnError() };
                let state = app_service_state();
                match registered {
                    Ok(()) => Ok(state),
                    // Registered but waiting for the user: not a failure.
                    Err(_) if state == AutostartState::RequiresApproval => Ok(state),
                    Err(err) => Err(AutostartError::Backend(format!(
                        "SMAppService register: {}",
                        err.localizedDescription()
                    ))),
                }
            }
            Self::LaunchAgent { plist, label } => {
                let text =
                    plist::render(label, command).ok_or_else(|| non_utf8(&command.program))?;
                write_atomic(plist, text.as_bytes()).map_err(|e| io_error("write", plist, &e))?;
                Ok(AutostartState::Enabled)
            }
            Self::Unsupported => Err(AutostartError::Unsupported),
        }
    }

    pub(crate) fn disable(
        &self,
        _config: &AutostartConfig,
    ) -> Result<AutostartState, AutostartError> {
        match self {
            Self::AppService => {
                // SAFETY: as in `enable`.
                let unregistered =
                    unsafe { SMAppService::mainAppService().unregisterAndReturnError() };
                let state = app_service_state();
                match unregistered {
                    Err(err) if state.is_enabled() => Err(AutostartError::Backend(format!(
                        "SMAppService unregister: {}",
                        err.localizedDescription()
                    ))),
                    // Not registered in the first place is fine.
                    Ok(()) | Err(_) => Ok(state),
                }
            }
            Self::LaunchAgent { plist, .. } => {
                remove_if_exists(plist).map_err(|e| io_error("remove", plist, &e))?;
                Ok(AutostartState::Disabled)
            }
            Self::Unsupported => Err(AutostartError::Unsupported),
        }
    }

    #[expect(clippy::unused_self, reason = "same surface as the other backends")]
    pub(crate) fn open_settings(&self) -> Result<(), AutostartError> {
        if has_app_service() {
            // SAFETY: a class method without arguments; the class exists
            // (checked above, macOS 13+).
            unsafe { SMAppService::openSystemSettingsLoginItems() };
            return Ok(());
        }
        let target = if Path::new(USERS_PANE).exists() {
            USERS_PANE
        } else {
            LOGIN_ITEMS_URL
        };
        Command::new("/usr/bin/open")
            .arg(target)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map(drop)
            .map_err(|e| AutostartError::Backend(format!("open {target}: {e}")))
    }

    /// Point an existing `LaunchAgent` at `program` and make sure it
    /// carries `marker`. `SMAppService` follows the bundle by itself.
    pub(crate) fn refresh(
        &self,
        _config: &AutostartConfig,
        program: &Path,
        marker: Option<&str>,
    ) -> Result<bool, AutostartError> {
        let Self::LaunchAgent { plist, label } = self else {
            return Ok(false);
        };
        let text = match std::fs::read_to_string(plist) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(err) => return Err(io_error("read", plist, &err)),
        };
        let Some(stored) = plist::program_arguments(&text) else {
            return Ok(false);
        };
        let mut exec = stored.into_iter();
        let old_program = exec.next();
        let mut args: Vec<String> = exec.collect();
        let before = args.len();
        ensure_marker(&mut args, marker);
        if old_program.as_deref().map(Path::new) == Some(program) && args.len() == before {
            return Ok(false);
        }
        let command = LaunchCommand {
            program: program.to_path_buf(),
            args,
        };
        let text = plist::render(label, &command).ok_or_else(|| non_utf8(program))?;
        write_atomic(plist, text.as_bytes()).map_err(|e| io_error("write", plist, &e))?;
        Ok(true)
    }
}

fn app_service_state() -> AutostartState {
    // SAFETY: `+mainAppService` returns the shared instance; `-status`
    // takes no arguments.
    let status = unsafe { SMAppService::mainAppService().status() };
    match status {
        SMAppServiceStatus::Enabled => AutostartState::Enabled,
        SMAppServiceStatus::RequiresApproval => AutostartState::RequiresApproval,
        // `NotRegistered`, `NotFound` (no such bundle), newer values.
        _ => AutostartState::Disabled,
    }
}

/// `SMAppService` is macOS 13+; the class is looked up at run time.
fn has_app_service() -> bool {
    AnyClass::get(c"SMAppService").is_some()
}

/// `SMAppService.mainApp` describes the enclosing `.app`; a bare binary
/// (`cargo run`) has none.
fn in_app_bundle() -> bool {
    std::env::current_exe().is_ok_and(|exe| {
        exe.parent()
            .and_then(|dir| dir.to_str())
            .is_some_and(|dir| dir.ends_with(".app/Contents/MacOS"))
    })
}

fn non_utf8(path: &Path) -> AutostartError {
    AutostartError::Backend(format!("executable path {} is not UTF-8", path.display()))
}

/// What the observer needs to publish once `NSApplication` finished
/// launching.
#[derive(Debug)]
struct Pending {
    runtime: Weak<Runtime>,
    marker_configured: bool,
    /// The `LaunchAgent` starts the app without a login-item event: only
    /// the marker tells.
    launch_agent: bool,
}

static PENDING: Mutex<Option<Pending>> = Mutex::new(None);

define_class!(
    // SAFETY: `NSObject` has no subclassing requirements and `Observer`
    // does not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[name = "IstmoAutostartObserver"]
    struct Observer;

    impl Observer {
        #[unsafe(method(istmoDidFinishLaunching:))]
        fn did_finish_launching(&self, _notification: &AnyObject) {
            let Some(pending) = istmo_core::sync::lock(&PENDING).take() else {
                return;
            };
            let Some(runtime) = pending.runtime.upgrade() else {
                return;
            };
            let signal = match (launched_as_login_item(), pending.launch_agent) {
                (true, _) => Signal::Login,
                (false, false) => Signal::Absent,
                (false, true) => Signal::Unavailable,
            };
            crate::launch::publish(&runtime, decide(signal, pending.marker_configured, false));
        }
    }
);

pub(crate) fn publish_launch(
    runtime: &Arc<Runtime>,
    backend: &Backend,
    config: &AutostartConfig,
    marker_found: bool,
) {
    if marker_found {
        crate::launch::publish(runtime, LaunchReason::Login(Detection::Marker));
        return;
    }
    let launch_agent = !matches!(backend, Backend::AppService);
    let marker_configured = config.marker().is_some();
    // Set before observing, so a notification on another thread finds it.
    *istmo_core::sync::lock(&PENDING) = Some(Pending {
        runtime: Arc::downgrade(runtime),
        marker_configured,
        launch_agent,
    });
    if !observe_did_finish_launching() {
        // No AppKit event loop to come, or it already finished launching:
        // the Apple Event is gone.
        istmo_core::sync::lock(&PENDING).take();
        crate::launch::publish(
            runtime,
            decide(Signal::Unavailable, marker_configured, false),
        );
    }
}

/// Register for `NSApplicationDidFinishLaunchingNotification`. `false`
/// when `AppKit` is not loaded or the app already finished launching.
fn observe_did_finish_launching() -> bool {
    let Some(running) = AnyClass::get(c"NSRunningApplication") else {
        return false;
    };
    // SAFETY: `+currentApplication` returns the shared instance (or nil);
    // `-isFinishedLaunching` returns a `BOOL`.
    let finished = unsafe {
        let current: Option<Retained<AnyObject>> = msg_send![running, currentApplication];
        current.is_none_or(|app| {
            let finished: bool = msg_send![&*app, isFinishedLaunching];
            finished
        })
    };
    if finished {
        return false;
    }
    let (Some(center_class), Some(string_class)) = (
        AnyClass::get(c"NSNotificationCenter"),
        AnyClass::get(c"NSString"),
    ) else {
        return false;
    };
    // SAFETY: `+defaultCenter` returns the shared center;
    // `+stringWithUTF8String:` copies the NUL-terminated literal; `+new`
    // on an `NSObject` subclass returns a retained instance.
    let (center, name, observer): (
        Option<Retained<AnyObject>>,
        Option<Retained<AnyObject>>,
        Retained<Observer>,
    ) = unsafe {
        (
            msg_send![center_class, defaultCenter],
            msg_send![
                string_class,
                stringWithUTF8String: c"NSApplicationDidFinishLaunchingNotification".as_ptr()
            ],
            msg_send![Observer::class(), new],
        )
    };
    let (Some(center), Some(name)) = (center, name) else {
        return false;
    };
    let observer_ref: &AnyObject = &observer;
    // SAFETY: `observer` implements `istmoDidFinishLaunching:` taking the
    // notification; a null object observes every sender.
    unsafe {
        let () = msg_send![
            &*center,
            addObserver: observer_ref,
            selector: sel!(istmoDidFinishLaunching:),
            name: &*name,
            object: std::ptr::null_mut::<AnyObject>()
        ];
    }
    // The notification center does not retain observers; this one lives
    // for the process (one notification, a few bytes).
    std::mem::forget(observer);
    true
}

/// Whether the Apple Event being handled is the `oapp` of a login item.
fn launched_as_login_item() -> bool {
    let Some(manager_class) = AnyClass::get(c"NSAppleEventManager") else {
        return false;
    };
    // SAFETY: `+sharedAppleEventManager` returns the shared instance;
    // `-currentAppleEvent` returns the event being handled or nil; the
    // descriptor getters take `FourCharCode`s and return plain integers
    // or autoreleased descriptors (nil when absent).
    unsafe {
        let manager: Option<Retained<AnyObject>> =
            msg_send![manager_class, sharedAppleEventManager];
        let Some(manager) = manager else {
            return false;
        };
        let event: Option<Retained<AnyObject>> = msg_send![&*manager, currentAppleEvent];
        let Some(event) = event else {
            return false;
        };
        let id: u32 = msg_send![&*event, eventID];
        if id != OPEN_APPLICATION {
            return false;
        }
        let prop: Option<Retained<AnyObject>> =
            msg_send![&*event, paramDescriptorForKeyword: PROP_DATA];
        prop.is_some_and(|prop| {
            let code: u32 = msg_send![&*prop, enumCodeValue];
            code == LAUNCHED_AS_LOGIN_ITEM
        })
    }
}
