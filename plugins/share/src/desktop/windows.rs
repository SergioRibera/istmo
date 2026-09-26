//! Windows: `DataTransferManager` through `IDataTransferManagerInterop`.
//!
//! The share UI is bound to a window and must be driven from the thread
//! that owns it. When a window is registered in the
//! [`WindowRegistry`] (from its UI thread), the backend subclasses it;
//! [`Backend::present`] then validates the request on the calling
//! thread and posts the job to the window, whose subclass procedure
//! builds the `DataPackage` and shows the UI.
//!
//! Outcome mapping:
//! * `DataPackage.ShareCompleted` → [`ShareOutcome::Shared`] with the
//!   target's app user model id;
//! * `DataPackage.ShareCanceled` (Windows 11) → [`ShareOutcome::Dismissed`];
//! * builds without `ShareCanceled` cannot report a dismissal, so the
//!   share resolves as [`ShareOutcome::Unknown`] as soon as the UI is
//!   shown.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use flume::Sender;
use istmo_window::{NativeWindow, WindowId, WindowListener, WindowRegistry};
use windows::ApplicationModel::Activation::{
    ActivationKind, IActivatedEventArgs, ShareTargetActivatedEventArgs,
};
use windows::ApplicationModel::AppInstance;
use windows::ApplicationModel::DataTransfer::{
    DataPackage, DataPackageView, DataRequestedEventArgs, DataTransferManager,
    ShareCompletedEventArgs, StandardDataFormats,
};
use windows::Foundation::{TypedEventHandler, Uri};
use windows::Storage::Streams::RandomAccessStreamReference;
use windows::Storage::{IStorageItem, NameCollisionOption, StorageFile, StorageFolder};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize};
use windows::Win32::UI::Shell::{
    DefSubclassProc, IDataTransferManagerInterop, RemoveWindowSubclass, SetWindowSubclass,
};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP, WM_NCDESTROY};
use windows::core::{HSTRING, IInspectable, Interface, factory};

use istmo_core::CancelToken;
use windows::Foundation::Metadata::ApiInformation;
use windows::Win32::Foundation::APPMODEL_ERROR_NO_PACKAGE;
use windows::Win32::Storage::Packaging::Appx::GetCurrentPackageFullName;

use super::{ShareJob, until_cancelled};
use crate::{IncomingFile, IncomingShare, ShareCapabilities, ShareError, ShareInbox, ShareOutcome};

pub(super) fn capabilities() -> ShareCapabilities {
    ShareCapabilities {
        send: true,
        files: true,
        mixed_content: true,
        rich_preview: true,
        reports_completion: true,
        reports_target: true,
        receive: can_receive(),
        direct_share: false,
        dismiss_on_cancel: false,
    }
}

/// Share-target activation for desktop apps needs Windows 10 2004
/// (build 19041, `UniversalApiContract` v10) and package identity (MSIX
/// or a sparse package).
pub(super) fn can_receive() -> bool {
    has_package_identity() && share_target_supported()
}

fn share_target_supported() -> bool {
    ensure_winrt();
    ApiInformation::IsApiContractPresentByMajor(
        &HSTRING::from("Windows.Foundation.UniversalApiContract"),
        10,
    )
    .unwrap_or(false)
}

fn has_package_identity() -> bool {
    let mut len = 0_u32;
    // SAFETY: querying the required length only; no buffer is passed.
    let status = unsafe { GetCurrentPackageFullName(&raw mut len, None) };
    status != APPMODEL_ERROR_NO_PACKAGE
}

pub(super) const UNSUPPORTED_REASON: &str = "";

const SUBCLASS_ID: usize = 0x1570_5A4E;
const WM_ISTMO_SHARE: u32 = WM_APP + 0x15;

type Reply = Sender<Result<ShareOutcome, ShareError>>;

