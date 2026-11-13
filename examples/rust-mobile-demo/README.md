# rust-mobile-demo

A full-Rust mobile app driven by [egui](https://github.com/emilk/egui) that
talks to real platform APIs through istmo plugins, on Android and iOS.
Tapping **Sign in**:

1. requests `POST_NOTIFICATIONS` via the `istmo.permissions` plugin;
2. runs the Google Sign-In flow via the `istmo.google_sign_in` plugin;
3. posts a welcome notification via the `istmo.notifications` plugin.

A second card loads and shows AdMob interstitial and rewarded ads
(`istmo.admob`, Google test ad units), and a feed hosts banner slots
(`BannerSlot`). The layout follows the safe-area insets the host
publishes (`SafeArea`).

Every plugin call is a `Frame::Call` across the FFI boundary; every result
is a `Frame::Respond` on the way back. Rust code owns the UI, the async
control flow, the RAII lifetime of the returned Google credential (via
`NativeHandle<Credential>`), and the type shape of every wire payload.

## Layout

```
examples/rust-mobile-demo/
├── Cargo.toml             — cdylib + staticlib crate
├── istmo.toml             — [app] identity, auto_register = false, min versions
├── build.rs               — istmo_build::emit_with(extra_contracts: permissions, notifications, admob)
├── src/
│   ├── lib.rs             — istmo::runtime!(plugins: [...])
│   └── app.rs             — #[istmo::mobile_app] entry + eframe app (mobile only)
├── android/
│   └── app/src/main/
│       ├── AndroidManifest.xml — RustMobileActivity + permissions + AdMob app id
│       └── java/dev/istmo/
│           ├── rustdemo/RustMobileActivity.kt — NativeActivity, registers the backends
│           └── runtime/        — hand-written *BackendImpl / *FactoryImpl next to
│                                 the generated dispatchers (android_package = "dev.istmo.runtime")
└── ios/
    ├── project.yml        — xcodegen spec (includes the generated istmo-plugins.yml)
    └── RustMobileDemo/
        ├── main.swift     — IstmoApp.run(registerPlugins:entry:)
        └── Plugins/       — hand-written backends + generated dispatchers
```

The Google Sign-In wire types come from `istmo-google-sign-in`
(generated into `dev.istmo.plugins.googlesignin`); the demo ships its
own Android backend on the legacy `GoogleSignInClient` API instead of
the plugin's Credential Manager one.

## Before you build

1. **Server client id.** `src/app.rs` embeds `SERVER_CLIENT_ID`, the
   OAuth 2.0 *web* client id of the demo's Google Cloud project. Replace
   it with your own backend's client id, and register the Android app
   (package + SHA-1 of the signing key) in the same project. The
   justfile mounts `~/.android` into the build container so the debug
   keystore matches the SHA-1 you registered.
2. **Device (or emulator) with Google Play Services** for sign-in and
   ads.
3. **`rustup target add aarch64-linux-android`** (or the ABI you're
   building for) when building outside Docker. `gradle istmoDoctor`
   (from `android/`) lists anything else the toolchain is missing.

## Building

### Android

The Gradle build lives at `examples/rust-mobile-demo/android/`; `just`
wraps it in the project's Docker image.

```bash
just build rust-mobile-demo    # Gradle drives cargo
just install rust-mobile-demo  # adb install
just run rust-mobile-demo      # launch RustMobileActivity
just rust-mobile-demo          # all three
```

### iOS (macOS, Xcode 15+, `xcodegen`)

```bash
just rust-mobile-ios-bootstrap  # cargo check (writes ios/.istmo + istmo-plugins.yml), xcodegen
just rust-mobile-ios-build      # simulator build
just rust-mobile-ios-run        # install + launch on the booted simulator
just rust-mobile-ios            # bootstrap if needed, then run
```

## How it wires together at runtime

1. Android launches `dev.istmo.rustdemo.RustMobileActivity` (a
   `NativeActivity` subclass).
2. `onCreate` calls `IstmoHost.onCreate(this)`, which loads
   `librust_mobile_demo.so` and starts the runtime with the
   declarations from `istmo::runtime!` in `src/lib.rs`.
3. Because `istmo.toml` sets `auto_register = false` (the backends need
   this activity's permission and activity-result callbacks), the
   activity registers the Permissions, Notifications, SignIn and AdMob
   dispatchers itself, then calls `super.onCreate(...)`. On iOS,
   `main.swift` does the same inside `IstmoApp.run(registerPlugins:)`.
4. `NativeActivity` starts `android_main`, emitted by
   `#[istmo::mobile_app]` in `src/app.rs`, which runs `eframe`. Button
   handlers run the plugin flows on a background thread via
   `pollster::block_on`.
5. Each client call emits `Frame::Call`; the pump invokes the Kotlin /
   Swift dispatcher, whose backend answers with wire-encoded bytes that
   travel back as `Frame::Respond`.
6. `RustMobileActivity` forwards window insets to
   `IstmoRuntime.publishSafeArea(...)`; the app reads them through
   `SafeArea`.

## Known limitations

- The Android sign-in backend's `revoke()` calls
  `GoogleSignInClient.revokeAccess()` from the device; apps with a
  backend usually revoke the grant there too.
- The demo's notification backends are app-local reference code, not
  a published plugin.
