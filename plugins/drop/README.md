# istmo-drop

Files dragged into your app from other apps, for [istmo](../../README.md).

People drag files between apps on Android (split screen, freeform
windows, ChromeOS) and on iPad and iPhone. This plugin listens for drops
on your app's windows, copies every file into a directory your app owns,
and hands the batch to Rust as one `DroppedFiles`: the files in drag
order, the ones left out and why, and where on the window the drop
landed. While a drag hovers, a `DragState` tells you so you can show it.

| Platform | Drops | Notes |
|---|---|---|
| Android 7+ | yes | from other apps' windows, any `Activity` (`NativeActivity` included) |
| iOS / iPadOS 14+ | yes | every window of every scene; iPhone gets drops from other apps since iOS 15 |
| Desktop | through your UI toolkit | forward them with `DropInbox::publish` for one code path |

## Use it

```toml
# Cargo.toml
[dependencies]
istmo-drop = "0.1.1-alpha.6"
```

```rust
istmo::runtime!(plugins: [istmo_drop::FileDropClient]);
```

The Android and iOS backends register themselves through the generated
plugin registry. Nothing listens until you ask:

```rust
use istmo_drop::{DropInbox, DropOptions, FileDropClient};

// Take PDFs and any text file, up to 50 MB each.
FileDropClient::acquire()?
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
use istmo_drop::{DragState, DropInbox};

let changes = DropInbox::acquire()?.drag_changes();
while let Ok(state) = changes.recv_async().await {
    match state {
        DragState::Over(over) => show_drop_target(over.position),
        DragState::Receiving(count) => show_receiving(count),
        DragState::Idle => hide_drop_target(),
    }
}
```

`listen` fails with `IstmoError::PluginError`; `DropError::try_from_istmo`
(or `DropError::from`) gives the `DropError` back.

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
- **Cleanup.** Files are copied to `<cache>/istmo-drop/incoming/<uuid>/`.
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

## Desktop

Desktop windows receive drops from your UI toolkit (winit
`WindowEvent::DroppedFile`, Freya `on_file_drop`, ...). Nothing answers
`listen` there unless you register the desktop host, and the call would
wait forever. Register it on desktop only: on mobile it would shadow the
native backend.

```rust
#[cfg(not(any(target_os = "android", target_os = "ios")))]
istmo_drop::register_desktop(&runtime);
```

For a single receiving path, forward the toolkit's drops. `position` is
`None` unless your toolkit gives one (winit does not); convert physical
pixels to logical ones when it does.

```rust
use istmo_drop::{DropInbox, DroppedFile, DroppedFiles};

// in your toolkit's drop handler
DropInbox::acquire()?.publish(DroppedFiles {
    files: paths
        .iter()
        .map(|path| DroppedFile {
            path: path.display().to_string(),
            display_name: path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            mime_type: None,
            size: std::fs::metadata(path).map_or(0, |meta| meta.len()),
        })
        .collect(),
    ..DroppedFiles::default()
})?;
```

## Native backends

- Android: `native/android/FileDropBackendImpl.kt`. A drag listener on the
  activity's decor view; it takes the drag's read permission on drop,
  copies on a worker thread and releases it. It outlives activity
  recreation (`IstmoActivityBound`).
- iOS: `native/ios/FileDropBackendImpl.swift`. A `UIDropInteraction` on
  every window of every scene, sheets included. Each file is copied
  inside its load callback, into its own folder.

Both are written against the code `istmo-build` generates for the
`FileDrop` contract, pinned in `tests/golden` (`cargo test -p istmo-drop
--test codegen_golden`).

`DropError`, `SkipReason` and `DragState` are sent by position: new
variants only ever go at the end.
