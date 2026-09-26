---
title: Native projects
description: How build.rs, istmo.toml and the Gradle / Xcode integration keep android/ and ios/ in sync — no CLI involved.
sidebar:
  order: 4
---

istmo has no CLI. An app is a Cargo crate with an `android/` Gradle
project and/or an `ios/` Xcode project next to it, and three pieces keep
them in sync:

| Piece | Runs | Does |
| --- | --- | --- |
| `build.rs` → `istmo_build::emit()` | every `cargo build` / `cargo check` | codegen, plugin metadata, app identity, doctor warnings |
| `dev.istmo.app` Gradle plugin | every Gradle build | builds the Rust crate, links plugins, applies `[app]` |
| `istmo-plugins.yml` + `ios/.istmo/build-rust.sh` | every Xcode build | builds the Rust crate, links plugins, applies `[app]` |

`istmo.toml` is the single source of truth. The native manifests
(`AndroidManifest.xml`, `Info.plist`) stay yours; they reference values
istmo provides.

## App identity: `[app]`

```toml
[app]
id      = "com.example.myapp"   # applicationId + PRODUCT_BUNDLE_IDENTIFIER
name    = "My App"              # launcher label / CFBundleDisplayName
build   = 12                    # versionCode / CFBundleVersion (default 1)
icon    = "assets/icon.png"     # launcher icons for both platforms
# version = "1.2.0"             # default: Cargo.toml `package.version`

[min_versions]
android = 24                    # minSdk
ios     = "15.0"                # IPHONEOS_DEPLOYMENT_TARGET
```

Every key is optional. Leave `id` out when Android and iOS need
different identifiers — the native projects keep theirs.

## Android

Apply the plugin to the app module:

```kotlin
// android/app/build.gradle.kts
plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("dev.istmo.app")
}

android {
    namespace = "com.example.myapp"
    compileSdk = 34
    defaultConfig {
        targetSdk = 34
        ndk { abiFilters += setOf("arm64-v8a", "x86_64") }
    }
}
```

and reference the placeholders from the manifest:

```xml
<application
    android:label="${istmoLabel}"
    android:icon="${istmoIcon}">
    <activity android:name="dev.istmo.runtime.IstmoGameActivity" android:exported="true">
        <meta-data android:name="android.app.lib_name" android:value="my_app" />
        <!-- MAIN / LAUNCHER intent filter -->
    </activity>
</application>
```

`dev.istmo.app` then:

- **builds the crate** with `cargo build --target <triple> --profile <p>`
  for every ABI in `abiFilters` (default `arm64-v8a` + `x86_64`) and
  every build type (`debug` → `dev`, others → `release`), and packages
  `lib<crate>.so`. The NDK's clang is used as linker and as `CC` / `AR`
  for `cc`-based build scripts unless you exported your own;
- **links every plugin** the crate depends on — from the workspace,
  crates.io or git — using `android/.istmo/istmo.json`, written by
  `build.rs`: Kotlin sources, manifests, resources, `[[gradle]]`
  dependencies, and the `[min_versions] android` check;
- **applies `[app]`**: `applicationId`, `versionName`, `versionCode`,
  `minSdk`, `${istmoLabel}`, `${istmoIcon}`. A value set in Gradle that
  disagrees is replaced, with a warning.

If `istmo.json` is missing or older than `Cargo.toml`, `Cargo.lock` or
`istmo.toml`, the plugin refreshes it with `cargo check` before
configuring. When cargo changes the plugin set during a build, the build
stops and asks you to run it again.

Optional configuration:

```kotlin
istmo {
    profiles.put("debug", "release")   // cargo profile per build type
    cargoArgs.addAll("--features", "extra")
    defaultAbis.set(listOf("arm64-v8a"))
    cargo.set("/opt/rust/bin/cargo")    // default: PATH, then ~/.cargo/bin
    crateDir.set(file("../rust"))       // default: the Gradle root's parent
}
```

`dev.istmo.app` replaces `dev.istmo.plugin-loader`, which only linked
plugins that are members of the same Cargo workspace.

## iOS

`build.rs` writes, next to your app sources, `istmo-plugins.yml`, and
under `ios/.istmo/`:

- `Istmo.xcconfig` — bundle id, `MARKETING_VERSION`,
  `CURRENT_PROJECT_VERSION`, `ISTMO_DISPLAY_NAME`, deployment target,
  app icon set and the Rust library link settings;
- `build-rust.sh` — the pre-build phase: runs cargo for every
  architecture Xcode builds and stages `lib<crate>.a`;
- `Assets.xcassets/IstmoAppIcon.appiconset` when `[app] icon` is set.

With xcodegen, include the fragment — it carries the plugin sources, the
same build settings and the pre-build phase:

```yaml
# ios/project.yml
name: MyApp
include:
  - path: MyApp/istmo-plugins.yml
targets:
  MyApp:
    type: application
    platform: iOS
    sources: [MyApp]
    info: { path: MyApp/Info.plist }
    dependencies:
      - package: IstmoRuntime
```

Settings in `project.yml` win over the fragment's, so drop
`PRODUCT_BUNDLE_IDENTIFIER`, `OTHER_LDFLAGS` and friends from it. For a
hand-made Xcode project, base the target on `Istmo.xcconfig` and add
`"${PROJECT_DIR}/.istmo/build-rust.sh"` as a Run Script phase.

In `Info.plist`, reference the generated values:

```xml
<key>CFBundleDisplayName</key>        <string>$(ISTMO_DISPLAY_NAME)</string>
<key>CFBundleShortVersionString</key> <string>$(MARKETING_VERSION)</string>
<key>CFBundleVersion</key>            <string>$(CURRENT_PROJECT_VERSION)</string>
```

`MARKETING_VERSION` drops any pre-release suffix (`0.3.0-beta.1` →
`0.3.0`), as App Store Connect requires.

The generated files are regenerated on every build; run one
`cargo build` (any target) after cloning, before `xcodegen generate`.

## Doctor

Setup mistakes are reported where you already look, with the fix
spelled out:

- **`cargo build`** — `build.rs` prints `istmo doctor:` warnings: a
  missing `cdylib` / `staticlib` crate type, `android/app` without
  `dev.istmo.app`, a manifest that ignores `${istmoLabel}` /
  `${istmoIcon}`, a `project.yml` without the fragment, an `Info.plist`
  that hardcodes the version. A malformed or misspelled `istmo.toml` key
  fails the build.
- **Gradle** — before cargo runs, missing pieces fail the build:
  `cargo` not found (also when Android Studio was started without your
  shell `PATH`), Rust target not installed, no NDK. `./gradlew
  istmoDoctor` prints the whole report:

  ```text
  istmo doctor
    ✓ cargo: /home/me/.cargo/bin/cargo
    ✗ Rust target x86_64-linux-android is not installed
        → run `rustup target add x86_64-linux-android`
    ✓ linker for aarch64-linux-android: NDK 26.1.10909125
    ✓ android/.istmo/istmo.json: 2 plugin(s), app com.example.myapp
  1 issue(s) found.
  ```
- **Xcode** — `build-rust.sh` stops with an `error:` line when cargo or
  the Rust target is missing.

## What to commit

Everything `build.rs` generates is rebuilt on each build and can be
ignored:

```gitignore
android/.istmo/
ios/.istmo/
android/app/src/main/java/dev/istmo/generated/
ios/*/Plugins/IstmoMain.swift
ios/*/Plugins/IstmoPluginRegistry.swift
```

Commit `istmo-plugins.yml` if your `project.yml` includes it and you
want `xcodegen generate` to work before the first cargo build.