/// A job validated off the UI thread, waiting for the window to pick it
/// up. `StorageFile` is not `Send` in windows-rs, so files travel as
/// paths and are opened inside the `DataRequested` handler.
struct PreparedJob {
    title: String,
    description: Option<String>,
    text: Option<String>,
    web_link: Option<Uri>,
    files: Vec<PathBuf>,
    thumbnail: Option<PathBuf>,
    reply: Reply,
}

/// Jobs keyed by the id posted in `WPARAM`.
fn jobs() -> &'static Mutex<HashMap<usize, PreparedJob>> {
    static JOBS: OnceLock<Mutex<HashMap<usize, PreparedJob>>> = OnceLock::new();
    JOBS.get_or_init(Mutex::default)
}

static NEXT_JOB: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Default)]
struct Hooks {
    /// `HWND`s (as integers) currently subclassed.
    hooked: Mutex<HashSet<isize>>,
}

impl WindowListener for Hooks {
    fn window_registered(&self, _id: WindowId, window: NativeWindow) -> Result<(), String> {
        let Some(hwnd) = window.hwnd() else {
            return Ok(());
        };
        let raw = hwnd.get();
        if self
            .hooked
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(&raw)
        {
            return Ok(());
        }
        // SAFETY: `raw` is a live HWND registered by the app; the
        // subclass procedure is a plain function with no reference data.
        // SetWindowSubclass fails (returns FALSE) when called from a
        // thread other than the window's, which we surface as an error.
        let ok = unsafe { SetWindowSubclass(to_hwnd(raw), Some(subclass_proc), SUBCLASS_ID, 0) };
        if !ok.as_bool() {
            return Err(
                "SetWindowSubclass failed — register the window from its UI thread".to_owned(),
            );
        }
        self.hooked
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(raw);
        Ok(())
    }

    fn window_unregistered(&self, _id: WindowId) {
        // The subclass removes itself on WM_NCDESTROY; a still-alive
        // window keeps its (inert) hook until then.
    }
}

#[derive(Debug)]
pub(super) struct Backend {
    hooks: Arc<Hooks>,
}

impl Backend {
    pub(super) fn attach(registry: &WindowRegistry) -> Result<Self, ShareError> {
        let hooks = Arc::new(Hooks::default());
        let listener: Arc<dyn WindowListener> = Arc::clone(&hooks) as Arc<dyn WindowListener>;
        registry
            .add_listener(&listener)
            .map_err(|err| ShareError::NoPresenter(err.to_string()))?;
        // The registry holds listeners weakly; keep the strong ref in a
        // process-wide list so the hook outlives this backend clone.
        leak_listener(listener);
        Ok(Self { hooks })
    }

    pub(super) async fn present(
        &self,
        job: ShareJob,
        cancel: &CancelToken,
    ) -> Result<ShareOutcome, ShareError> {
        let (_, window) = job.window;
        let hwnd = window
            .hwnd()
            .ok_or_else(|| ShareError::NoPresenter(format!("{window:?} is not a Win32 window")))?
            .get();
        if !self
            .hooks
            .hooked
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(&hwnd)
        {
            return Err(ShareError::NoPresenter(
                "window was not registered from its UI thread".to_owned(),
            ));
        }
        let (tx, rx) = flume::bounded(1);
        let prepared = prepare(&job, tx)?;
        let id = usize::try_from(NEXT_JOB.fetch_add(1, Ordering::Relaxed)).unwrap_or_default();
        jobs()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id, prepared);
        // SAFETY: plain message post to a live window; the payload is an
        // integer key, not a pointer.
        if let Err(err) =
            unsafe { PostMessageW(Some(to_hwnd(hwnd)), WM_ISTMO_SHARE, WPARAM(id), LPARAM(0)) }
        {
            jobs()
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&id);
            return Err(ShareError::Backend(format!("PostMessageW: {err}")));
        }
        let Some(reply) = until_cancelled(cancel, rx.recv_async()).await else {
            // Windows offers no way to close the share UI. Drop the job
            // if the window has not picked it up yet; otherwise the UI
            // stays until the user dismisses it and its events go
            // nowhere.
            jobs()
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&id);
            return Ok(ShareOutcome::Dismissed);
        };
        reply.map_err(|_| ShareError::Backend("share UI closed without reporting".to_owned()))?
    }
}

