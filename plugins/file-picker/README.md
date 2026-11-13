# istmo-file-picker

Native file pickers from Rust, for the
[istmo](https://github.com/SergioRibera/istmo) framework.

| Platform | Backend |
| --- | --- |
| Android | Storage Access Framework (`native/android/FilePickerBackendImpl.kt`) |
| iOS | `UIDocumentPickerViewController` (`native/ios/FilePickerBackendImpl.swift`) |
| Desktop | `DesktopFilePicker` over `rfd`: `IFileOpenDialog`, `NSOpenPanel`, XDG portal |

The plugin declares no `[min_versions]`.

## Install

```toml
[dependencies]
istmo-file-picker = "0.1.1-alpha.5"
```

## Usage

```rust,ignore
use std::io::Read;
use std::sync::Arc;
use istmo_file_picker::{FilePickerClient, PickConfig};

let picker = FilePickerClient::from_runtime(&runtime)?;
if let Some(file) = picker.pick_file_owned(PickConfig::default()).await? {
    let file = Arc::new(file);
    let mut text = String::new();
    file.open_reader(&picker).await?.read_to_string(&mut text)?;
}
```

Picked locations travel as `NativeHandle<FileRef>`s: the native side
keeps SAF grants / security scope alive until the handle drops. Reads
and writes go through file descriptors adopted into `std::fs::File`
(`PickedFileReader` / `PickedFileWriter`), so the plugin must run in
the app process (`default_deployment = "local"`).

Desktop apps register `FilePickerHost::new(picker.clone())` and then
call `picker.install_release_hook(&runtime)` before the first pick.

On iOS `IstmoPluginRegistry.registerAll()` registers the backend. To
register it by hand, wrap it in the generated dispatcher:

```swift
IstmoRuntime.shared.registerHandler(
    FilePickerDispatcher.PLUGIN_ID,
    FilePickerDispatcher(backend: FilePickerBackendImpl(), codecs: FilePickerCodecsImpl())
)
```

On Android the backend needs the host `ComponentActivity` and is built
in `onCreate` by the generated `IstmoPluginRegistry`. A picker still
open when that activity is destroyed fails with
`FilePickerError::Backend`; handles picked earlier keep working.

Android grants are persisted while a handle lives and released when the
last handle of that document drops (or at the next app start, for
handles a killed process left behind). Picking a held document again
with write access upgrades its grant. Only access modes the backend
added itself are released — a grant the app already persisted for that
document is left alone. Android keeps one grant per document and app,
so a grant the app persists for the same document *while* a handle is
live is shared and goes away with it.

Full guide: <https://sergioribera.rs/istmo/plugins/file-picker/>
