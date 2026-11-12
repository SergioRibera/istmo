# istmo-barcode-scanner

Barcode and QR scanning from Rust, for the
[istmo](https://github.com/SergioRibera/istmo) framework. One async
call opens the platform's full-screen camera scanner and resolves with
the first code read, or `Ok(None)` when the user backs out.

| Platform | Backend |
| --- | --- |
| Android | Google code scanner (`play-services-code-scanner`); no `CAMERA` permission needed |
| iOS | `AVCaptureSession` + `AVCaptureMetadataOutput`; needs `NSCameraUsageDescription` |
| Desktop | `DesktopBarcodeScanner`, always `ScanError::Unavailable` |

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

Reference backends live in `native/android` and `native/ios`; the
generated `IstmoPluginRegistry` registers them.
