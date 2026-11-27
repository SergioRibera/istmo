//! Registration: what makes toasts work for this app and reach it when
//! closed.
//!
//! Unpackaged apps get registry-only registration (no Start-menu
//! shortcut, the route the Windows App SDK takes), refreshed on every
//! start:
//!
//! * `HKCU\Software\Classes\AppUserModelId\<AUMID>`: `DisplayName`,
//!   `IconUri`, `IconBackgroundColor`, `CustomActivator = {CLSID}`;
//! * `HKCU\Software\Classes\CLSID\{CLSID}\LocalServer32` =
//!   `"<exe>" -istmo-notification` (COM appends `-Embedding`);
//! * the process AUMID (`SetCurrentProcessExplicitAppUserModelID`).
//!
//! Packaged (MSIX) apps carry activation in their manifest
//! (`com:ExeServer` with the same CLSID and `-istmo-notification` as
//! its arguments, plus `desktop:ToastNotificationActivation`); nothing
//! is written. Both register the COM class object.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::OnceLock;

use windows::ApplicationModel::{AppInfo, Package};
use windows::UI::Notifications::ToastNotificationManager;
use windows_core::{GUID, HSTRING};
use windows_registry::{CURRENT_USER, LOCAL_MACHINE};

use super::{GROUP, MIN_BUILD, activator, ident, sys};
use crate::{DesktopNotifications, Feature, NotificationError, PlatformError};

/// What [`DesktopNotifications::install`] registers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowsApp {
    aumid: String,
    display_name: String,
    icon: Option<Icon>,
    icon_background: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Icon {
    File(PathBuf),
    Png(Vec<u8>),
}

impl WindowsApp {
    /// An app with this `AppUserModelID` (`[app] id`, at most 128
    /// characters, no spaces) and display name (`[app] name`).
    #[must_use]
    pub fn new(aumid: impl Into<String>, display_name: impl Into<String>) -> Self {
        Self {
            aumid: aumid.into(),
            display_name: display_name.into(),
            icon: None,
            icon_background: None,
        }
    }

    /// The icon of the app in toasts and settings: an absolute path to a
    /// `.png` or `.ico` that stays where it is.
    #[must_use]
    pub fn with_icon_file(mut self, path: impl Into<PathBuf>) -> Self {
        self.icon = Some(Icon::File(path.into()));
        self
    }

    /// The icon as PNG bytes (`istmo::app_icon!()`): written once to the
    /// plugin's data directory, named by content hash.
    #[must_use]
    pub fn with_icon_png(mut self, png: impl Into<Vec<u8>>) -> Self {
        self.icon = Some(Icon::Png(png.into()));
        self
    }

    /// Background behind the icon, `0xAARRGGBB`.
    #[must_use]
    pub const fn with_icon_background(mut self, argb: u32) -> Self {
        self.icon_background = Some(argb);
        self
    }

    /// The app as the build describes it: `[app] id` (else the crate
    /// name), `[app] name` (else the id), `[app.notifications] accent`
    /// as the icon background. Use [`windows_app!`](crate::windows_app),
    /// which reads these at the app crate.
    #[doc(hidden)]
    #[must_use]
    pub fn from_build(
        bundle_id: Option<&str>,
        crate_name: &str,
        name: Option<&str>,
        accent: Option<&str>,
    ) -> Self {
        let aumid = bundle_id.unwrap_or(crate_name);
        let mut app = Self::new(aumid, name.unwrap_or(aumid));
        if let Some(argb) = accent
            .map(|hex| hex.trim_start_matches('#'))
            .and_then(|hex| u32::from_str_radix(hex, 16).ok())
        {
            app.icon_background = Some(argb);
        }
        app
    }

    /// The `AppUserModelID`.
    #[must_use]
    pub fn aumid(&self) -> &str {
        &self.aumid
    }

    /// The activator CLSID (`{...}`): UUID v5 of the AUMID. A packaged
    /// app's manifest declares this one.
    #[must_use]
    pub fn activator_clsid(&self) -> String {
        ident::guid_text(ident::activator_clsid(&self.aumid))
    }
}

/// The outcome of [`DesktopNotifications::install`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Installation {
    /// The AUMID toasts are shown under (the package's for MSIX).
    pub aumid: String,
    /// The activator CLSID.
    pub clsid: String,
    /// Packaged (MSIX): nothing was written to the registry.
    pub packaged: bool,
    /// COM started this process for a toast activation.
    pub launched_by_notification: bool,
    /// The Windows build.
    pub build: u32,
}

