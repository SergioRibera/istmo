//! macOS: documents opened from Finder, the Dock or `open file.ext`.
//!
//! `LaunchServices` does not put these on the command line. It sends an
//! `odoc` Apple Event to the app — to the running instance when there is
//! one, without starting a second process — so the socket never sees
//! them. A small Objective-C object registers for that event and
//! publishes each file on the documents queue.
//!
//! `NSApplication` installs its own `odoc` handler while it finishes
//! launching, which would replace one registered earlier. The handler is
//! therefore registered twice: right away (for an app that is already
//! running) and again on `NSApplicationWillFinishLaunchingNotification`,
//! which Apple documents as the moment to install Apple Event handlers
//! so the launch documents reach them.

use std::ffi::{CStr, c_char};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock, Weak};

use istmo_core::Runtime;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, NSObject};
use objc2::{ClassType, define_class, msg_send, sel};

use crate::documents::{DocumentOrigin, Documents, OpenedDocument};

/// `kCoreEventClass` (`'aevt'`).
const CORE_EVENT_CLASS: u32 = u32::from_be_bytes(*b"aevt");
/// `kAEOpenDocuments` (`'odoc'`).
const OPEN_DOCUMENTS: u32 = u32::from_be_bytes(*b"odoc");
/// `keyDirectObject` (`'----'`).
const DIRECT_OBJECT: u32 = u32::from_be_bytes(*b"----");

/// The runtime documents are published to. Set once by [`install`].
static RUNTIME: OnceLock<Weak<Runtime>> = OnceLock::new();

define_class!(
    // SAFETY: `NSObject` has no subclassing requirements and `Opener`
    // does not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[name = "IstmoDocumentsOpener"]
    struct Opener;

    impl Opener {
        #[unsafe(method(istmoWillFinishLaunching:))]
        fn will_finish_launching(&self, _notification: &AnyObject) {
            register(self);
        }

        #[unsafe(method(istmoOpenDocuments:withReplyEvent:))]
        fn open_documents(&self, event: &AnyObject, _reply: &AnyObject) {
            deliver(event);
        }
    }
);

/// Start receiving `odoc` events. Must run on the main thread, before
/// the event loop runs; later calls do nothing.
pub(super) fn install(runtime: &Arc<Runtime>) {
    if RUNTIME.set(Arc::downgrade(runtime)).is_err() {
        return;
    }
    let Some(thread) = AnyClass::get(c"NSThread") else {
        return;
    };
    // SAFETY: `+[NSThread isMainThread]` takes no arguments and returns
    // a `BOOL`.
    let main: bool = unsafe { msg_send![thread, isMainThread] };
    if !main {
        tracing::warn!(target: "istmo.documents", "documents from Finder need the publisher on the main thread");
        return;
    }
    // SAFETY: `+new` on an `NSObject` subclass returns a retained
    // instance.
    let opener: Retained<Opener> = unsafe { msg_send![Opener::class(), new] };
    register(&opener);
    observe_will_finish_launching(&opener);
    // One per process, alive for as long as the app runs: the Apple
    // Event manager and the notification center do not retain it.
    std::mem::forget(opener);
}

/// Make `opener` the `odoc` handler.
fn register(opener: &Opener) {
    let Some(manager_class) = AnyClass::get(c"NSAppleEventManager") else {
        return;
    };
    // SAFETY: `+sharedAppleEventManager` takes no arguments and returns
    // the shared instance.
    let manager: Option<Retained<AnyObject>> =
        unsafe { msg_send![manager_class, sharedAppleEventManager] };
    let Some(manager) = manager else {
        return;
    };
    let handler: &AnyObject = opener;
    // SAFETY: `handler` implements the selector with the
    // `(NSAppleEventDescriptor *, NSAppleEventDescriptor *)` signature
    // the manager calls; event class and id are `FourCharCode`s.
    unsafe {
        let () = msg_send![
            &*manager,
            setEventHandler: handler,
            andSelector: sel!(istmoOpenDocuments:withReplyEvent:),
            forEventClass: CORE_EVENT_CLASS,
            andEventID: OPEN_DOCUMENTS
        ];
    }
}

