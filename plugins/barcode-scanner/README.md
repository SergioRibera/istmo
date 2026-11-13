# istmo-barcode-scanner

Barcode and QR scanning from Rust, for the
[istmo](https://github.com/SergioRibera/istmo) framework. One async
call opens the platform's full-screen camera scanner and resolves with
the first code read, or `Ok(None)` when the user backs out.

| Platform | Backend | Minimum |
| --- | --- | --- |
| Android | Google code scanner (`play-services-code-scanner`); no `CAMERA` permission needed | API 21 |
| iOS | `AVCaptureSession` + `AVCaptureMetadataOutput`; needs `NSCameraUsageDescription` | 13.0 |
| Desktop | `DesktopBarcodeScanner`, always `ScanError::Unavailable` | — |

The minimums come from the plugin's `[min_versions]` and are checked by
the app's `build.rs`.

## Install

```toml
[dependencies]
istmo-barcode-scanner = "0.1.1-alpha.5"
```

## Usage

```rust,ignore
use istmo_barcode_scanner::{BarcodeScannerClient, ScanConfig, ScanError};

let scanner = BarcodeScannerClient::from_runtime(&runtime)?;
if let Some(code) = scanner.scan(ScanConfig::qr("Scan the pairing code")).await.map_err(ScanError::from)? {
    println!("{:?}: {}", code.format, code.text);
}
```

`ScannedCode::text` is empty for payloads that are not text; Android
then still hands over the bytes in `ScannedCode::raw`, while iOS skips
such codes and keeps scanning. `BarcodeFormat` is `#[non_exhaustive]`
and reports symbologies without a variant as `BarcodeFormat::Other`.

## App setup

Reference backends live in `native/android` and `native/ios`; the
generated `IstmoPluginRegistry` registers them, handing the Android
backend the host activity. `play-services-code-scanner` comes from the
plugin's `istmo.toml`. On iOS the plugin adds a default
`NSCameraUsageDescription`; reword it with `[app.info_plist]` in the
app's `istmo.toml`. Desktop apps that want the typed
`ScanError::Unavailable` register
`BarcodeScannerHost::new(DesktopBarcodeScanner)`.

Full guide: <https://sergioribera.rs/istmo/plugins/barcode-scanner/>
