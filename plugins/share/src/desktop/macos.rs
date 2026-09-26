//! macOS: `NSSharingServicePicker` anchored on the registered window's
//! `NSView`, driven on the main queue.
//!
//! Outcome mapping:
//! * picker closed without a choice → [`ShareOutcome::Dismissed`];
//! * service reports success → [`ShareOutcome::Shared`] with the
//!   service title;
//! * service reports failure → [`ShareError::Backend`];
//! * service chosen but silent for [`SERVICE_REPORT_TIMEOUT`] (some
//!   services never call back) → [`ShareOutcome::TargetChosen`], so a
//!   quiet service cannot leave the backend busy forever.

use std::cell::RefCell;
use std::ffi::c_void;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use dispatch2::{DispatchQueue, DispatchTime};
use flume::Sender;
use istmo_window::{NativeWindow, WindowRegistry};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSSharingService, NSSharingServiceDelegate, NSSharingServicePicker,
    NSSharingServicePickerDelegate, NSView,
};
use objc2_core_foundation::{
    CFDictionary, CFNotificationCenter, CFNotificationName, CFNotificationSuspensionBehavior,
    CFString,
};
use objc2_foundation::{NSArray, NSError, NSPoint, NSRect, NSRectEdge, NSSize, NSString, NSURL};

use istmo_core::CancelToken;

use super::{ShareJob, until_cancelled};
use crate::{ShareCapabilities, ShareError, ShareInbox, ShareOutcome, ShareRect};

pub(super) const fn capabilities() -> ShareCapabilities {
    ShareCapabilities {
        send: true,
        files: true,
        mixed_content: true,
        rich_preview: false,
        reports_completion: true,
        reports_target: true,
        receive: true,
        direct_share: false,
        dismiss_on_cancel: true,
    }
}

pub(super) const UNSUPPORTED_REASON: &str = "";

type Reply = Sender<Result<ShareOutcome, ShareError>>;

/// How long a chosen service may stay silent before the share resolves
/// as [`ShareOutcome::TargetChosen`].
const SERVICE_REPORT_TIMEOUT: Duration = Duration::from_secs(60);

/// Identifies the sheet in [`ACTIVE`], so a late timeout cannot finish
/// a newer sheet.
static GENERATION: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
pub(super) struct Backend;

impl Backend {
    #[allow(clippy::unnecessary_wraps)]
    pub(super) const fn attach(_registry: &WindowRegistry) -> Result<Self, ShareError> {
        Ok(Self)
    }

    pub(super) async fn present(
        &self,
        job: ShareJob,
        cancel: &CancelToken,
    ) -> Result<ShareOutcome, ShareError> {
        let (_, window) = job.window;
        let NativeWindow::AppKit { ns_view } = window else {
            return Err(ShareError::NoPresenter(format!(
                "{window:?} is not an AppKit window"
            )));
        };
        let payload = Payload {
            text: job.request.text.clone(),
            url: job.request.url.clone(),
            files: job
                .files
                .iter()
                .map(|f| f.path.display().to_string())
                .collect(),
            rect: job.request.anchor.and_then(|a| a.rect),
            ns_view: ns_view.get(),
        };
        let (tx, rx) = flume::bounded(1);
        DispatchQueue::main().exec_async(move || {
            let Some(mtm) = MainThreadMarker::new() else {
                drop(tx.send(Err(ShareError::Backend(
                    "main queue did not run on the main thread".to_owned(),
                ))));
                return;
            };
            if let Err(err) = show(mtm, &payload, tx.clone()) {
                drop(tx.send(Err(err)));
            }
        });
        let Some(reply) = until_cancelled(cancel, rx.recv_async()).await else {
            // The caller dropped the share: close the picker. Its
            // delegate reports a `nil` service, which clears ACTIVE.
            DispatchQueue::main().exec_async(|| {
                let picker = ACTIVE.with(|active| active.borrow().as_ref().map(|(p, _)| p.clone()));
                if let Some(picker) = picker {
                    picker.close();
                }
            });
            return Ok(ShareOutcome::Dismissed);
        };
        reply
            .map_err(|_| ShareError::Backend("share picker closed without reporting".to_owned()))?
    }
}

/// `Send` subset of the job handed to the main queue.
struct Payload {
    text: Option<String>,
    url: Option<String>,
    files: Vec<String>,
    rect: Option<ShareRect>,
    ns_view: usize,
}

thread_local! {
    /// Picker + delegate of the sheet currently on screen. The picker
    /// only holds its delegate weakly, so both live here until the
    /// outcome is reported. Main thread only.
    static ACTIVE: RefCell<Option<(Retained<NSSharingServicePicker>, Retained<ShareDelegate>)>> =
        const { RefCell::new(None) };
}

