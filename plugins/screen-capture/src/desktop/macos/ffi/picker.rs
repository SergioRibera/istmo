//! `SCContentSharingPicker` (macOS 14+): the system's own picker, whose
//! choice comes back as a content filter.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::{Duration, Instant};

use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{AllocAnyThread, DefinedClass, define_class, msg_send};
use objc2_foundation::NSError;
use objc2_screen_capture_kit::{
    SCContentFilter, SCContentSharingPicker, SCContentSharingPickerMode,
    SCContentSharingPickerObserver, SCStream,
};

use crate::types::{CaptureError, PickerOptions, SourceKind};

use super::{Filter, Shared, error_of};

/// Longest a person may take to pick.
const PICK_TIMEOUT: Duration = Duration::from_secs(120);
/// How often a pending pick checks whether its caller still waits.
const PICK_POLL: Duration = Duration::from_millis(250);

type Outcome = Result<Shared<SCContentFilter>, CaptureError>;

struct ObserverIvars {
    tx: flume::Sender<Outcome>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; no `Drop`.
    #[unsafe(super(NSObject))]
    #[name = "IstmoScreenCapturePickerObserver"]
    #[ivars = ObserverIvars]
    struct PickerObserver;

    unsafe impl NSObjectProtocol for PickerObserver {}

    unsafe impl SCContentSharingPickerObserver for PickerObserver {
        #[unsafe(method(contentSharingPicker:didCancelForStream:))]
        fn did_cancel(&self, _picker: &SCContentSharingPicker, _stream: Option<&SCStream>) {
            let _sent = self.ivars().tx.try_send(Err(CaptureError::ConsentDenied));
        }

        #[unsafe(method(contentSharingPicker:didUpdateWithFilter:forStream:))]
        fn did_update(
            &self,
            _picker: &SCContentSharingPicker,
            filter: &SCContentFilter,
            _stream: Option<&SCStream>,
        ) {
            let filter = Shared(objc2::Message::retain(filter));
            let _sent = self.ivars().tx.try_send(Ok(filter));
        }

        #[unsafe(method(contentSharingPickerStartDidFailWithError:))]
        fn did_fail(&self, error: &NSError) {
            let _sent = self.ivars().tx.try_send(Err(error_of(error, "picker")));
        }
    }
);

impl PickerObserver {
    fn new(tx: flume::Sender<Outcome>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(ObserverIvars { tx });
        // SAFETY: NSObject's `init` on a freshly allocated object.
        unsafe { msg_send![super(this), init] }
    }
}

/// The picker modes `options` allows.
fn modes(options: &PickerOptions) -> SCContentSharingPickerMode {
    let kinds: &[SourceKind] = if options.kinds.is_empty() {
        &[
            SourceKind::Monitor,
            SourceKind::Window,
            SourceKind::Application,
        ]
    } else {
        &options.kinds
    };
    let mut modes = SCContentSharingPickerMode::empty();
    for kind in kinds {
        modes |= match (kind, options.multiple) {
            (SourceKind::Monitor, _) => SCContentSharingPickerMode::SingleDisplay,
            (SourceKind::Window, false) => SCContentSharingPickerMode::SingleWindow,
            (SourceKind::Window, true) => SCContentSharingPickerMode::MultipleWindows,
            (SourceKind::Application, false) => SCContentSharingPickerMode::SingleApplication,
            (SourceKind::Application, true) => SCContentSharingPickerMode::MultipleApplications,
        };
    }
    modes
}

/// Show the shared picker and wait for the person's choice, while
/// `waiting()` holds. The picker needs a running `NSApplication` (an app
/// with a main run loop).
///
/// # Errors
/// `ConsentDenied` when cancelled; `ConsentRequired` when nobody waits
/// any more or the person took too long; the framework's error when the
/// picker could not start.
pub(crate) fn pick(
    options: &PickerOptions,
    waiting: impl Fn() -> bool,
) -> Result<Filter, CaptureError> {
    let (tx, rx) = flume::bounded(1);
    let observer = Shared(PickerObserver::new(tx));
    let modes = modes(options);
    let shown = std::sync::Arc::new(observer);
    let present = std::sync::Arc::clone(&shown);
    DispatchQueue::main().exec_async(move || {
        // SAFETY: the shared picker, used
        // from the main queue as AppKit UI must be; the observer stays
        // alive (`present` holds it) until it is removed again.
        let picker = unsafe { SCContentSharingPicker::sharedPicker() };
        // SAFETY: as above.
        let config = unsafe { picker.defaultConfiguration() };
        // SAFETY: as above.
        unsafe { config.setAllowedPickerModes(modes) };
        // SAFETY: as above.
        unsafe { picker.setDefaultConfiguration(&config) };
        // SAFETY: as above.
        unsafe { picker.addObserver(ProtocolObject::from_ref(&**present)) };
        // SAFETY: as above.
        unsafe { picker.setActive(true) };
        // SAFETY: as above.
        unsafe { picker.present() };
    });
    let deadline = Instant::now() + PICK_TIMEOUT;
    let outcome = loop {
        match rx.recv_timeout(PICK_POLL) {
            Ok(outcome) => break outcome,
            Err(flume::RecvTimeoutError::Disconnected) => {
                break Err(CaptureError::protocol(
                    super::ID,
                    "picker",
                    "the observer went away",
                ));
            }
            Err(flume::RecvTimeoutError::Timeout) => {
                if !waiting() || Instant::now() >= deadline {
                    break Err(CaptureError::ConsentRequired);
                }
            }
        }
    };
    DispatchQueue::main().exec_async(move || {
        let observer = catch_unwind(AssertUnwindSafe(|| {
            // SAFETY: undoes the registration above, on the main queue.
            let picker = unsafe { SCContentSharingPicker::sharedPicker() };
            // SAFETY: as above.
            unsafe { picker.removeObserver(ProtocolObject::from_ref(&**shown)) };
            // SAFETY: as above.
            unsafe { picker.setActive(false) };
        }));
        if observer.is_err() {
            tracing::error!("screen-capture: closing the picker panicked");
        }
    });
    outcome.map(Filter)
}
