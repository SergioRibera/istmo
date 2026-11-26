//! Windows: Jump List tasks, taskbar progress, window flashing and the
//! shell's recent documents.
//!
//! COM calls run on one thread of the plugin's own (`istmo-launcher-com`,
//! a single-threaded apartment started on first use). It creates the
//! `ITaskbarList3` and `ICustomDestinationList` once and keeps them, so a
//! progress update is one call on a live object instead of a
//! `CoCreateInstance` per update.
//!
//! A Jump List task is a shell link to the running executable with the
//! action's URL as its only argument; `istmo_plugins::DocumentsPublisher`
//! turns that argument into a deep link (forwarded to the running
//! instance when single instance is on).

use std::num::NonZeroIsize;
use std::path::PathBuf;

use istmo_window::WindowRegistry;
use windows::Win32::Foundation::HWND;
use windows::Win32::Storage::EnhancedStorage::PKEY_Title;
use windows::Win32::System::Com::StructuredStorage::{
    PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0, PropVariantClear,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoCreateInstance,
    CoInitializeEx,
};
use windows::Win32::System::Variant::VT_LPWSTR;
use windows::Win32::UI::Shell::Common::{IObjectArray, IObjectCollection};
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
use windows::Win32::UI::Shell::{
    ApplicationDestinations, DestinationList, EnumerableObjectCollection, IApplicationDestinations,
    ICustomDestinationList, IShellLinkW, ITaskbarList3, KDC_RECENT, SHARD_PATHW, SHAddToRecentDocs,
    SHStrDupW, ShellLink, TBPF_ERROR, TBPF_INDETERMINATE, TBPF_NOPROGRESS, TBPF_NORMAL,
    TBPF_PAUSED, TBPFLAG, TaskbarList,
};
use windows::Win32::UI::WindowsAndMessaging::{
    FLASHW_STOP, FLASHW_TIMERNOFG, FLASHW_TRAY, FLASHWINFO, FlashWindowEx,
};
use windows::core::{HSTRING, Interface, PCWSTR};

use super::{Config, ico};
use crate::{
    ActionIcon, Attention, Badge, BadgeSupport, Capabilities, LauncherAction, LauncherError,
    Progress, WINDOWS_MAX_ACTIONS,
};

/// Taskbar progress is reported in these units.
const PROGRESS_TOTAL: u64 = 10_000;
const PROGRESS_TOTAL_F64: f64 = 10_000.0;

/// Work for the COM thread.
enum Job {
    Tasks(Vec<LauncherAction>),
    Progress(Progress, Vec<NonZeroIsize>),
    AddRecent(String),
    ClearRecent,
}

type Reply = oneshot::Sender<Result<(), LauncherError>>;

/// Windows side of [`super::DesktopLauncherActions`].
pub(crate) struct Platform {
    jobs: flume::Sender<(Job, Reply)>,
}

impl std::fmt::Debug for Platform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Platform").finish_non_exhaustive()
    }
}

impl Platform {
    pub(crate) fn new(_config: &Config) -> Self {
        let (jobs, inbox) = flume::unbounded::<(Job, Reply)>();
        let started = std::thread::Builder::new()
            .name("istmo-launcher-com".into())
            .spawn(move || ComThread::default().run(&inbox));
        if let Err(err) = started {
            // The receiver is gone with the closure: every job then fails
            // with "COM thread is gone" instead of hanging.
            tracing::warn!(target: "istmo.launcher_actions", %err, "COM thread not started");
        }
        Self { jobs }
    }

    pub(crate) const fn capabilities() -> Capabilities {
        Capabilities {
            dynamic_actions: true,
            max_actions: WINDOWS_MAX_ACTIONS,
            pin: false,
            // The overlay icon is an image the plugin would have to draw.
            badge: BadgeSupport::Unsupported,
            progress: true,
            attention: true,
            recent_documents: true,
        }
    }

    async fn run(&self, job: Job) -> Result<(), LauncherError> {
        let (reply, answer) = oneshot::channel();
        let gone = || LauncherError::Backend("COM thread is gone".into());
        self.jobs.send((job, reply)).map_err(|_| gone())?;
        answer.await.unwrap_or_else(|_| Err(gone()))
    }