fn show(mtm: MainThreadMarker, payload: &Payload, reply: Reply) -> Result<(), ShareError> {
    let items = build_items(payload)?;
    // SAFETY: `ns_view` came from a live `NSView*` registered by the app
    // in the istmo-window registry; the app keeps the window alive
    // while it stays registered. Retaining it here keeps it alive for
    // the duration of the call.
    let view: Retained<NSView> = unsafe { Retained::retain(payload.ns_view as *mut NSView) }
        .ok_or_else(|| ShareError::NoPresenter("null NSView".to_owned()))?;

    // SAFETY: `items` holds NSString / NSURL objects, the element types
    // NSSharingServicePicker accepts.
    let picker =
        unsafe { NSSharingServicePicker::initWithItems(NSSharingServicePicker::alloc(), &items) };
    let delegate = ShareDelegate::new(mtm, reply);
    picker.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));

    let rect = anchor_rect(&view, payload.rect);
    picker.showRelativeToRect_ofView_preferredEdge(rect, &view, NSRectEdge::MinY);
    ACTIVE.with(|active| *active.borrow_mut() = Some((picker, delegate)));
    Ok(())
}

fn build_items(payload: &Payload) -> Result<Retained<NSArray<AnyObject>>, ShareError> {
    let mut items: Vec<Retained<AnyObject>> = Vec::new();
    if let Some(text) = &payload.text {
        items.push(Retained::into_super(Retained::into_super(
            NSString::from_str(text),
        )));
    }
    if let Some(url) = &payload.url {
        let url = NSURL::URLWithString(&NSString::from_str(url))
            .ok_or_else(|| ShareError::InvalidRequest(format!("not a valid URL: {url}")))?;
        items.push(Retained::into_super(Retained::into_super(url)));
    }
    for path in &payload.files {
        if !Path::new(path).is_file() {
            return Err(ShareError::NotFound(path.clone()));
        }
        let url = NSURL::fileURLWithPath(&NSString::from_str(path));
        items.push(Retained::into_super(Retained::into_super(url)));
    }
    Ok(NSArray::from_retained_slice(&items))
}

/// Convert a top-left-origin rect into the view's coordinate space.
fn anchor_rect(view: &NSView, rect: Option<ShareRect>) -> NSRect {
    let bounds = view.bounds();
    let Some(rect) = rect else {
        return NSRect::new(
            NSPoint::new(bounds.size.width / 2.0, bounds.size.height / 2.0),
            NSSize::new(1.0, 1.0),
        );
    };
    let y = if view.isFlipped() {
        rect.y
    } else {
        bounds.size.height - rect.y - rect.height
    };
    NSRect::new(
        NSPoint::new(rect.x, y),
        NSSize::new(rect.width, rect.height),
    )
}

fn finish(outcome: Result<ShareOutcome, ShareError>, reply: &Reply) {
    drop(reply.send(outcome));
    ACTIVE.with(|active| active.borrow_mut().take());
}

/// Resolve sheet `generation` as [`ShareOutcome::TargetChosen`] if it
/// is still waiting for its service after [`SERVICE_REPORT_TIMEOUT`].
fn schedule_service_timeout(generation: u64) {
    let Ok(when) = DispatchTime::try_from(SERVICE_REPORT_TIMEOUT) else {
        return;
    };
    let scheduled = DispatchQueue::main().after(when, move || {
        let pending = ACTIVE.with(|active| {
            active.borrow().as_ref().and_then(|(_, delegate)| {
                let ivars = delegate.ivars();
                (ivars.generation == generation)
                    .then(|| (ivars.reply.clone(), ivars.chosen.borrow().clone()))
            })
        });
        if let Some((reply, chosen)) = pending {
            let outcome = chosen.map_or(ShareOutcome::Unknown, ShareOutcome::TargetChosen);
            finish(Ok(outcome), &reply);
        }
    });
    if let Err(err) = scheduled {
        tracing::warn!(?err, "scheduling share service timeout");
    }
}

#[derive(Debug)]
struct DelegateIvars {
    reply: Reply,
    generation: u64,
    chosen: RefCell<Option<String>>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; the delegate
    // does not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "IstmoShareDelegate"]
    #[ivars = DelegateIvars]
    struct ShareDelegate;

    unsafe impl NSObjectProtocol for ShareDelegate {}

