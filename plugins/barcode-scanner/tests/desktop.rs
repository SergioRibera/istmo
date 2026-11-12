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
fn format_wire_indices_stay_stable() {
    use istmo_barcode_scanner::BarcodeFormat;
    for (format, index) in [
        (BarcodeFormat::Qr, 0u8),
        (BarcodeFormat::Ean13, 5),
        (BarcodeFormat::Ean8, 6),
        (BarcodeFormat::Other, 13),
    ] {
        assert_eq!(istmo_core::codec::encode(&format).expect("encode"), [index]);
    }
}

#[test]
fn plugin_error_payload_decodes_to_typed_error() {
    let bytes = istmo_core::codec::encode(&ScanError::PermissionDenied).expect("encode");
    let err = ScanError::from(istmo_core::IstmoError::PluginError { bytes });
    assert_eq!(err, ScanError::PermissionDenied);
}
