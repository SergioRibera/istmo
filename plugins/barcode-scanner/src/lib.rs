//! Barcode and QR scanner plugin for [`istmo`](https://docs.rs/istmo).
//!
//! Rust code sees one async [`BarcodeScanner::scan`] call. It opens the
//! platform's camera scanner full screen and resolves with the first
//! code read, or `Ok(None)` when the user backs out.
//!
//! * Android — Google code scanner (`play-services-code-scanner`):
//!   Play services owns the camera UI and decoding, so the app declares
//!   no `CAMERA` permission. Devices without Play services report
//!   [`ScanError::Unavailable`].
//! * iOS — an `AVCaptureSession` with `AVCaptureMetadataOutput`
//!   presented over the top view controller. Needs
//!   `NSCameraUsageDescription` (declared in this plugin's `istmo.toml`).
//! * Desktop and web — [`DesktopBarcodeScanner`] reports
//!   [`ScanError::Unavailable`]: there is no platform scanner UI. Use the
//!   frame decoder below.
//!
//! # App-drawn scanner
//!
//! With the `reader` feature, `BarcodeReader` decodes the frames of an
//! `istmo-camera` session in Rust, on every platform the camera runs
//! on (mobile included). The app draws the preview and its own overlay
//! from the corners each detected code carries; see the `reader`
//! module.
//!
//! # Auto-registration
//!
//! The generated `IstmoPluginRegistry` registers both mobile backends.
//! Desktop apps register [`DesktopBarcodeScanner`] themselves:
//!
//! ```rust,ignore
//! runtime.register_host(BarcodeScannerHost::new(DesktopBarcodeScanner));
//! ```

#![doc(html_root_url = "https://docs.rs/istmo-barcode-scanner")]

#[cfg(feature = "reader")]
pub mod reader;
#[cfg(feature = "reader")]
pub use reader::{BarcodeReader, DetectedCode, FramePoint, ReadError};

use istmo_macros::{message, plugin};

/// Wire identifier for the barcode scanner plugin.
pub const BARCODE_SCANNER_PLUGIN_ID: &str = "istmo.barcode_scanner";

/// A symbology the scanner may look for.
///
/// Not every platform reads every format: iOS has no separate UPC-A
/// (it reports those as [`Ean13`](Self::Ean13) with a leading `0`) and
/// reads Codabar only from iOS 15.4. A requested format the platform
/// cannot read is ignored.
///
/// New variants are only ever appended, so existing wire indices stay
/// stable.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum BarcodeFormat {
    /// QR code.
    Qr,
    /// Aztec.
    Aztec,
    /// Data Matrix.
    DataMatrix,
    /// PDF417.
    Pdf417,
    /// Code 128.
    Code128,
    /// EAN-13.
    Ean13,
    /// EAN-8.
    Ean8,
    /// UPC-A.
    UpcA,
    /// UPC-E.
    UpcE,
    /// Code 39.
    Code39,
    /// Code 93.
    Code93,
    /// Interleaved 2 of 5 (ITF, including ITF-14).
    Itf,
    /// Codabar.
    Codabar,
    /// A code the platform read but whose symbology has no variant
    /// here. Never matches anything when requested in
    /// [`ScanConfig::formats`].
    Other,
}

/// What to scan for.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct ScanConfig {
    /// Hint shown over the camera where the backend allows one.
    pub prompt: Option<String>,
    /// Symbologies to accept. Empty accepts every supported one.
    pub formats: Vec<BarcodeFormat>,
}

impl ScanConfig {
    /// QR codes only, with `prompt` over the camera.
    #[must_use]
    pub fn qr(prompt: impl Into<String>) -> Self {
        Self {
            prompt: Some(prompt.into()),
            formats: vec![BarcodeFormat::Qr],
        }
    }
}

/// A code the scanner read.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ScannedCode {
    /// Decoded text (UTF-8). Empty when the payload is not text — read
    /// [`raw`](Self::raw) then.
    pub text: String,
    /// Symbology of the code.
    pub format: BarcodeFormat,
    /// Raw payload bytes, when the platform exposes them. Android
    /// always fills it (binary QR codes included); iOS leaves it `None`
    /// and skips codes it cannot decode as text.
    pub raw: Option<Vec<u8>>,
}

/// Why a scan could not run.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ScanError {
    /// No camera, or no scanner service on this device.
    Unavailable,
    /// The user refused camera access.
    PermissionDenied,
    /// Any other backend failure, with the platform message.
    Backend(String),
}

impl std::fmt::Display for ScanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable => f.write_str("no barcode scanner on this device"),
            Self::PermissionDenied => f.write_str("camera access denied"),
            Self::Backend(msg) => write!(f, "barcode scanner backend error: {msg}"),
        }
    }
}

impl std::error::Error for ScanError {}

/// Recover the typed error from a failed [`BarcodeScannerClient::scan`]:
/// `client.scan(config).await.map_err(ScanError::from)`.
impl From<istmo_core::IstmoError> for ScanError {
    fn from(err: istmo_core::IstmoError) -> Self {
        match err {
            istmo_core::IstmoError::PluginError { bytes } => {
                istmo_core::codec::decode::<Self>(&bytes).map_or_else(
                    |e| Self::Backend(format!("decode plugin error payload: {e}")),
                    |(err, _)| err,
                )
            }
            other => Self::Backend(other.to_string()),
        }
    }
}

/// The plugin trait. Native backends implement it; Rust calls it via the
/// macro-generated [`BarcodeScannerClient`].
#[plugin(name = "istmo.barcode_scanner", crate = "::istmo_core")]
pub trait BarcodeScanner {
    /// Open the camera scanner. `Ok(None)` when the user cancels.
    async fn scan(&self, config: ScanConfig) -> Result<Option<ScannedCode>, ScanError>;
}

/// Desktop backend: there is no platform scanner UI, every scan reports
/// [`ScanError::Unavailable`]. Apps that draw their own scanner decode
/// camera frames with the `reader` feature instead.
#[derive(Debug, Clone, Copy, Default)]
pub struct DesktopBarcodeScanner;

impl BarcodeScanner for DesktopBarcodeScanner {
    fn scan(
        &self,
        _config: ScanConfig,
    ) -> impl std::future::Future<Output = Result<Option<ScannedCode>, ScanError>> + Send + '_ {
        std::future::ready(Err(ScanError::Unavailable))
    }
}
