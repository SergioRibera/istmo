//! Drags out of macOS windows: an `NSDraggingSession` from the window's
//! view, started with the mouse event of the press in progress.
//!
//! Files go as `NSURL`s, links as URLs and text as `NSString`s; each item
//! shows the app's preview or the file's icon. A small source object says
//! the drag copies and reports how it ended.

use std::cell::RefCell;
use std::num::NonZeroUsize;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSApplication, NSDragOperation, NSDraggingContext, NSDraggingItem, NSDraggingSession,
    NSDraggingSource, NSEventType, NSImage, NSPasteboardWriting, NSView, NSWorkspace,
};
use objc2_foundation::{
    NSArray, NSData, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString, NSURL,
};

use super::DragDone;
use crate::{DndError, DragItem, DragRequest, DragResult};

/// Size of each item's image when the app gives no preview.
const ICON: f64 = 48.0;

pub(super) fn start_drag(ns_view: NonZeroUsize, request: DragRequest, done: DragDone) {
    let address = ns_view.get();
    dispatch2::run_on_main(move |mtm| begin(mtm, address, &request, done));
}

fn begin(mtm: MainThreadMarker, address: usize, request: &DragRequest, done: DragDone) {
    // SAFETY: `address` is the `NSView` of a window registered with
    // `istmo-window`, alive while it stays registered; this runs on the
    // main thread, which owns it.
    let view: &NSView = unsafe { &*(address as *const NSView) };
    let app = NSApplication::sharedApplication(mtm);
    let Some(event) = app.currentEvent() else {
        return done.finish(Err(DndError::NotPressed));
    };
    let mouse = matches!(
        event.r#type(),
        NSEventType::LeftMouseDown
            | NSEventType::LeftMouseDragged
            | NSEventType::RightMouseDown
            | NSEventType::RightMouseDragged
            | NSEventType::OtherMouseDown
            | NSEventType::OtherMouseDragged
    );
    if !mouse {
        return done.finish(Err(DndError::NotPressed));
    }
    let at = view.convertPoint_fromView(event.locationInWindow(), None);
    let preview = request.preview.as_ref().and_then(|preview| {
        let data = NSData::with_bytes(&preview.png);
        let image = NSImage::initWithData(NSImage::alloc(), &data)?;
        Some((
            image,
            NSSize::new(f64::from(preview.width), f64::from(preview.height)),
        ))
    });
    let workspace = NSWorkspace::sharedWorkspace();
    let mut items = Vec::new();
    for (index, item) in request.items.iter().enumerate() {
        let (writer, icon): (
            Retained<ProtocolObject<dyn NSPasteboardWriting>>,
            Retained<NSImage>,
        ) = match item {
            DragItem::File(file) => {
                let path = NSString::from_str(&file.path);
                let url = NSURL::fileURLWithPath(&path);
                (
                    ProtocolObject::from_retained(url),
                    workspace.iconForFile(&path),
                )
            }
            DragItem::Url(link) => {
                let Some(url) = NSURL::URLWithString(&NSString::from_str(link)) else {
                    continue;
                };
                (ProtocolObject::from_retained(url), text_icon(&workspace))
            }
            DragItem::Text(text) => (
                ProtocolObject::from_retained(NSString::from_str(text)),
                text_icon(&workspace),
            ),
        };
        let dragging = NSDraggingItem::initWithPasteboardWriter(NSDraggingItem::alloc(), &writer);
        let (image, size) = preview
            .clone()
            .unwrap_or_else(|| (icon, NSSize::new(ICON, ICON)));
        #[allow(clippy::cast_precision_loss, reason = "a handful of items")]
        let offset = index as f64 * 6.0;
        let frame = NSRect::new(
            NSPoint::new(
                at.x - size.width / 2.0 + offset,
                at.y - size.height / 2.0 - offset,
            ),
            size,
        );
        // SAFETY: an image for the item's frame, as AppKit expects.
        unsafe { dragging.setDraggingFrame_contents(frame, Some(&image)) };
        items.push(dragging);
    }
    if items.is_empty() {
        return done.finish(Err(DndError::Backend("nothing to drag".into())));
    }
    let items = NSArray::from_retained_slice(&items);
    let source = DragSource::new(mtm, done);
    let _session = view.beginDraggingSessionWithItems_event_source(
        &items,
        &event,
        ProtocolObject::from_ref(&*source),
    );
    // The session does not keep its source: hold it until the drag ends.
    SOURCES.with(|sources| sources.borrow_mut().push(source));
}

fn text_icon(workspace: &NSWorkspace) -> Retained<NSImage> {
    // SAFETY: a plain file type name.
    #[allow(deprecated)]
    unsafe {
        msg_send![workspace, iconForFileType: &*NSString::from_str("txt")]
    }
}

thread_local! {
    /// Sources of the drags in progress (main thread).
    static SOURCES: RefCell<Vec<Retained<DragSource>>> = const { RefCell::new(Vec::new()) };
}

/// State of one drag's source.
struct SourceIvars {
    done: RefCell<Option<DragDone>>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and the class
    // implements no `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "IstmoDndDragSource"]
    #[ivars = SourceIvars]
    struct DragSource;

    unsafe impl NSObjectProtocol for DragSource {}

    unsafe impl NSDraggingSource for DragSource {
        #[unsafe(method(draggingSession:sourceOperationMaskForDraggingContext:))]
        fn operation_mask(
            &self,
            _session: &NSDraggingSession,
            _context: NSDraggingContext,
        ) -> NSDragOperation {
            NSDragOperation::Copy
        }

        #[unsafe(method(draggingSession:endedAtPoint:operation:))]
        fn ended(&self, _session: &NSDraggingSession, _point: NSPoint, operation: NSDragOperation) {
            let result = if operation == NSDragOperation::None {
                DragResult::Cancelled
            } else {
                DragResult::Dropped
            };
            if let Some(done) = self.ivars().done.borrow_mut().take() {
                done.finish(Ok(result));
            }
            let address = std::ptr::from_ref(self);
            SOURCES.with(|sources| {
                sources
                    .borrow_mut()
                    .retain(|source| !std::ptr::eq(std::ptr::from_ref(&**source), address));
            });
        }
    }
);

impl DragSource {
    fn new(mtm: MainThreadMarker, done: DragDone) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(SourceIvars {
            done: RefCell::new(Some(done)),
        });
        // SAFETY: NSObject's `init` on a freshly allocated object.
        unsafe { msg_send![super(this), init] }
    }
}
