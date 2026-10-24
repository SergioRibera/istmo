use istmo_barcode_scanner::{BarcodeScanner, DesktopBarcodeScanner, ScanConfig, ScanError};

#[test]
fn desktop_reports_unavailable() {
    let result = pollster::block_on(DesktopBarcodeScanner.scan(ScanConfig::qr("scan")));
    assert_eq!(result, Err(ScanError::Unavailable));
}

#[test]
fn qr_config_limits_formats() {
    let config = ScanConfig::qr("hola");
    assert_eq!(config.prompt.as_deref(), Some("hola"));
    assert_eq!(
        config.formats,
        vec![istmo_barcode_scanner::BarcodeFormat::Qr]
    );
}

#[test]
fn plugin_error_payload_decodes_to_typed_error() {
    let bytes = bincode::encode_to_vec(ScanError::PermissionDenied, bincode::config::standard())
        .expect("encode");
    let err = ScanError::from(istmo_core::IstmoError::PluginError { bytes });
    assert_eq!(err, ScanError::PermissionDenied);
}
