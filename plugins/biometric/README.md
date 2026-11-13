# istmo-biometric

Biometric authentication from Rust, for the
[istmo](https://github.com/SergioRibera/istmo) framework: check what
the device can verify, show the system prompt, and keep small secrets
encrypted under key material the OS only releases after a successful
verification.

| Platform | Backend | Minimum |
| --- | --- | --- |
| Android | `androidx.biometric` `BiometricPrompt` + Keystore (`native/android`) | API 23 |
| iOS | `LocalAuthentication` + Keychain (`native/ios`) | — |
| macOS | `DesktopBiometric`: `LocalAuthentication` (Touch ID) + data-protection keychain | — |
| Windows | `DesktopBiometric`: Windows Hello (`UserConsentVerifier`) | — |
| Linux | `DesktopBiometric`: `fprintd` over the system D-Bus | — |

The Android minimum comes from the plugin's `[min_versions]` and is
checked by the app's `build.rs`.

## Install

```toml
[dependencies]
istmo-biometric = "0.1.1-alpha.5"
```

## Usage

```rust,ignore
use istmo_biometric::{AuthPolicy, AuthPrompt, BiometricClient, BiometricError};

let biometric = BiometricClient::from_runtime(&runtime)?;
if biometric.availability(AuthPolicy::BiometricStrong).await?.is_available() {
    let prompt = AuthPrompt::new("Unlock", "Confirm it's you to open your notes");
    match biometric.authenticate(prompt.clone()).await {
        Ok(method) => println!("verified with {method:?}"),
        Err(err) => match BiometricError::try_from(err) {
            Ok(BiometricError::UserCancelled) => {}
            Ok(domain) => eprintln!("{domain}"),
            Err(transport) => eprintln!("runtime error: {transport}"),
        },
    }

    biometric
        .store_secret("api.token".into(), b"secret".to_vec(), prompt.clone())
        .await?;
    let token = biometric.read_secret("api.token".into(), prompt).await?;
}
```

A successful `authenticate` is only a UI gate; protect real secrets
with `store_secret` / `read_secret`. Dropping a pending future cancels
the call and dismisses the prompt where the platform allows it.
`enrollment_state` returns a token that changes when fingers or faces
are added or removed.

## App setup

* **Android**: the activity must be a `FragmentActivity` —
  `IstmoGameActivity`, or your own `AppCompatActivity` calling
  `IstmoHost.onCreate(this)`. The generated `IstmoPluginRegistry` hands
  it to `BiometricBackendImpl`. `androidx.biometric` (and with it the
  `USE_BIOMETRIC` permission) comes from the plugin's `istmo.toml`.
* **iOS**: the registry registers `BiometricBackendImpl()`. The plugin
  adds a default `NSFaceIDUsageDescription`; reword it with
  `[app.info_plist]` in the app's `istmo.toml`.
* **Desktop**: register the Rust backend yourself:
  `runtime.register_host(BiometricHost::new(DesktopBiometric::default()))`.
  On macOS the secret vault needs an app signed with a Team ID and the
  `keychain-access-groups` entitlement; elsewhere vault calls fail with
  `BiometricError::UnsupportedOperation`. Linux has no vault unless the
  app opts into `DesktopBiometric::with_ui_gated_vault`.

Full guide: <https://sergioribera.rs/istmo/plugins/biometric/>