    unsafe impl NSSharingServicePickerDelegate for ShareDelegate {
        #[unsafe(method(sharingServicePicker:didChooseSharingService:))]
        fn did_choose(&self, _picker: &NSSharingServicePicker, service: Option<&NSSharingService>) {
            match service {
                // Keep the picker alive: the service reports back through
                // `sharingService:didShareItems:` below.
                Some(service) => {
                    *self.ivars().chosen.borrow_mut() = Some(service.title().to_string());
                    service.setDelegate(Some(ProtocolObject::from_ref(self)));
                    schedule_service_timeout(self.ivars().generation);
                }
                None => finish(Ok(ShareOutcome::Dismissed), &self.ivars().reply),
            }
        }
    }

    unsafe impl NSSharingServiceDelegate for ShareDelegate {
        #[unsafe(method(sharingService:didShareItems:))]
        fn did_share(&self, service: &NSSharingService, _items: &NSArray) {
            let title = service.title().to_string();
            finish(Ok(ShareOutcome::Shared(Some(title))), &self.ivars().reply);
        }

        #[unsafe(method(sharingService:didFailToShareItems:error:))]
        fn did_fail(&self, _service: &NSSharingService, _items: &NSArray, error: &NSError) {
            let message = error.localizedDescription().to_string();
            // NSUserCancelledError (3072): the user backed out of the
            // service's own UI.
            let outcome = if error.code() == 3072 {
                Ok(ShareOutcome::Dismissed)
            } else {
                Err(ShareError::Backend(message))
            };
            finish(outcome, &self.ivars().reply);
        }
    }
);

impl ShareDelegate {
    fn new(mtm: MainThreadMarker, reply: Reply) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(DelegateIvars {
            reply,
            generation: GENERATION.fetch_add(1, Ordering::Relaxed),
            chosen: RefCell::new(None),
        });
        // SAFETY: `init` is NSObject's designated initialiser.
        unsafe { msg_send![super(this), init] }
    }
}

/// Publish every share the Share Extension left in App Group `group_id`.
///
/// See `templates/apple-share-extension`. Files are moved to
/// `~/Library/Caches/istmo-share/incoming/`. Call at startup and
/// whenever the app becomes active. Returns how many were published.
pub fn drain_app_group_inbox(group_id: &str, inbox: &ShareInbox) -> Result<usize, ShareError> {
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .ok_or_else(|| ShareError::Io("HOME is not set".to_owned()))?;
    let inbox_dir = home
        .join("Library/Group Containers")
        .join(group_id)
        .join("istmo-share/inbox");
    let dest = home.join("Library/Caches/istmo-share/incoming");
    super::prune_stale_entries(&dest, crate::INCOMING_RETENTION);
    super::handoff::drain_dir(&inbox_dir, &dest, inbox)
}

/// App Groups whose hand-off notification this process observes.
static OBSERVED_GROUPS: Mutex<Vec<(String, ShareInbox)>> = Mutex::new(Vec::new());

/// Drain App Group `group_id` whenever the Share Extension commits a share.
///
/// Works while the app is running (the extension posts a Darwin
/// notification). Call once at startup, alongside an initial
/// [`drain_app_group_inbox`]; the extension also launches the app when
/// it is not running, which then drains at startup.
pub fn observe_app_group_handoffs(group_id: &str, inbox: &ShareInbox) -> Result<(), ShareError> {
    let center = CFNotificationCenter::darwin_notify_center()
        .ok_or_else(|| ShareError::Backend("Darwin notify center unavailable".to_owned()))?;
    let name = CFString::from_str(&format!("{group_id}.istmo-share.handoff"));
    {
        let mut groups = OBSERVED_GROUPS
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if groups.iter().any(|(group, _)| group == group_id) {
            return Ok(());
        }
        groups.push((group_id.to_owned(), inbox.clone()));
    }
    // SAFETY: the observer is the address of a static (never freed), the
    // callback matches CFNotificationCallback and only touches
    // `OBSERVED_GROUPS`, and the Darwin center ignores `object`.
    unsafe {
        center.add_observer(
            (&raw const OBSERVED_GROUPS).cast(),
            Some(on_handoff),
            Some(&name),
            std::ptr::null(),
            CFNotificationSuspensionBehavior::DeliverImmediately,
        );
    }
    Ok(())
}

unsafe extern "C-unwind" fn on_handoff(
    _center: *mut CFNotificationCenter,
    _observer: *mut c_void,
    _name: *const CFNotificationName,
    _object: *const c_void,
    _info: *const CFDictionary,
) {
    let groups = OBSERVED_GROUPS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    for (group, inbox) in groups {
        if let Err(err) = drain_app_group_inbox(&group, &inbox) {
            tracing::warn!(%err, group, "draining share inbox after hand-off");
        }
    }
}
