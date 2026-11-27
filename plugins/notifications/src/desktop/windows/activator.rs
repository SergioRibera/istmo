//! The COM activator: Windows calls
//! `INotificationActivationCallback::Activate` for a tap, a button or a
//! reply, in the running app or in one COM starts for it
//! (`LocalServer32` = `"<exe>" -istmo-notification`, COM appends
//! `-Embedding`).
//!
//! The class object is registered (`CLSCTX_LOCAL_SERVER`,
//! `REGCLS_MULTIPLEUSE`) on a dedicated MTA thread that stays in the
//! apartment until [`revoke`] (a forwarding instance of a single-instance
//! app hands activations back to COM, which then routes them to the
//! primary's class object). Activations route to the host's sink; those that arrive
//! before the backend started (cold start: COM calls in as soon as the
//! class object exists) wait in a small queue and are replayed from a
//! thread of their own once the backend started, so no sink call runs
//! inside the backend start.

#![allow(
    unsafe_code,
    reason = "COM server: class object registration and the raw arguments of Activate"
)]
#![allow(
    clippy::inline_always,
    clippy::ref_as_ptr,
    reason = "the COM glue `#[implement]` generates"
)]

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use istmo_core::sync::lock;
use windows::Win32::Foundation::{CLASS_E_NOAGGREGATION, E_POINTER};
use windows::Win32::System::Com::{
    CLSCTX_LOCAL_SERVER, COINIT_MULTITHREADED, CoInitializeEx, CoRegisterClassObject,
    CoRevokeClassObject, CoUninitialize, IClassFactory, IClassFactory_Impl, REGCLS_MULTIPLEUSE,
};
use windows::Win32::UI::Notifications::{
    INotificationActivationCallback, INotificationActivationCallback_Impl,
    NOTIFICATION_USER_INPUT_DATA,
};
use windows_core::{BOOL, GUID, IUnknown, Interface, PCWSTR, Ref, implement};

use super::args;
use crate::desktop::{Activation, BackendSink};

/// Activations kept before the backend starts.
const MAX_PENDING: usize = 64;

struct Router {
    sink: Option<BackendSink>,
    pending: VecDeque<Activation>,
}

static ROUTER: Mutex<Router> = Mutex::new(Router {
    sink: None,
    pending: VecDeque::new(),
});

/// COM started this process for an activation: the first one reports
/// `launched_app`.
static LAUNCHED: AtomicBool = AtomicBool::new(false);

/// Marks this process as started by COM.
pub(super) fn mark_launched() {
    LAUNCHED.store(true, Ordering::Release);
}

