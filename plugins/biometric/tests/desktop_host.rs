//! The reference desktop backend hosted in-process. Runs everywhere:
//! without a sensor (or, on Linux, without fprintd) availability simply
//! reports it. The interactive round-trip is `#[ignore]`d — run it by
//! hand on a machine with enrolled biometrics:
//!
//! ```sh
//! cargo test -p istmo-biometric --test desktop_host -- --ignored --nocapture
//! ```

#![cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]

use istmo_biometric::{
    AuthPolicy, AuthPrompt, BiometricClient, BiometricError, BiometricHost, BiometricStatus,
    DesktopBiometric,
};
use istmo_core::Runtime;

fn client() -> (std::sync::Arc<Runtime>, BiometricClient) {
    let init = Runtime::mock()
        .expects::<BiometricClient>()
        .host(BiometricHost::new(DesktopBiometric::default()))
        .finish();
    let client = BiometricClient::from_runtime(&init.runtime).expect("client declared");
    (init.runtime, client)
}

#[test]
fn availability_answers_without_ui() {
    let (_rt, client) = client();
    for policy in [
        AuthPolicy::BiometricStrong,
        AuthPolicy::BiometricWeak,
        AuthPolicy::BiometricOrDeviceCredential,
    ] {
        let availability =
            pollster::block_on(client.availability(policy)).expect("availability never fails");
        if availability.status == BiometricStatus::Unsupported {
            assert!(availability.kinds.is_empty());
        }
    }
}

#[test]
fn empty_reason_is_rejected_before_touching_the_platform() {
    let (_rt, client) = client();
    let err = pollster::block_on(client.authenticate(AuthPrompt::new("Unlock", "  ")))
        .expect_err("empty reason");
    assert!(matches!(
        BiometricError::try_from(err),
        Ok(BiometricError::InvalidPrompt(_))
    ));
}

#[test]
#[ignore = "interactive: needs enrolled biometrics and a human"]
fn interactive_authentication() {
    let (_rt, client) = client();
    let availability =
        pollster::block_on(client.availability(AuthPolicy::BiometricStrong)).expect("availability");
    println!("availability: {availability:?}");
    let prompt = AuthPrompt::new("istmo", "Verify it's you to run the istmo biometric test")
        .policy(AuthPolicy::BiometricOrDeviceCredential);
    let outcome = pollster::block_on(client.authenticate(prompt)).map_err(BiometricError::try_from);
    println!("authenticate: {outcome:?}");
}
