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
//! * Desktop — [`DesktopBarcodeScanner`] reports
//!   [`ScanError::Unavailable`]; desktop apps offer paste instead.
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

#[cfg(feature = "codegen")]
pub mod codegen;

use istmo_macros::{message, plugin};

/// Wire identifier for the barcode scanner plugin.
pub const BARCODE_SCANNER_PLUGIN_ID: &str = "istmo.barcode_scanner";

/// A symbology the scanner may look for.
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BarcodeFormat {
    Qr,
    Aztec,
    DataMatrix,
    Pdf417,
    Code128,
    Ean13,
}

/// What to scan for.
#[message(bincode = "::bincode", crate = "::istmo_core")]
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
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ScannedCode {
    /// Decoded text (UTF-8).
    pub text: String,
    pub format: BarcodeFormat,
}

/// Why a scan could not run.
#[message(bincode = "::bincode", crate = "::istmo_core")]
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
                bincode::decode_from_slice::<Self, _>(&bytes, bincode::config::standard())
                    .map_or_else(
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

/// Desktop backend: there is no camera scanner, every scan reports
/// [`ScanError::Unavailable`].
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
