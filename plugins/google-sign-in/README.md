# istmo-google-sign-in

Sign in with Google from Rust, for the
[istmo](https://github.com/SergioRibera/istmo) framework.

| Platform | Backend | Dependency (from `istmo.toml`) |
| --- | --- | --- |
| Android | Credential Manager (`native/android/GoogleSignInBackendImpl.kt`) | `androidx.credentials`, `googleid` |
| iOS | `GIDSignIn` (`native/ios/GoogleSignInBackendImpl.swift`) | `GoogleSignIn-iOS` 7.1+ |

## Usage

```rust,ignore
use istmo_google_sign_in::{SignInClient, SignInConfig, SignInMode};

let config = SignInConfig::builder("1234-abc.apps.googleusercontent.com").build();
let client = SignInClient::acquire_with(config).await?;
let account = client.sign_in_owned(SignInMode::Interactive).await?;
send_to_server(&account.id_token);
```

`server_client_id` is the **Web** OAuth client id from the Google Cloud
Console; it becomes the audience of the returned id token. Verify the
token on your server — the backends do not.

## Platform setup

* **Android**: create an Android OAuth client for the app's package and
  signing certificate SHA-1, in the same Cloud project as the Web
  client. The generated `IstmoPluginRegistry` registers
  `GoogleSignInFactoryImpl` with the host activity.
* **iOS**: set `GIDClientID` (the iOS OAuth client id) in `Info.plist`,
  add the reversed client id as a URL scheme, and forward
  `application(_:open:options:)` to `GIDSignIn.sharedInstance.handle(_:)`.

## Limits of the Android reference backend

Credential Manager authenticates but does not grant OAuth scopes, has no
token refresh (`refresh` re-runs the silent flow) and no revoke call
(`revoke` clears local state; revoke the grant from your server).
`grantedScopes` therefore only lists `openid`, `email` and `profile`.

`Debug` output of `SignInAccount` and `SignInConfig` redacts the id
token and the nonce.