fn leak_listener(listener: Arc<dyn WindowListener>) {
    static KEEP: OnceLock<Mutex<Vec<Arc<dyn WindowListener>>>> = OnceLock::new();
    KEEP.get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(listener);
}

/// Validate the request on the calling thread so failures surface
/// before anything is shown.
fn prepare(job: &ShareJob, reply: Reply) -> Result<PreparedJob, ShareError> {
    ensure_winrt();
    let request = &job.request;
    let files: Vec<PathBuf> = job.files.iter().map(|f| f.path.clone()).collect();
    let thumbnail = job.thumbnail.as_ref().map(|f| f.path.clone());
    let web_link = match &request.url {
        Some(url) => Some(
            Uri::CreateUri(&HSTRING::from(url.as_str()))
                .map_err(|_| ShareError::InvalidRequest(format!("not a valid URL: {url}")))?,
        ),
        None => None,
    };
    let preview_title = request.preview.as_ref().and_then(|p| p.title.clone());
    // The share UI refuses packages without a title.
    let title = preview_title
        .or_else(|| request.subject.clone())
        .or_else(|| request.text.clone())
        .or_else(|| request.url.clone())
        .or_else(|| job.files.first().map(|f| file_name(&f.path)))
        .unwrap_or_else(|| "Share".to_owned());
    Ok(PreparedJob {
        title,
        description: request.subject.clone(),
        // `SetWebLink` carries the URL; keep the text as written.
        text: request.text.clone(),
        web_link,
        files,
        thumbnail,
        reply,
    })
}

fn storage_file(path: &Path) -> windows::core::Result<StorageFile> {
    StorageFile::GetFileFromPathAsync(&HSTRING::from(path.as_os_str())).and_then(|op| op.join())
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn ensure_winrt() {
    // SAFETY: RoInitialize is idempotent per thread; RPC_E_CHANGED_MODE
    // (thread already initialised as STA) is fine — WinRT still works.
    drop(unsafe { RoInitialize(RO_INIT_MULTITHREADED) });
}

// By value so it plugs straight into `map_err`.
#[allow(clippy::needless_pass_by_value)]
fn backend(err: windows::core::Error) -> ShareError {
    ShareError::Backend(err.to_string())
}

const fn to_hwnd(raw: isize) -> HWND {
    HWND(raw as *mut core::ffi::c_void)
}

unsafe extern "system" fn subclass_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    if msg == WM_ISTMO_SHARE {
        let job = jobs()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&wparam.0);
        if let Some(job) = job {
            let reply = job.reply.clone();
            if let Err(err) = show(hwnd, job) {
                drop(reply.send(Err(err)));
            }
        }
        return LRESULT(0);
    }
    if msg == WM_NCDESTROY {
        // SAFETY: removing our own subclass from the window being
        // destroyed, on its thread.
        let _ = unsafe { RemoveWindowSubclass(hwnd, Some(subclass_proc), SUBCLASS_ID) };
    }
    // SAFETY: forwarding the untouched message down the subclass chain.
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}

thread_local! {
    /// `DataRequested` registration per window, so each share replaces
    /// the previous handler instead of stacking them. UI thread only.
    static REQUEST_TOKENS: std::cell::RefCell<HashMap<isize, i64>> =
        std::cell::RefCell::new(HashMap::new());
}