    pub(crate) async fn set_actions(
        &self,
        actions: Vec<LauncherAction>,
    ) -> Result<(), LauncherError> {
        self.run(Job::Tasks(actions)).await
    }

    #[expect(clippy::unused_async, reason = "same shape as the other platforms")]
    pub(crate) async fn set_badge(&self, _: Badge) -> Result<(), LauncherError> {
        Err(LauncherError::Unsupported)
    }

    pub(crate) async fn set_progress(&self, progress: Progress) -> Result<(), LauncherError> {
        let windows = windows()?;
        self.run(Job::Progress(progress, windows)).await
    }

    #[expect(clippy::unused_async, reason = "same shape as the other platforms")]
    pub(crate) async fn request_attention(
        &self,
        attention: Attention,
    ) -> Result<(), LauncherError> {
        let (flags, count) = match attention {
            Attention::Informational => (FLASHW_TRAY, 3),
            Attention::Critical => (FLASHW_TRAY | FLASHW_TIMERNOFG, 0),
            Attention::Cancel => (FLASHW_STOP, 0),
        };
        for handle in windows()? {
            let info = FLASHWINFO {
                cbSize: u32::try_from(std::mem::size_of::<FLASHWINFO>()).unwrap_or(u32::MAX),
                hwnd: hwnd(handle),
                dwFlags: flags,
                uCount: count,
                dwTimeout: 0,
            };
            // SAFETY: `info` is a valid FLASHWINFO for the call; the
            // return value is the previous state, not an error.
            let _ = unsafe { FlashWindowEx(&raw const info) };
        }
        Ok(())
    }

    pub(crate) async fn add_recent_document(&self, path: String) -> Result<(), LauncherError> {
        self.run(Job::AddRecent(path)).await
    }

    pub(crate) async fn clear_recent_documents(&self) -> Result<(), LauncherError> {
        self.run(Job::ClearRecent).await
    }
}

/// Every window registered with `istmo-window`.
fn windows() -> Result<Vec<NonZeroIsize>, LauncherError> {
    let registry = WindowRegistry::global();
    let windows: Vec<NonZeroIsize> = registry
        .ids()
        .into_iter()
        .filter_map(|id| registry.get(id)?.hwnd())
        .collect();
    if windows.is_empty() {
        Err(LauncherError::NoWindow)
    } else {
        Ok(windows)
    }
}

const fn hwnd(handle: NonZeroIsize) -> HWND {
    HWND(handle.get() as *mut _)
}

fn backend(context: &str, err: &windows::core::Error) -> LauncherError {
    LauncherError::Backend(format!("{context}: {err}"))
}

/// State of the COM thread: objects created once and kept.
#[derive(Default)]
struct ComThread {
    taskbar: Option<ITaskbarList3>,
    destinations: Option<ICustomDestinationList>,
}

impl ComThread {
    fn run(mut self, inbox: &flume::Receiver<(Job, Reply)>) {
        // SAFETY: first COM call of this thread, which never leaves the
        // apartment it enters here.
        let init =
            unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
        if let Err(err) = init.ok() {
            for (_, reply) in inbox {
                let _ = reply.send(Err(backend("CoInitializeEx", &err)));
            }
            return;
        }
        for (job, reply) in inbox {
            let result = match job {
                Job::Tasks(actions) => self.tasks(&actions),
                Job::Progress(progress, windows) => self.progress(progress, &windows),
                Job::AddRecent(path) => {
                    Self::add_recent(&path);
                    Ok(())
                }
                Job::ClearRecent => Self::clear_recent(),
            };
            // The caller may have dropped its future; nothing to report then.
            let _ = reply.send(result);
        }
        // COM stays initialised until the thread exits with the process's
        // last `Platform`; the kept objects are released first by drop.
    }

