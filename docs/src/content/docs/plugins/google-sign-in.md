---
title: Google Sign-In
description: OAuth sign-in on Android (Credential Manager) and iOS (GoogleSignIn SDK).
sidebar:
  order: 1
---

`istmo-google-sign-in` wraps the platform-idiomatic Google Sign-In
flows behind a single Rust trait.

- **Android** uses `androidx.credentials` (Credential Manager) plus
  the `googleid` provider.
- **iOS** uses the official `GoogleSignIn` SwiftPM package (7.0+).

Both flows return a `NativeHandle<Credential>` you can refresh and
release deterministically.

## Install

```toml
[dependencies]
istmo-google-sign-in = "0.1"
```

Restart your app build; `istmo_build::emit()` picks up the plugin's
Gradle + SwiftPM entries automatically. No further platform config
required beyond the OAuth client IDs.

## Configure the OAuth clients

Follow the [Google Cloud Console
walkthrough](https://developers.google.com/identity) to create:

- One **Web** OAuth client — used as `serverClientId` on Android.
- One **iOS** OAuth client — used as `clientID` on iOS.

Add them to your app's config module and pass them at runtime:

```rust
use istmo_google_sign_in::{SignInClient, SignInConfig};

let sign_in = SignInClient::from_runtime(&runtime)?;
sign_in.configure(SignInConfig {
    server_client_id: "1234-web.apps.googleusercontent.com".into(),
    ios_client_id: Some("1234-ios.apps.googleusercontent.com".into()),
    scopes: vec!["email".into(), "profile".into()],
}).await?;
```

## Sign in

```rust
let account = sign_in.sign_in_owned(SignInRequest {
    prompt: SignInPrompt::SelectAccount,
}).await?;

tracing::info!("signed in as {} ({})", account.display_name, account.email);
// account.credential: NativeHandle<Credential>
```

Use `sign_in_owned` (owned variant) unless you specifically need to
re-ship the raw `NativeHandleId` — the owned variant frees the
credential automatically on drop.

## Refresh silently

Silent refresh returns a new credential handle. Drop the old one first
if you want deterministic release:

```rust
let refreshed = sign_in.refresh_owned(&account.credential).await?;
```

## Sign out

```rust
sign_in.sign_out().await?;
```

`sign_out` invalidates the credential on the native side and emits
`Frame::ReleaseNativeHandle` for any outstanding credential your Rust
code still holds.

## Android specifics

The plugin declares its Gradle deps via `istmo.toml`:

```toml
[[gradle]]
group    = "androidx.credentials"
artifact = "credentials"
version  = "1.3.0"

[[gradle]]
group    = "androidx.credentials"
artifact = "credentials-play-services-auth"
version  = "1.3.0"

[[gradle]]
scope    = "api"
group    = "com.google.android.libraries.identity.googleid"
artifact = "googleid"
version  = "1.1.1"
```

Nothing to add to your app-side `build.gradle.kts` — the entries flow
through `istmo-build`'s native-dep handover.

The Credential Manager flow needs `Activity` context. The reference
backend under `plugins/google-sign-in/native/android/` picks it up
from `IstmoRuntime.instance.currentActivity`.

## iOS specifics

The plugin declares the Swift package:

```toml
[[swift_package]]
url          = "https://github.com/google/GoogleSignIn-iOS.git"
product      = "GoogleSignIn"
from_version = "7.0.0"
```

Your `Info.plist` must include the reversed client id under
`CFBundleURLSchemes`, and your `App.swift` must call
`GIDSignIn.sharedInstance.handle(url:)` inside `.onOpenURL`. The
plugin ships a `SignInAppDelegate` helper that does both for you if
you register it in `IstmoRuntime.shared`.

## Wire it up

`SignIn` is stateful (it carries an OAuth configuration per instance),
so `istmo-build` generates it into `IstmoPluginRegistry.registerAll(...)`
using its `SignInFactoryImpl`. As long as you copy the reference
`SignInFactoryImpl.kt` / `SignInFactoryImpl.swift` into your app tree
and call the registry once, the plugin is wired.

```kotlin
override fun onCreate(savedInstanceState: Bundle?) {
    IstmoRuntime.instance.start(this)
    IstmoPluginRegistry.registerAll(applicationContext)
    super.onCreate(savedInstanceState)
}
```

```swift
@main
struct MyApp: App {
    init() {
        IstmoRuntime.shared.start()
        IstmoPluginRegistry.registerAll()
    }
}
```

See [Auto-registration](/build-scripts/auto-register/) for the full
mechanism and opt-out flags.

## Reference backends

The plugin repo ships reference implementations under
`plugins/google-sign-in/native/`:

- `android/SignInBackendImpl.kt`
- `ios/SignInBackendImpl.swift`

Copy these into your app tree the first time you enable the plugin,
then customise as needed. Because the trait shape is fixed, most apps
never need to touch them.

## Full end-to-end sample

See [`examples/rust-mobile-demo`](https://github.com/sergioribera/istmo/tree/main/examples/rust-mobile-demo)
in the repo — an egui-based app that combines sign-in, permissions,
notifications, and AdMob in a single Rust binary.