/// Runs on the window's UI thread.
fn show(hwnd: HWND, job: PreparedJob) -> Result<(), ShareError> {
    let interop: IDataTransferManagerInterop =
        factory::<DataTransferManager, IDataTransferManagerInterop>().map_err(backend)?;
    // SAFETY: `hwnd` is the live window this procedure is running for.
    let manager: DataTransferManager = unsafe { interop.GetForWindow(hwnd) }.map_err(backend)?;

    let key = hwnd.0 as isize;
    if let Some(previous) = REQUEST_TOKENS.with(|t| t.borrow_mut().remove(&key)) {
        drop(manager.RemoveDataRequested(previous));
    }

    let reply = job.reply.clone();
    let job = Mutex::new(Some(job));
    let cancel_supported = Arc::new(Mutex::new(true));
    let cancel_flag = Arc::clone(&cancel_supported);
    let handler =
        TypedEventHandler::<DataTransferManager, DataRequestedEventArgs>::new(move |_, args| {
            let Some(args) = args.as_ref() else {
                return Ok(());
            };
            let Some(job) = job.lock().unwrap_or_else(PoisonError::into_inner).take() else {
                return Ok(());
            };
            let package = args.Request()?.Data()?;
            if !fill(&package, &job)? {
                *cancel_flag.lock().unwrap_or_else(PoisonError::into_inner) = false;
            }
            Ok(())
        });
    let token = manager.DataRequested(&handler).map_err(backend)?;
    REQUEST_TOKENS.with(|t| t.borrow_mut().insert(key, token));

    // SAFETY: same live window as above.
    unsafe { interop.ShowShareUIForWindow(hwnd) }.map_err(backend)?;

    // `DataRequested` fires synchronously inside ShowShareUIForWindow.
    if !*cancel_supported
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
    {
        drop(reply.send(Ok(ShareOutcome::Unknown)));
    }
    Ok(())
}

/// Fill `package` from `job` and wire the completion events. Returns
/// whether dismissals can be reported (`ShareCanceled` available).
fn fill(package: &DataPackage, job: &PreparedJob) -> windows::core::Result<bool> {
    let props = package.Properties()?;
    props.SetTitle(&HSTRING::from(job.title.as_str()))?;
    if let Some(description) = &job.description {
        props.SetDescription(&HSTRING::from(description.as_str()))?;
    }
    if let Some(thumbnail) = &job.thumbnail {
        props.SetThumbnail(&RandomAccessStreamReference::CreateFromFile(
            &storage_file(thumbnail)?,
        )?)?;
    }
    if let Some(text) = &job.text {
        package.SetText(&HSTRING::from(text.as_str()))?;
    }
    if let Some(link) = &job.web_link {
        package.SetWebLink(link)?;
    }
    if !job.files.is_empty() {
        let items: Vec<Option<IStorageItem>> = job
            .files
            .iter()
            .map(|path| storage_file(path)?.cast::<IStorageItem>().map(Some))
            .collect::<windows::core::Result<_>>()?;
        let items: windows_collections::IIterable<IStorageItem> = items.into();
        package.SetStorageItemsReadOnly(&items)?;
    }

    let completed = job.reply.clone();
    package.ShareCompleted(
        &TypedEventHandler::<DataPackage, ShareCompletedEventArgs>::new(move |_, args| {
            let target = args
                .as_ref()
                .and_then(|a| a.ShareTarget().ok())
                .and_then(|t| t.AppUserModelId().ok())
                .map(|id| id.to_string_lossy());
            drop(completed.send(Ok(ShareOutcome::Shared(target))));
            Ok(())
        }),
    )?;
    let canceled = job.reply.clone();
    let cancel_supported = package
        .ShareCanceled(&TypedEventHandler::<DataPackage, IInspectable>::new(
            move |_, _| {
                drop(canceled.send(Ok(ShareOutcome::Dismissed)));
                Ok(())
            },
        ))
        .is_ok();
    Ok(cancel_supported)
}