    fn taskbar(&mut self) -> Result<&ITaskbarList3, LauncherError> {
        if self.taskbar.is_none() {
            // SAFETY: COM is initialised on this thread.
            let taskbar: ITaskbarList3 =
                unsafe { CoCreateInstance(&TaskbarList, None, CLSCTX_INPROC_SERVER) }
                    .map_err(|e| backend("TaskbarList", &e))?;
            // SAFETY: a fresh taskbar list, initialised once before use.
            unsafe { taskbar.HrInit() }.map_err(|e| backend("ITaskbarList3::HrInit", &e))?;
            self.taskbar = Some(taskbar);
        }
        self.taskbar
            .as_ref()
            .ok_or_else(|| LauncherError::Backend("taskbar list missing".into()))
    }

    fn destinations(&mut self) -> Result<&ICustomDestinationList, LauncherError> {
        if self.destinations.is_none() {
            // SAFETY: COM is initialised on this thread.
            let list: ICustomDestinationList =
                unsafe { CoCreateInstance(&DestinationList, None, CLSCTX_INPROC_SERVER) }
                    .map_err(|e| backend("DestinationList", &e))?;
            self.destinations = Some(list);
        }
        self.destinations
            .as_ref()
            .ok_or_else(|| LauncherError::Backend("destination list missing".into()))
    }

    fn progress(
        &mut self,
        progress: Progress,
        windows: &[NonZeroIsize],
    ) -> Result<(), LauncherError> {
        let state: TBPFLAG = match progress {
            Progress::Clear => TBPF_NOPROGRESS,
            Progress::Indeterminate => TBPF_INDETERMINATE,
            Progress::Normal(_) => TBPF_NORMAL,
            Progress::Paused(_) => TBPF_PAUSED,
            Progress::Error(_) => TBPF_ERROR,
        };
        let completed = progress.fraction().map(|f| {
            // A clamped fraction times 10 000 always fits.
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "0..=10000"
            )]
            let units = (f64::from(f) * PROGRESS_TOTAL_F64).round() as u64;
            units
        });
        let taskbar = self.taskbar()?;
        for &handle in windows {
            let hwnd = hwnd(handle);
            // SAFETY: a registered window; the taskbar ignores a handle
            // whose button it does not know.
            unsafe {
                taskbar
                    .SetProgressState(hwnd, state)
                    .map_err(|e| backend("SetProgressState", &e))?;
                if let Some(units) = completed {
                    taskbar
                        .SetProgressValue(hwnd, units, PROGRESS_TOTAL)
                        .map_err(|e| backend("SetProgressValue", &e))?;
                }
            }
        }
        Ok(())
    }

    fn tasks(&mut self, actions: &[LauncherAction]) -> Result<(), LauncherError> {
        let list = self.destinations()?.clone();
        if actions.is_empty() {
            // SAFETY: a live destination list; null = this process's AppID.
            return unsafe { list.DeleteList(PCWSTR::null()) }
                .map_err(|e| backend("ICustomDestinationList::DeleteList", &e));
        }
        let exe = std::env::current_exe()
            .map_err(|e| LauncherError::Backend(format!("current executable: {e}")))?;
        let exe = HSTRING::from(exe.as_os_str());
        let icon_dir = icon_dir();
        // SAFETY: COM is initialised on this thread.
        let tasks: IObjectCollection =
            unsafe { CoCreateInstance(&EnumerableObjectCollection, None, CLSCTX_INPROC_SERVER) }
                .map_err(|e| backend("EnumerableObjectCollection", &e))?;
        for action in actions {
            let link = shell_link(action, &exe, &icon_dir)?;
            // SAFETY: both objects are live; the collection takes a reference.
            unsafe { tasks.AddObject(&link) }.map_err(|e| backend("AddObject", &e))?;
        }
        let tasks: IObjectArray = tasks.cast().map_err(|e| backend("IObjectArray", &e))?;
        let mut slots = 0u32;
        // SAFETY: a live list; one Begin / Commit transaction on this thread.
        unsafe {
            let _removed: IObjectArray = list
                .BeginList(&raw mut slots)
                .map_err(|e| backend("BeginList", &e))?;
            // Keep the shell's own Recent list next to the tasks: a custom
            // list replaces the default one entirely. Apps without a
            // registered file type have no Recent category; that is fine.
            let _ = list.AppendKnownCategory(KDC_RECENT);
            if let Err(err) = list.AddUserTasks(&tasks) {
                let _ = list.AbortList();
                return Err(backend("AddUserTasks", &err));
            }
            list.CommitList().map_err(|e| backend("CommitList", &e))
        }
    }

    fn add_recent(path: &str) {
        let wide = HSTRING::from(path);
        // SAFETY: a null-terminated wide path, read during the call.
        unsafe { SHAddToRecentDocs(SHARD_PATHW.0 as u32, Some(wide.as_ptr().cast())) };
    }

    fn clear_recent() -> Result<(), LauncherError> {
        // `SHAddToRecentDocs(…, NULL)` would empty the user's recent
        // files of every app; this clears only this app's lists.
        // SAFETY: COM is initialised on this thread.
        let destinations: IApplicationDestinations =
            unsafe { CoCreateInstance(&ApplicationDestinations, None, CLSCTX_INPROC_SERVER) }
                .map_err(|e| backend("ApplicationDestinations", &e))?;
        // SAFETY: a live object.
        unsafe { destinations.RemoveAllDestinations() }
            .map_err(|e| backend("RemoveAllDestinations", &e))
    }
}

