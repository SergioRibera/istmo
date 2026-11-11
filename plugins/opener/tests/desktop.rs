use istmo_opener::{DesktopOpener, OpenError, Opener};

#[test]
fn empty_url_is_invalid() {
    let result = pollster::block_on(DesktopOpener.open(String::new()));
    assert_eq!(result, Err(OpenError::Invalid));
}

#[test]
fn plugin_error_payload_decodes_to_typed_error() {
    let bytes = istmo_core::codec::encode(&OpenError::NoHandler).expect("encode");
    let err = OpenError::from(istmo_core::IstmoError::PluginError { bytes });
    assert_eq!(err, OpenError::NoHandler);
}
