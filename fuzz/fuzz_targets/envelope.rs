//! Bytes from the `:remote` bridge or a native transport: decoding must
//! fail cleanly, never abort or allocate past what the input justifies.
#![no_main]

use istmo_core::Envelope;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(envelope) = Envelope::from_wire_bytes(data) else {
        return;
    };
    let bytes = envelope.to_wire_bytes().expect("a decoded envelope encodes");
    let again = Envelope::from_wire_bytes(&bytes).expect("an encoded envelope decodes");
    assert_eq!(again, envelope);
});