/// For MSIX-packaged apps declared as a Share Target: when the process
/// was activated by a share, copy the shared content into app-owned
/// storage, publish it to `inbox` and report completion to Windows.
///
/// Call once at startup. Returns `Ok(false)` when the app was launched
/// normally (or is not packaged).
pub fn take_share_target_activation(inbox: &ShareInbox) -> Result<bool, ShareError> {
    if !can_receive() {
        return Ok(false);
    }
    let Ok(args) = AppInstance::GetActivatedEventArgs() else {
        return Ok(false);
    };
    let Ok(kind) = args.Kind() else {
        return Ok(false);
    };
    if kind != ActivationKind::ShareTarget {
        return Ok(false);
    }
    let share: ShareTargetActivatedEventArgs =
        Interface::cast::<ShareTargetActivatedEventArgs>(&args as &IActivatedEventArgs)
            .map_err(backend)?;
    let operation = share.ShareOperation().map_err(backend)?;
    let view = operation.Data().map_err(backend)?;
    let incoming = read_view(&view)?;
    operation.ReportCompleted().map_err(backend)?;
    inbox
        .publish(IncomingShare {
            source_app: operation
                .Data()
                .and_then(|d| d.Properties())
                .and_then(|p| p.PackageFamilyName())
                .ok()
                .map(|n| n.to_string_lossy())
                .filter(|n| !n.is_empty()),
            ..incoming
        })
        .map_err(|err| ShareError::Backend(err.to_string()))?;
    Ok(true)
}

fn read_view(view: &DataPackageView) -> Result<IncomingShare, ShareError> {
    let has = |format: windows::core::Result<HSTRING>| {
        format.and_then(|f| view.Contains(&f)).unwrap_or(false)
    };
    let text = if has(StandardDataFormats::Text()) {
        Some(
            view.GetTextAsync()
                .and_then(|op| op.join())
                .map_err(backend)?
                .to_string_lossy(),
        )
    } else {
        None
    };
    let url = if has(StandardDataFormats::WebLink()) {
        Some(
            view.GetWebLinkAsync()
                .and_then(|op| op.join())
                .and_then(|uri| uri.AbsoluteUri())
                .map_err(backend)?
                .to_string_lossy(),
        )
    } else {
        None
    };
    let subject = view
        .Properties()
        .and_then(|p| p.Title())
        .ok()
        .map(|t| t.to_string_lossy())
        .filter(|t| !t.is_empty());
    let files = if has(StandardDataFormats::StorageItems()) {
        copy_items(view)?
    } else {
        Vec::new()
    };
    Ok(IncomingShare {
        text,
        url,
        subject,
        files,
        source_app: None,
        received_at_ms: None,
        target_id: None,
    })
}

fn copy_items(view: &DataPackageView) -> Result<Vec<IncomingFile>, ShareError> {
    let incoming_root = std::env::temp_dir().join("istmo-share").join("incoming");
    super::prune_stale_entries(&incoming_root, crate::INCOMING_RETENTION);
    let dest_dir = incoming_root.join(format!(
        "{}-{}",
        std::process::id(),
        NEXT_JOB.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dest_dir)
        .map_err(|err| ShareError::Io(format!("{}: {err}", dest_dir.display())))?;
    let folder = StorageFolder::GetFolderFromPathAsync(&HSTRING::from(dest_dir.as_os_str()))
        .and_then(|op| op.join())
        .map_err(backend)?;
    let items = view
        .GetStorageItemsAsync()
        .and_then(|op| op.join())
        .map_err(backend)?;
    let mut files = Vec::new();
    for item in items {
        // Folders are skipped; only files are shared into the app.
        let Ok(file) = item.cast::<StorageFile>() else {
            continue;
        };
        let name = file.Name().map_err(backend)?;
        let copy = file
            .CopyOverload(&folder, &name, NameCollisionOption::GenerateUniqueName)
            .and_then(|op| op.join())
            .map_err(backend)?;
        let path = copy.Path().map_err(backend)?.to_string_lossy();
        files.push(IncomingFile {
            size: std::fs::metadata(&path).ok().map(|m| m.len()),
            path,
            name: name.to_string_lossy(),
            mime_type: file
                .ContentType()
                .ok()
                .map(|t| t.to_string_lossy())
                .filter(|t| !t.is_empty()),
        });
    }
    Ok(files)
}
