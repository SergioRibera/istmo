# istmo-dnd

Drag and drop between your app and other apps, for [istmo](../../README.md):
files dragged in, and text, links and files dragged out.

For drops, this plugin listens on your app's windows, copies every file into a directory your app owns,
and hands the batch to Rust as one `DroppedFiles`: the files in drag
order, the ones left out and why, and where on the window the drop
landed. While a drag hovers, a `DragState` tells you so you can show it.

| Platform | Drops in | Drags out | Notes |
|---|---|---|---|
| Android 7+ | yes | yes | between apps' windows (split screen, freeform, ChromeOS); any `Activity`, `NativeActivity` included |
| iOS / iPadOS 14+ | yes | yes | every window of every scene; drags out start on the system's long press |
| Linux Wayland | yes | yes | for windows registered with `istmo-window`; Flatpak through the document portal |
| X11 | through your UI toolkit | yes | XDND |
| macOS, Windows | through your UI toolkit | yes | forward drops with `DndPublisher::publish_paths` |

## Use it

```toml
# Cargo.toml
[dependencies]
istmo-dnd = "0.1.1-alpha.7"
```

```rust
istmo::runtime!(plugins: [istmo_dnd::DragAndDropClient]);
```

The Android and iOS backends register themselves through the generated
plugin registry. Nothing listens until you ask:

```rust
use istmo_dnd::{DropInbox, DropOptions, DragAndDropClient};

// Take PDFs and any text file, up to 50 MB each.
DragAndDropClient::acquire()?
    .listen(DropOptions::accepting(["application/pdf", "text/*"]).max_bytes(50 << 20))
    .await?;

let drops = DropInbox::acquire()?.stream();
while let Ok(dropped) = drops.recv_async().await {
    for file in &dropped.files {
        open(&file.path, &file.display_name);
    }
    // Always say what happened: to the other app the drop already
    // looked successful, even if every file was left out.
    announce(dropped.files.len(), &dropped.skipped);
    // The copies are yours: delete them once read.
    dropped.cleanup()?;
}
```

Show the drag while it happens, from a second task:

```rust
use istmo_dnd::{DragState, DropInbox};

let changes = DropInbox::acquire()?.drag_changes();
while let Ok(state) = changes.recv_async().await {
    match state {
        DragState::Over(over) => show_drop_target(over.position),
        DragState::Receiving(count) => show_receiving(count),
        DragState::Idle => hide_drop_target(),
    }
}
```

`listen` fails with `IstmoError::PluginError`; `DndError::try_from_istmo`
(or `DndError::from`) gives the `DndError` back.

- **Refusing what you don't take.** A drag carrying none of the accepted
  MIME types is not taken. Android shows no "can't drop" cue of its own,
  so `DragState` is how the person learns the window takes files. Files
  of other types dragged along with accepted ones come back as
  `SkipReason::WrongType`. `DropOptions::any()` takes every file; text,
  links and folders are not files (accepting `text/*` takes dragged text
  as `.txt` files on iOS).
- **Too large, unreadable, not a file.** These come back in
  `DroppedFiles::skipped` with a `SkipReason`, so you can say so instead
  of dropping them silently. `max_bytes(0)` keeps the 512 MiB default.
- **Large files.** Nothing is published until every file is copied.
  `DragState::Receiving` is your cue to show the drop was received.
- **Position.** `DroppedFiles::position` is in logical pixels (Android dp,
  iOS points) from the top-left corner of the window it landed on. An
  Android app not drawn edge to edge has its content below the status
  bar: subtract that inset before hit-testing. Drops made with VoiceOver
  or a keyboard have no meaningful point; fall back to the focused view.
- **Only other apps' content.** Android reads the `content://` URIs other
  apps provide, never `file://` ones or this app's own providers.
- **Cleanup.** Files are copied to `<cache>/istmo-dnd/incoming/<uuid>/`.
  `cleanup` deletes only copies inside that directory and leaves every
  other path alone without an error, so it is safe on drops you forwarded
  from desktop with the person's own files. Copies older than a day are
  removed when the backend starts.
- **Cold start.** Drops that land before Rust subscribes wait in the
  queue (up to 8; past that the oldest goes). Only the first `stream()`
  subscriber gets the kept ones.
- **`stop()`** removes the listener: drags over the window are refused
  again. A drop still copying when it is called is still published.
- **Activities.** On Android the drag's read permission dies with its
  activity: files still copying when it is destroyed come back
  unreadable.

## Dragging out

When a press on something people can drag turns into a drag (it moved
past a few pixels), start it. The call returns when the drag ends:

```rust
use istmo_dnd::{DragAndDropClient, DragFile, DragItem, DragRequest, DragResult, drag_out_dir};

// Files go out as files: write them first, preferably under
// `drag_out_dir()` (Android exposes files from there as they are).
let path = drag_out_dir()?.join("note.md");
std::fs::write(&path, markdown)?;
let request = DragRequest::new([
    DragItem::File(DragFile { path: path.display().to_string(), mime_type: Some("text/markdown".into()) }),
    DragItem::Text(markdown.clone()),
]);
match DragAndDropClient::acquire()?.start_drag(request).await? {
    DragResult::Dropped => { /* another app took it */ }
    DragResult::Cancelled => {}
    _ => {}
}
```

- **Several forms.** A drag can carry a file and its text: each target
  takes what it reads (a file manager the file, a text field the text).
- **iOS** starts drags itself, on its own long press (or a pointer drag
  on iPad): there call `start_drag` when the press lands
  (`DRAG_STARTS_ON_LONG_PRESS`); the drag happens if the press becomes
  the system's lift within a couple of seconds, else it returns
  `Cancelled`.
- **No press, no drag.** Called after the button or finger is up it fails
  with `DndError::NotPressed`.
- **Your own window.** Letting go over the app itself takes nothing on
  Android, iOS and Wayland. On X11, macOS and Windows the toolkit sees a
  drop: skip toolkit drops while `DndPublisher::dragging_out()` (it
  stays true a second after the drag ends, for drops delivered late).
- **Preview.** `DragRequest::preview` (a PNG and its size) shows under
  the pointer; without it each platform shows its own (a file icon, a
  label).
- **Platform notes.** On Windows the drag is the shell's OLE drag: the
  window does not repaint while it lasts. On X11 the drag runs without
  grabbing the pointer (the toolkit owns it), so Escape does not cancel
  it: letting go outside any target does.

## Desktop

Install the desktop side once, on desktop only (on mobile it would
shadow the native backend), and let it follow your windows:

```rust
#[cfg(not(any(target_os = "android", target_os = "ios")))]
let drops = {
    let drops = istmo_dnd::register_desktop(&runtime);
    drops.follow_registry(istmo_window::WindowRegistry::global())?;
    drops
};
```

Without it nothing answers `listen` on desktop and the call waits
forever.

- **Wayland.** winit delivers no drops there. The publisher opens a
  second connection on the window's `wl_display` (the one registered
  with `istmo-window`) and binds a `wl_data_device` of its own, like
  `istmo-pen` does for tablets. You get the full `DragState` (with
  `window_id` and the position in logical pixels; `count` is 0, Wayland
  does not say before the drop) and `DroppedFiles` with the paths.
- **Flatpak.** Inside the sandbox host paths cannot be opened. When the
  source offers a portal file transfer (GTK, Qt, Firefox, file managers)
  the files come through the document portal instead, as paths under
  `/run/user/<uid>/doc/`. Nothing to declare: the document portal is
  available to every Flatpak app.
- **X11, Windows, macOS.** The UI toolkit gets the drop (winit
  `WindowEvent::DroppedFile`, Freya `on_file_drop`). Forward it for one
  receiving path; the options `listen` set are applied the same way:

  ```rust
  drops.publish_paths(paths, None, Some(window_id))?;
  // hover, when the toolkit reports it (winit `HoveredFile`)
  drops.publish_state(DragState::Over(DragOver::default()));
  ```

Desktop drops are not copied: the paths are the person's own files,
and `cleanup` leaves them alone. Types are told from the extension;
a file whose type cannot be told only passes `*/*` or
`application/octet-stream`. Folders come through only with
`DropOptions::with_folders()`, as entries of type `inode/directory`.

## Native backends

- Android: `native/android/DragAndDropBackendImpl.kt`. A drag listener on the
  activity's decor view; it takes the drag's read permission on drop,
  copies on a worker thread and releases it. It outlives activity
  recreation (`IstmoActivityBound`).
- iOS: `native/ios/DragAndDropBackendImpl.swift`. A `UIDropInteraction` on
  every window of every scene, sheets included; each file is copied
  inside its load callback, into its own folder. A `UIDragInteraction` on
  every window too, for drags out.
- Android drags out with `View.startDragAndDrop`; files go through the
  plugin's `IstmoDndFileProvider` (only `cache/istmo-dnd/outgoing/`).

Both are written against the code `istmo-build` generates for the
`DragAndDrop` contract, pinned in `tests/golden` (`cargo test -p istmo-dnd
--test codegen_golden`).

`DndError`, `SkipReason` and `DragState` are sent by position: new
variants only ever go at the end.
