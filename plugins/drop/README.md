# istmo-drop

Files dragged into your app from other apps, for [istmo](../../README.md).

People drag files between apps on Android (split screen, freeform
windows, ChromeOS) and on iPadOS. This plugin listens for drops on your
app's window, copies every file into a directory your app owns, and hands
the batch to Rust as one `DroppedFiles`: the files in drag order, the ones
left out and why, and where on the window the drop landed.

| Platform | Drops | Notes |
|---|---|---|
| Android 7+ | yes | from other apps' windows (`DRAG_FLAG_GLOBAL`) |
| iPadOS 14+ | yes | iPhone only within the app |
| Desktop | through your UI toolkit | forward them with `DropInbox::publish` for one code path |

## Use it

```toml
# Cargo.toml
[dependencies]
istmo-drop = "0.1"
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
    for skipped in &dropped.skipped {
        tell_the_person(&skipped.display_name, skipped.reason);
    }
    // The copies are yours: delete them once read.
    DropInbox::cleanup(&dropped)?;
}
```

- **Refusing what you don't take.** A drag carrying none of the accepted
  MIME types is refused while it hovers, so the system shows it cannot be
  dropped. `DropOptions::any()` takes every file (plain text alone is
  refused: there is no file to copy).
- **Too large, unreadable, not a file.** These come back in
  `DroppedFiles::skipped` with a `SkipReason`, so you can say so instead
  of dropping them silently.
- **Position.** `DroppedFiles::position` is in logical pixels (Android dp,
  iOS points) from the window's top-left corner, the space your UI lays
  out in: hit-test it against your own views to know what the file was
  dropped on.
- **Cleanup.** Files are copied to `<cache>/istmo-drop/incoming/<uuid>/`.
  `DropInbox::cleanup` deletes only copies inside that directory, so it is
  safe to call on drops you forwarded from desktop with paths of the
  person's own files (those are left alone). Copies older than a day are
  removed when the app starts.
- **Cold start.** Drops that land before Rust subscribes wait in the
  queue (up to 8).
- **`stop()`** removes the listener: drags over the window are refused
  again.

## Desktop

Desktop windows receive drops from your UI toolkit (winit
`WindowEvent::DroppedFile`, Freya `on_file_drop`, ...). Register
`DesktopFileDrop` so the same `listen` call succeeds everywhere, and, if
you want a single receiving path, forward the toolkit's drops:

```rust
runtime.register_host(istmo_drop::FileDropHost::new(istmo_drop::DesktopFileDrop));

// in your toolkit's drop handler
DropInbox::acquire()?.publish(DroppedFiles {
    files: paths.iter().map(|path| DroppedFile { /* ... */ }).collect(),
    ..DroppedFiles::default()
})?;
```

## Native backends

- Android: `native/android/FileDropBackendImpl.kt`. A drag listener on the
  activity's decor view; it takes the drag's read permission on drop,
  copies on a worker thread and releases it. It outlives activity
  recreation (`IstmoActivityBound`).
- iOS: `native/ios/FileDropBackendImpl.swift`. A `UIDropInteraction` on
  the key window's root view, moved to a new window when a scene
  activates. Each file is copied inside its load callback.

Both are written against the code `istmo-build` generates for the
`FileDrop` contract, pinned in `tests/golden` (`cargo test -p istmo-drop
--test codegen_golden`).