/// Routes activations to `sink` from now on; replays the queued ones
/// from a thread of their own (the caller is the backend start).
pub(super) fn attach(sink: BackendSink) {
    let pending: Vec<Activation> = {
        let mut router = lock(&ROUTER);
        router.sink = Some(sink.clone());
        router.pending.drain(..).collect()
    };
    if pending.is_empty() {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("istmo-notifications-replay".to_owned())
        .spawn(move || {
            for activation in pending {
                sink.activated(activation);
            }
        });
    if let Err(err) = spawned {
        tracing::warn!(%err, "notifications: queued activations dropped");
    }
}

fn route(raw: &str, inputs: &[(String, String)]) {
    let launched = LAUNCHED.swap(false, Ordering::AcqRel);
    let Some(activation) = args::activation(raw, inputs, launched) else {
        tracing::debug!("notifications: activation with foreign arguments ignored");
        return;
    };
    let sink = {
        let mut router = lock(&ROUTER);
        let Some(sink) = router.sink.clone() else {
            if router.pending.len() == MAX_PENDING {
                router.pending.pop_front();
            }
            router.pending.push_back(activation);
            return;
        };
        sink
    };
    sink.activated(activation);
}

/// A wide string from COM, empty when null or not UTF-16.
fn wide(value: PCWSTR) -> String {
    if value.is_null() {
        return String::new();
    }
    // SAFETY: COM hands NUL-terminated strings valid for the call.
    unsafe { value.to_string() }.unwrap_or_default()
}

#[implement(INotificationActivationCallback)]
struct Activator;

impl INotificationActivationCallback_Impl for Activator_Impl {
    fn Activate(
        &self,
        _appusermodelid: &PCWSTR,
        invokedargs: &PCWSTR,
        data: *const NOTIFICATION_USER_INPUT_DATA,
        count: u32,
    ) -> windows_core::Result<()> {
        let raw = wide(*invokedargs);
        let mut inputs = Vec::new();
        if !data.is_null() && count > 0 {
            let len = usize::try_from(count).unwrap_or(0);
            // SAFETY: COM passes `count` entries at `data`, valid and
            // unchanged for the duration of the call.
            let entries = unsafe { std::slice::from_raw_parts(data, len) };
            inputs.reserve(entries.len());
            for entry in entries {
                inputs.push((wide(entry.Key), wide(entry.Value)));
            }
        }
        // Returns at once: the app works on the response in its own time.
        route(&raw, &inputs);
        Ok(())
    }
}

#[implement(IClassFactory)]
struct Factory;

impl IClassFactory_Impl for Factory_Impl {
    fn CreateInstance(
        &self,
        outer: Ref<IUnknown>,
        riid: *const GUID,
        object: *mut *mut core::ffi::c_void,
    ) -> windows_core::Result<()> {
        if object.is_null() || riid.is_null() {
            return Err(E_POINTER.into());
        }
        // SAFETY: `object` is COM's non-null out pointer for this call.
        unsafe { object.write(core::ptr::null_mut()) };
        if outer.is_some() {
            return Err(CLASS_E_NOAGGREGATION.into());
        }
        let activator: IUnknown = Activator.into();
        // SAFETY: `riid` and `object` are COM's valid pointers for the call;
        // `query` AddRefs what it hands out.
        unsafe { activator.query(riid, object) }.ok()
    }

    fn LockServer(&self, _lock: BOOL) -> windows_core::Result<()> {
        Ok(())
    }
}

/// Asks the COM thread to revoke the class object; answered once done.
type Revoke = flume::Sender<windows_core::Result<()>>;

/// The COM thread's mailbox while the class object is registered.
static SERVING: Mutex<Option<flume::Sender<Revoke>>> = Mutex::new(None);

/// Registers the class object of `clsid` on a thread that stays in the
/// MTA until [`revoke`] (else for the life of the process).
pub(super) fn serve(clsid: GUID) -> windows_core::Result<()> {
    let (tx, rx) = flume::bounded(1);
    let (revoke_tx, revoke_rx) = flume::bounded::<Revoke>(1);
    std::thread::Builder::new()
        .name("istmo-notifications-com".to_owned())
        .spawn(move || {
            let cookie = match register(&clsid) {
                Ok(cookie) => cookie,
                Err(err) => {
                    let _sent = tx.send(Err(err));
                    return;
                }
            };
            let _sent = tx.send(Ok(()));
            // The apartment (and with it the registration) lives while
            // this thread waits: for a revoke, else for the life of the
            // process (the sender stays in `SERVING`).

            if let Ok(done) = revoke_rx.recv() {
                // SAFETY: `cookie` came from `CoRegisterClassObject` on
                // this thread and is revoked once.
                let revoked = unsafe { CoRevokeClassObject(cookie) };
                // SAFETY: pairs the `CoInitializeEx` of `register` on
                // this thread; nothing COM is used here afterwards.
                unsafe { CoUninitialize() };
                let _answered = done.send(revoked);
            }
        })
        .map_err(|err| windows_core::Error::new(windows_core::HRESULT(-1), err.to_string()))?;
    rx.recv()
        .map_err(|_| windows_core::Error::from_hresult(windows_core::HRESULT(-1)))??;
    *lock(&SERVING) = Some(revoke_tx);
    Ok(())
}

/// Revokes the class object [`serve`] registered: COM stops routing
/// activations to this process. `false` when none was registered (or it
/// was revoked already).
pub(super) fn revoke() -> windows_core::Result<bool> {
    let Some(mailbox) = lock(&SERVING).take() else {
        return Ok(false);
    };
    let (tx, rx) = flume::bounded(1);
    if mailbox.send(tx).is_err() {
        return Ok(false);
    }
    rx.recv()
        .map_err(|_| windows_core::Error::from_hresult(windows_core::HRESULT(-1)))??;
    Ok(true)
}

/// Joins the MTA and registers the class object; its cookie.
fn register(clsid: &GUID) -> windows_core::Result<u32> {
    // SAFETY: first COM call on this new thread; it joins the MTA.
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.ok()?;
    let factory: IClassFactory = Factory.into();
    // SAFETY: `clsid` outlives the call; COM AddRefs `factory`.
    unsafe { CoRegisterClassObject(clsid, &factory, CLSCTX_LOCAL_SERVER, REGCLS_MULTIPLEUSE) }
}
