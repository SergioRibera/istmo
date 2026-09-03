# rust-mobile-demo

A full-Rust mobile app driven by [egui](https://github.com/emilk/egui) that
talks to real Android platform APIs through istmo plugins. One button on
screen. Tap it and the demo:

1. Requests `POST_NOTIFICATIONS` via the `istmo.permissions` plugin.
2. Runs the Google Sign-In flow via the `istmo.google_sign_in` plugin
   (Credential Manager + Google Identity Services).
3. Posts a welcome notification via the `istmo.notifications` plugin.

Every plugin call is a `Frame::Call` across the JNI boundary; every result
is a `Frame::Respond` on the way back. Rust code owns the UI, the async
control flow, the RAII lifetime of the returned Google credential (via
`NativeHandle<Credential>`), and the type shape of every wire payload.

## Layout

```
examples/rust-mobile-demo/
├── Cargo.toml             — cdylib crate, Android-only egui/winit deps
├── src/
│   ├── lib.rs             — istmo::runtime!(plugins: [...])
│   └── android.rs         — android_main + eframe app (cfg android)
└── android/
    ├── build.gradle.kts / settings.gradle.kts / gradle.properties
    └── app/
        ├── build.gradle.kts        — istmoCargoLib + native deps
        └── src/main/
            ├── AndroidManifest.xml — NativeActivity subclass + POST_NOTIFICATIONS
            └── java/dev/istmo/
                ├── rustdemo/RustMobileActivity.kt
                └── runtime/
                    ├── IstmoRuntime.kt         — JNI singleton + pump callbacks
                    ├── PluginHandler.kt
                    ├── Bincode.kt
                    ├── PermissionsHandler.kt   — ActivityCompat.requestPermissions
                    ├── NotificationsHandler.kt — NotificationManagerCompat
                    └── GoogleSignInHandler.kt  — Credential Manager + GoogleIdOption
```

## Before you build

1. **Server client id.** `src/android.rs` embeds
   `REPLACE_WITH_YOUR_SERVER_CLIENT_ID.apps.googleusercontent.com`. Replace
   it with the OAuth 2.0 client id issued for your *backend* in the Google
   Cloud Console. The Android app also needs to be registered with the
   correct SHA-1 fingerprint against your Firebase / GCP project — read
   Google's Credential Manager docs.
2. **Device (or emulator) with Google Play Services.** The Credential
   Manager backing store lives in Play Services; an image without them
   returns `NoCredentialException` for every call.
3. **`rustup target add aarch64-linux-android`** (or the ABI you're
   building for).

## Building

The repo uses the same Docker recipe as `android-demo`; the Kotlin build
lives at `examples/rust-mobile-demo/android/`, and `just` wraps the
Gradle invocation.

```bash
# Build APK inside the docker image (Gradle drives cargo).
just build rust-mobile-demo

# Install on the connected device / emulator.
just install rust-mobile-demo

# Launch RustMobileActivity.
just run rust-mobile-demo

# All three in one shot (flutter-run-style).
just rust-mobile-demo
```

## How it wires together at runtime

1. Android launches `dev.istmo.rustdemo.RustMobileActivity`
   (a `NativeActivity` subclass declared in the manifest).
2. `RustMobileActivity.onCreate` calls `IstmoRuntime.start()` *before*
   `super.onCreate()`. `IstmoRuntime`:
   - `System.loadLibrary("rust_mobile_demo")` — loads the cdylib the
     manifest's `android.app.lib_name` metadata also references.
   - `nativeStart(...)` — resolves `__istmo_configure_runtime` (emitted
     by the `istmo::runtime!` macro in `src/lib.rs`), initialises the
     process-global `Runtime`, spawns the outbound pump thread.
3. `RustMobileActivity.onCreate` registers three `PluginHandler`s under
   the plugin ids `istmo.permissions`, `istmo.notifications`,
   `istmo.google_sign_in`, then calls `super.onCreate(...)`.
4. `NativeActivity`'s super.onCreate triggers `ANativeActivity_onCreate`,
   which the `android-activity` crate hooks; a fresh thread runs
   `android_main` (`src/android.rs`).
5. `android_main` starts `eframe`; when the user taps the button, a
   background thread runs the plugin chain via `pollster::block_on`.
6. Every `SignInClient::...` / `PermissionsClient::...` /
   `NotificationsClient::...` call emits `Frame::Call`; the Rust pump
   invokes `IstmoRuntime.onCall(...)` in Kotlin; the matching
   `PluginHandler` produces bincode-encoded response bytes;
   `IstmoRuntime.nativeSubmitResponse(...)` ships them back to Rust.

## Known limitations

- The Google Sign-In handler does not implement backend-side OAuth
  revocation for the `revoke()` method — it only clears the local
  credential cache. Real apps must call their backend's
  `/token/revoke` endpoint.
- `Notifications::schedule` honours zero delay only. Delayed posts
  would require an `AlarmManager` + `BroadcastReceiver` shim; deferred
  until a real use case surfaces.
- iOS parity is stubbed: an iOS twin of this demo needs Swift
  dispatchers for the three plugins (Credential Manager equivalents,
  `UNUserNotificationCenter`, `UNAuthorizationOptions`). Follow-up
  behind the M5 iOS-demo scaffold.
- `NativeHandle<Credential>` release fires on Rust drop, but the
  Kotlin `IstmoRuntime.onReleaseNativeHandle` currently no-ops. Wire
  it into `GoogleSignInHandler.releaseCredential(handleId)` when a
  real app needs deterministic release.
