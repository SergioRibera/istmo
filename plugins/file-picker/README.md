# istmo-file-picker

Native file pickers from Rust, for the
[istmo](https://github.com/SergioRibera/istmo) framework.

| Platform | Backend |
| --- | --- |
| Android | Storage Access Framework (`native/android/FilePickerBackendImpl.kt`) |
| iOS | `UIDocumentPickerViewController` (`native/ios/FilePickerBackendImpl.swift`) |
| Desktop | `DesktopFilePicker` over `rfd`: `IFileOpenDialog`, `NSOpenPanel`, XDG portal |

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

On Android the backend needs the host `ComponentActivity` and is built
in `onCreate` by the generated `IstmoPluginRegistry`. A picker still
open when that activity is destroyed fails with
`FilePickerError::Backend`; handles picked earlier keep working.