fn observe_will_finish_launching(opener: &Opener) {
    let (Some(center_class), Some(string_class)) = (
        AnyClass::get(c"NSNotificationCenter"),
        AnyClass::get(c"NSString"),
    ) else {
        return;
    };
    // SAFETY: `+defaultCenter` returns the shared center;
    // `+stringWithUTF8String:` copies the NUL-terminated literal.
    let (center, name): (Option<Retained<AnyObject>>, Option<Retained<AnyObject>>) = unsafe {
        (
            msg_send![center_class, defaultCenter],
            msg_send![
                string_class,
                stringWithUTF8String: c"NSApplicationWillFinishLaunchingNotification".as_ptr()
            ],
        )
    };
    let (Some(center), Some(name)) = (center, name) else {
        return;
    };
    let observer: &AnyObject = opener;
    // SAFETY: `observer` implements `istmoWillFinishLaunching:` taking
    // the notification; a null object observes every sender.
    unsafe {
        let () = msg_send![
            &*center,
            addObserver: observer,
            selector: sel!(istmoWillFinishLaunching:),
            name: &*name,
            object: std::ptr::null_mut::<AnyObject>()
        ];
    }
}

/// Publish every file URL in an `odoc` event.
fn deliver(event: &AnyObject) {
    let Some(runtime) = RUNTIME.get().and_then(Weak::upgrade) else {
        return;
    };
    // SAFETY: `event` is the `NSAppleEventDescriptor` of an `odoc`
    // event; the getters take plain integers and return autoreleased
    // descriptors or nil.
    let list: Option<Retained<AnyObject>> =
        unsafe { msg_send![event, paramDescriptorForKeyword: DIRECT_OBJECT] };
    let Some(list) = list else {
        return;
    };
    // SAFETY: as above.
    let count: isize = unsafe { msg_send![&*list, numberOfItems] };
    let items: Vec<Retained<AnyObject>> = if count == 0 {
        // A single file can come as one descriptor instead of a list.
        vec![list]
    } else {
        (1..=count)
            // SAFETY: AE lists are 1-based; `index <= count`.
            .filter_map(|index| unsafe { msg_send![&*list, descriptorAtIndex: index] })
            .collect()
    };
    for item in items {
        let Some(path) = file_path(&item) else {
            continue;
        };
        if let Some(document) = OpenedDocument::from_path(path, DocumentOrigin::Running) {
            if let Err(err) = Documents::publish(&runtime, &document) {
                tracing::warn!(target: "istmo.documents", %err, "publishing a document from Finder");
            }
        }
    }
}

/// Path of a descriptor that holds a file URL.
fn file_path(descriptor: &AnyObject) -> Option<PathBuf> {
    // SAFETY: `-fileURLValue` (macOS 10.11+) coerces the descriptor and
    // returns an `NSURL` or nil; `-path` returns an `NSString` or nil.
    let url: Option<Retained<AnyObject>> = unsafe { msg_send![descriptor, fileURLValue] };
    let url = url?;
    // SAFETY: as above.
    let path: Option<Retained<AnyObject>> = unsafe { msg_send![&*url, path] };
    let path = path?;
    // SAFETY: `-UTF8String` returns a NUL-terminated buffer owned by
    // `path`, which stays alive until the copy below is done.
    let utf8: *const c_char = unsafe { msg_send![&*path, UTF8String] };
    if utf8.is_null() {
        return None;
    }
    // SAFETY: non-null, NUL-terminated and alive (see above).
    let text = unsafe { CStr::from_ptr(utf8) }.to_str().ok()?;
    Some(PathBuf::from(text))
}