/// Where task icons are written: `%LOCALAPPDATA%\istmo\launcher-actions`.
fn icon_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map_or_else(std::env::temp_dir, PathBuf::from)
        .join("istmo")
        .join("launcher-actions")
}

fn shell_link(
    action: &LauncherAction,
    exe: &HSTRING,
    icon_dir: &std::path::Path,
) -> Result<IShellLinkW, LauncherError> {
    // SAFETY: COM is initialised on this thread (the only caller).
    let link: IShellLinkW = unsafe { CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER) }
        .map_err(|e| backend("ShellLink", &e))?;
    // The URL holds no whitespace or `"` (validated), so it stays one
    // argv entry without quoting.
    let arguments = HSTRING::from(action.url.as_str());
    let tooltip = HSTRING::from(action.subtitle.as_deref().unwrap_or(&action.title));
    let icon = match &action.icon {
        Some(ActionIcon::Png(png)) => match ico::materialise(icon_dir, png) {
            Ok(path) => HSTRING::from(path.as_os_str()),
            Err(err) => {
                tracing::warn!(target: "istmo.launcher_actions", %err, "task icon not written");
                exe.clone()
            }
        },
        Some(ActionIcon::Symbol(_)) | None => exe.clone(),
    };
    // SAFETY: a live link; every string outlives its call.
    unsafe {
        link.SetPath(exe).map_err(|e| backend("SetPath", &e))?;
        link.SetArguments(&arguments)
            .map_err(|e| backend("SetArguments", &e))?;
        link.SetDescription(&tooltip)
            .map_err(|e| backend("SetDescription", &e))?;
        link.SetIconLocation(&icon, 0)
            .map_err(|e| backend("SetIconLocation", &e))?;
    }
    let store: IPropertyStore = link.cast().map_err(|e| backend("IPropertyStore", &e))?;
    let mut title = string_variant(&action.title)?;
    // SAFETY: a live store and a VT_LPWSTR variant owned here, cleared
    // right after (the store copies it).
    let stored = unsafe {
        let set = store.SetValue(&PKEY_Title, &raw const title);
        let _ = PropVariantClear(&raw mut title);
        set.and_then(|()| store.Commit())
    };
    stored.map_err(|e| backend("PKEY_Title", &e))?;
    Ok(link)
}

/// A `VT_LPWSTR` variant holding a `CoTaskMemAlloc` copy of `text`.
fn string_variant(text: &str) -> Result<PROPVARIANT, LauncherError> {
    let wide = HSTRING::from(text);
    // SAFETY: a null-terminated wide string, copied by the call.
    let copy = unsafe { SHStrDupW(&wide) }.map_err(|e| backend("SHStrDupW", &e))?;
    Ok(PROPVARIANT {
        Anonymous: PROPVARIANT_0 {
            Anonymous: std::mem::ManuallyDrop::new(PROPVARIANT_0_0 {
                vt: VT_LPWSTR,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: PROPVARIANT_0_0_0 { pwszVal: copy },
            }),
        },
    })
}