/// The registration of this process, set once by `install`.
#[derive(Debug)]
pub(super) struct Registered {
    pub(super) aumid: String,
    pub(super) build: u32,
}

static REGISTERED: OnceLock<Result<Installation, NotificationError>> = OnceLock::new();

/// The registration, when `install` succeeded.
pub(super) fn registered() -> Option<Registered> {
    match REGISTERED.get() {
        Some(Ok(installation)) => Some(Registered {
            aumid: installation.aumid.clone(),
            build: installation.build,
        }),
        _ => None,
    }
}

pub(super) fn platform(err: &windows_core::Error) -> NotificationError {
    NotificationError::Platform(PlatformError {
        code: i64::from(err.code().0),
        message: err.message(),
    })
}

fn registry(err: &windows_core::Error, what: &str) -> NotificationError {
    NotificationError::Platform(PlatformError {
        code: i64::from(err.code().0),
        message: format!("{what}: {}", err.message()),
    })
}

/// The build number (`CurrentBuildNumber`); 0 when unreadable.
pub(super) fn build_number() -> u32 {
    LOCAL_MACHINE
        .open(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion")
        .and_then(|key| key.get_string("CurrentBuildNumber"))
        .ok()
        .and_then(|build| build.trim().parse().ok())
        .unwrap_or(0)
}

/// COM started this process for a toast activation (`-istmo-notification`
/// on the command line).
#[must_use]
pub fn launched_by_notification() -> bool {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    ident::is_activation_launch(&args)
}

/// The command line without the program name and the activation flags
/// COM adds (`-istmo-notification`, `-Embedding`).
#[must_use]
pub fn app_args() -> Vec<OsString> {
    ident::strip_activation_args(std::env::args_os().skip(1))
}

fn app_key(aumid: &str) -> String {
    format!(r"Software\Classes\AppUserModelId\{aumid}")
}

fn clsid_key(clsid: &str) -> String {
    format!(r"Software\Classes\CLSID\{clsid}")
}

/// The icon path written to `IconUri`.
fn icon_path(icon: &Icon) -> Result<PathBuf, NotificationError> {
    match icon {
        Icon::File(path) => Ok(path.clone()),
        Icon::Png(bytes) => {
            let dir = istmo_core::path::data_dir().join("istmo-notifications");
            let path = dir.join(format!("app-icon-{:016x}.png", ident::fnv1a(bytes)));
            if !path.is_file() {
                let io = |err: std::io::Error| {
                    NotificationError::Platform(PlatformError {
                        code: 0,
                        message: format!("app icon not written: {err}"),
                    })
                };
                std::fs::create_dir_all(&dir).map_err(io)?;
                let tmp = path.with_extension("png.tmp");
                std::fs::write(&tmp, bytes).map_err(io)?;
                std::fs::rename(&tmp, &path).map_err(io)?;
            }
            Ok(path)
        }
    }
}

fn write_keys(app: &WindowsApp, clsid: &str) -> Result<(), NotificationError> {
    let exe = std::env::current_exe().map_err(|err| {
        NotificationError::Platform(PlatformError {
            code: 0,
            message: format!("executable path unknown: {err}"),
        })
    })?;
    let exe = exe.to_string_lossy();
    let exe = exe.strip_prefix(r"\\?\").unwrap_or(&exe);
    let key = CURRENT_USER
        .create(app_key(&app.aumid))
        .map_err(|err| registry(&err, "AppUserModelId key"))?;
    key.set_string("DisplayName", &app.display_name)
        .map_err(|err| registry(&err, "DisplayName"))?;
    match &app.icon {
        Some(icon) => {
            let path = icon_path(icon)?;
            key.set_string("IconUri", path.to_string_lossy())
                .map_err(|err| registry(&err, "IconUri"))?;
        }
        None => {
            let _absent = key.remove_value("IconUri");
        }
    }
    match app.icon_background {
        Some(argb) => key
            .set_string("IconBackgroundColor", format!("{argb:08X}"))
            .map_err(|err| registry(&err, "IconBackgroundColor"))?,
        None => {
            let _absent = key.remove_value("IconBackgroundColor");
        }
    }
    key.set_string("CustomActivator", clsid)
        .map_err(|err| registry(&err, "CustomActivator"))?;
    let server = CURRENT_USER
        .create(format!(r"{}\LocalServer32", clsid_key(clsid)))
        .map_err(|err| registry(&err, "LocalServer32 key"))?;
    server
        .set_string("", ident::server_command(exe))
        .map_err(|err| registry(&err, "LocalServer32"))?;
    Ok(())
}

fn install_once(app: &WindowsApp) -> Result<Installation, NotificationError> {
    ident::check_aumid(&app.aumid)?;
    if sys::is_elevated() {
        return Err(NotificationError::Elevated);
    }
    let build = build_number();
    if build < MIN_BUILD {
        return Err(NotificationError::Unsupported(Feature::Show));
    }
    let launched = launched_by_notification();
    if launched {
        activator::mark_launched();
        // A console-subsystem exe started by COM gets a console window
        // of its own; nothing reads it.
        sys::free_console();
    }
    let clsid_value = ident::activator_clsid(&app.aumid);
    let clsid = ident::guid_text(clsid_value);
    let packaged = Package::Current().is_ok();
    let aumid = if packaged {
        AppInfo::Current()
            .and_then(|info| info.AppUserModelId())
            .map_or_else(|_| app.aumid.clone(), |id| id.to_string())
    } else {
        sys::set_process_aumid(&app.aumid).map_err(|err| platform(&err))?;
        write_keys(app, &clsid)?;
        app.aumid.clone()
    };
    activator::serve(GUID::from_u128(clsid_value)).map_err(|err| platform(&err))?;
    Ok(Installation {
        aumid,
        clsid,
        packaged,
        launched_by_notification: launched,
        build,
    })
}

impl DesktopNotifications {
    /// Windows: registers the app for toasts and starts the activator.
    /// Call it first thing in `main`, before the runtime starts: when a
    /// toast started the process, COM waits at most 60 s for the
    /// activator. Runs once per process (later calls return the first
    /// outcome).
    ///
    /// Refuses elevated processes ([`NotificationError::Elevated`]) and
    /// builds older than 17763 ([`NotificationError::Unsupported`]).
    /// Without a successful install the host reports no backend.
    pub fn install(app: &WindowsApp) -> Result<Installation, NotificationError> {
        REGISTERED.get_or_init(|| install_once(app)).clone()
    }

    /// Windows: removes this app's toasts, scheduled toasts and registry
    /// keys (for uninstallers). Packaged apps only lose their toasts.
    pub fn unregister(app: &WindowsApp) -> Result<(), NotificationError> {
        ident::check_aumid(&app.aumid)?;
        let aumid = HSTRING::from(app.aumid.as_str());
        if let Ok(history) = ToastNotificationManager::History() {
            let _cleared = history.ClearWithId(&aumid);
        }
        if let Ok(notifier) = ToastNotificationManager::CreateToastNotifierWithId(&aumid)
            && let Ok(scheduled) = notifier.GetScheduledToastNotifications()
        {
            for toast in &scheduled {
                if toast.Group().is_ok_and(|group| group == GROUP) {
                    let _removed = notifier.RemoveFromSchedule(&toast);
                }
            }
        }
        if Package::Current().is_ok() {
            return Ok(());
        }
        let clsid = app.activator_clsid();
        for path in [app_key(&app.aumid), clsid_key(&clsid)] {
            match CURRENT_USER.remove_tree(&path) {
                Ok(()) => {}
                // Already gone.
                Err(err)
                    if err.code().0
                        == windows::Win32::Foundation::ERROR_FILE_NOT_FOUND
                            .to_hresult()
                            .0 => {}
                Err(err) => return Err(registry(&err, &path)),
            }
        }
        Ok(())
    }
}

/// The [`WindowsApp`] of the app crate this expands in.
///
/// It reads what `istmo-build` bakes into that crate: `ISTMO_APP_BUNDLE_ID` (`[app] id`, else
/// `CARGO_PKG_NAME`) as the AUMID, `ISTMO_APP_NAME` as the display name,
/// `ISTMO_NOTIFICATION_ACCENT` (`AARRGGBB`) as the icon background.
/// Add the icon with [`WindowsApp::with_icon_png`]
/// (`istmo::app_icon!()`).
#[macro_export]
macro_rules! windows_app {
    () => {
        $crate::desktop::windows::WindowsApp::from_build(
            ::core::option_env!("ISTMO_APP_BUNDLE_ID"),
            ::core::env!("CARGO_PKG_NAME"),
            ::core::option_env!("ISTMO_APP_NAME"),
            ::core::option_env!("ISTMO_NOTIFICATION_ACCENT"),
        )
    };
}
