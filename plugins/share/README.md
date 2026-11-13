# istmo-share

Share sheet from Rust, for the
[istmo](https://github.com/SergioRibera/istmo) framework: send any mix
of text, a link, a subject and files through the platform share UI, and
receive what other apps share into yours as a stream of
`IncomingShare` values.

| Platform | Send | Receive (opt-in) | Minimum |
| --- | --- | --- | --- |
| Android | `ACTION_SEND(_MULTIPLE)` chooser + `FileProvider` (`native/android`) | `ACTION_SEND` intent filters on an app-declared alias | API 22 |
| iOS | `UIActivityViewController` (`native/ios`) | Share Extension + App Group | 13.0 |
| macOS | `DesktopShare`: `NSSharingServicePicker` | Share Extension + App Group | 10.15 |
| Windows | `DesktopShare`: `DataTransferManager` | Share Target (package identity, 10.0.19041+) | 10.0.16299 |
| Linux | not available (`ShareError::Unsupported`) | app-driven (`ShareInbox::publish`) | — |

The minimums come from the plugin's `[min_versions]` and are checked by
the app's `build.rs`.

## Install

```toml
[dependencies]
istmo-share = "0.1.1-alpha.5"
# `ShareFile::from_picked` for files from istmo-file-picker:
# istmo-share = { version = "0.1.1-alpha.5", features = ["file-picker"] }
```

## Usage

```rust,ignore
use istmo_share::{ShareClient, ShareError, ShareFile, ShareInbox, ShareOutcome, ShareRequest};

let share = ShareClient::from_runtime(&runtime)?;
let request = ShareRequest::text("Trip photos")
    .with_url("https://example.com/trip")
    .with_file(ShareFile::bytes("itinerary.pdf", pdf_bytes));
match share.share(request).await.map_err(ShareError::from)? {
    ShareOutcome::Shared(target) => println!("shared via {target:?}"),
    other => println!("{other:?}"),
}

// Receiving: shares that cold-started the app are buffered.
let incoming = ShareInbox::from_runtime(&runtime)?.stream();
while let Ok(received) = incoming.recv_async().await {
    import(&received)?;
    ShareInbox::cleanup(&received)?;
}
```

Each platform reports a different amount through `ShareOutcome`;
`capabilities()` says what the current install supports (sending,
receiving, direct-share targets, dismissing on cancel).

## App setup

* **Android**: the generated `IstmoPluginRegistry` hands
  `ShareBackendImpl` the host `ComponentActivity` (`IstmoGameActivity`,
  or your own activity calling `IstmoHost.onCreate(this)`). The plugin
  manifest brings the `FileProvider`; receiving needs an
  `activity-alias` named `dev.istmo.plugins.share.ShareTarget` pointing
  at `dev.istmo.plugins.share.ShareReceiverActivity` with your intent
  filters.
* **iOS**: the registry registers `ShareBackendImpl()`. The plugin adds
  a default `NSPhotoLibraryAddUsageDescription` (the "Save Image" action
  needs it); reword it with `[app.info_plist]` and localise it with the
  `InfoPlist.strings` in `templates/ios-localization`. Receiving needs
  the Share Extension in `templates/apple-share-extension`.
* **Desktop**: register each window in the `istmo-window` registry and
  host the Rust backend with
  `ShareHost::new(DesktopShare::with_global_registry()?)`. macOS drains
  the extension's App Group with `desktop::drain_app_group_inbox` /
  `desktop::observe_app_group_handoffs`; Windows share-target launches
  call `desktop::take_share_target_activation`.

Full guide: <https://sergioribera.rs/istmo/plugins/share/>
