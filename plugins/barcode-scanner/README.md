# istmo-barcode-scanner

Barcode and QR scanning from Rust, for the
[istmo](https://github.com/SergioRibera/istmo) framework. One async
call opens the platform's full-screen camera scanner and resolves with
the first code read, or `Ok(None)` when the user backs out.

| Platform | Backend | Minimum |
| --- | --- | --- |
| Android | Google code scanner (`play-services-code-scanner`); no `CAMERA` permission needed | API 21 |
| iOS | `AVCaptureSession` + `AVCaptureMetadataOutput`; needs `NSCameraUsageDescription` | 14.0 |
| Desktop, web | `DesktopBarcodeScanner`, always `ScanError::Unavailable`; draw your own scanner with `BarcodeReader` | — |

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

## App-drawn scanner

The `reader-*` features add `BarcodeReader`, a pure-Rust decoder (`rxing`)
for the frames of an `istmo-camera` session. The app draws the preview
and its overlay; each `DetectedCode` carries its corners in frame
pixels. It works wherever the camera does, mobile included.

```toml
istmo-barcode-scanner = { version = "0.1.1-alpha.5", features = ["reader-qr"] }
```

No symbology is compiled in by default; enable only the ones the app
reads, so the binary carries only those decoders:

| Feature | Symbologies |
| ------- | ----------- |
| `reader-qr` | QR |
| `reader-1d` | EAN-13, EAN-8, UPC-A, UPC-E, Code 128, Code 39, Code 93, ITF, Codabar |
| `reader-datamatrix` | Data Matrix |
| `reader-pdf417` | PDF417 |
| `reader-aztec` | Aztec |

`BarcodeReader::supports(format)` tells whether a symbology is compiled
in; asking `with_formats` for one that is not makes `read` fail with
`ReadError::FormatNotCompiled`.

```rust,ignore
use istmo_barcode_scanner::{BarcodeFormat, BarcodeReader};

let reader = BarcodeReader::default().with_formats([BarcodeFormat::Qr]);
for code in reader.read(&frame)? {
    draw_outline(&code.corners, frame.rotation_degrees);
    println!("{}", code.code.text);
}
```

`read` takes NV21, YUV 4:2:0, RGBA or BGRA frames and blocks for one
decode pass: call it off the UI thread and skip frames while a pass
runs.

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
