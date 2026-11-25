//! The drop queue, cleanup of the copies, and the desktop backend.

use std::sync::Arc;

use istmo_core::Runtime;
use istmo_dnd::{
    DROP_DIR, DesktopDragAndDrop, DragAndDropClient, DragAndDropHost, DropInbox, DropOptions,
    DropPoint, DroppedFile, DroppedFiles, SkipReason, SkippedFile,
};

fn runtime() -> Arc<Runtime> {
    Runtime::mock()
        .expects::<DragAndDropClient>()
        .host(DragAndDropHost::new(DesktopDragAndDrop::default()))
        .finish()
        .runtime
}

fn copy_at(root: &std::path::Path, name: &str) -> std::path::PathBuf {
    let folder = root.join(DROP_DIR).join("one-drop");
    std::fs::create_dir_all(&folder).expect("drop folder");
    let path = folder.join(name);
    std::fs::write(&path, b"dropped").expect("write");
    path
}

#[test]
fn a_drop_published_before_anyone_listens_waits_whole() {
    let rt = runtime();
    let inbox = DropInbox::from_runtime(&rt).expect("declared");
    let root = std::env::temp_dir().join(format!("istmo-dnd-wait-{}", std::process::id()));
    let a = copy_at(&root, "a.pdf");
    let b = copy_at(&root, "b.md");
    let file = |path: &std::path::Path, name: &str| DroppedFile {
        path: path.display().to_string(),
        display_name: name.to_owned(),
        mime_type: None,
        size: 7,
    };
    inbox
        .publish(DroppedFiles {
            files: vec![file(&a, "a.pdf"), file(&b, "b.md")],
            skipped: vec![SkippedFile {
                display_name: "huge.mov".to_owned(),
                reason: SkipReason::TooLarge,
            }],
            position: Some(DropPoint { x: 12.5, y: 40.0 }),
            received_at_ms: None,
            window_id: None,
        })
        .expect("publish before subscribing");

    let dropped = inbox.stream().try_recv().expect("kept").expect("decoded");
    assert_eq!(dropped.files.len(), 2, "one drop, every file");
    assert_eq!(dropped.files[0].display_name, "a.pdf", "in drag order");
    assert_eq!(dropped.skipped[0].reason, SkipReason::TooLarge);
    assert_eq!(dropped.position, Some(DropPoint { x: 12.5, y: 40.0 }));
    assert!(dropped.received_at_ms.is_some(), "publish stamps the time");

    DropInbox::cleanup(&dropped).expect("cleanup");
    assert!(!a.exists() && !b.exists());
    assert!(
        !a.parent().expect("folder").exists(),
        "the empty drop folder goes too"
    );
    DropInbox::cleanup(&dropped).expect("cleanup twice is fine");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn cleanup_never_touches_files_outside_the_drop_dir() {
    let root = std::env::temp_dir().join(format!("istmo-dnd-escape-{}", std::process::id()));
    let inside = copy_at(&root, "x.pdf");
    let mine = root.join("my-own-file.pdf");
    std::fs::write(&mine, b"keep").expect("write");
    let file = |path: std::path::PathBuf| DroppedFile {
        path: path.display().to_string(),
        display_name: "x".to_owned(),
        mime_type: None,
        size: 0,
    };
    let dropped = DroppedFiles {
        files: vec![
            // A desktop drop forwarded with the person's own path.
            file(mine.clone()),
            file(
                inside
                    .parent()
                    .expect("folder")
                    .join("../../../my-own-file.pdf"),
            ),
        ],
        ..DroppedFiles::default()
    };
    // Left alone, and no error: a receive loop using `?` keeps going.
    dropped.cleanup().expect("foreign paths are skipped");
    assert_eq!(std::fs::read(&mine).expect("kept"), b"keep");
    assert!(inside.exists(), "only the files named are deleted");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_inbox_needs_the_plugin_declared() {
    let init = Runtime::mock().finish();
    assert!(DropInbox::from_runtime(&init.runtime).is_err());
}

#[test]
fn listening_on_desktop_succeeds_and_does_nothing() {
    let rt = runtime();
    let client = DragAndDropClient::from_runtime(&rt).expect("declared");
    pollster::block_on(client.listen(DropOptions::accepting(["application/pdf"]).max_bytes(1024)))
        .expect("listen");
    pollster::block_on(client.stop()).expect("stop");
}

#[test]
fn options_builders() {
    let options = DropOptions::accepting(["image/*", "application/pdf"]).max_bytes(10);
    assert_eq!(options.mime_types, ["image/*", "application/pdf"]);
    assert_eq!(options.max_bytes, 10);
    assert_eq!(DropOptions::any(), DropOptions::default());
}

#[test]
fn the_drag_state_starts_idle_and_follows_the_backend() {
    use istmo_dnd::{DRAG_CHANNEL, DragOver, DragState};

    let rt = runtime();
    let inbox = DropInbox::from_runtime(&rt).expect("declared");
    assert_eq!(inbox.drag_state(), DragState::Idle);
    let over = DragState::Over(DragOver {
        count: 2,
        position: Some(DropPoint { x: 1.0, y: 2.0 }),
        window_id: Some(3),
    });
    rt.publish_early_latest(
        DRAG_CHANNEL,
        istmo_core::codec::encode(&over).expect("encode"),
    );
    assert_eq!(inbox.drag_state(), over);
    let changes = inbox.drag_changes();
    assert_eq!(changes.recv().expect("current first"), over);
}

#[test]
fn a_domain_error_comes_back_typed() {
    use istmo_dnd::DndError;

    let bytes = istmo_core::codec::encode(&DndError::Unsupported).expect("encode");
    let err = istmo_core::IstmoError::PluginError { bytes };
    assert!(matches!(
        DndError::try_from_istmo(err),
        Ok(DndError::Unsupported)
    ));
    let transport = DndError::from(istmo_core::IstmoError::ChannelClosed);
    assert!(matches!(transport, DndError::Backend(_)));
}
