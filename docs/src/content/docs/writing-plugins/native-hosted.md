---
title: Native-hosted plugin
description: Wrap a native SDK behind a Rust trait so your Rust code can call it as if it were local.
sidebar:
  order: 2
---

A native-hosted plugin declares its shape in Rust and delegates the
actual work to Kotlin (Android) and Swift (iOS). This is the shape you
want when the platform owns the API — permissions, background
execution, Sign in with Google, HealthKit, Live Activities.

## The full loop

1. You write the Rust trait with `#[istmo::plugin]`.
2. `istmo_build::emit()` publishes the contract.
3. The app-side `build.rs` generates Kotlin/Swift dispatchers into
   `android/` and `ios/`.
4. You implement each generated backend interface in Kotlin/Swift with
   the real platform code.
5. You register the backend in the runtime at app startup.
6. Your Rust code calls `<T>Client::method(...)` like any other async
   function.

## Skeleton — a system share sheet

```rust
// plugins/share-sheet/src/lib.rs
use istmo::plugin;

#[plugin(name = "acme.share_sheet")]
pub trait ShareSheet {
    async fn share_text(&self, text: String, subject: Option<String>)
        -> Result<(), ShareError>;
}

#[istmo::message]
pub struct ShareError {
    pub reason: String,
}

impl core::fmt::Display for ShareError { /* ... */ }
impl std::error::Error for ShareError {}
```

`istmo.toml`:

```toml
[plugin]
id          = "acme.share_sheet"
client_type = "::share_sheet::ShareSheetClient"

# The plugin needs nothing native at compile-time; the Android/iOS
# system APIs are available out of the box. Some plugins add Gradle or
# SwiftPM entries here (see the google-sign-in plugin for an example).
```

`build.rs`:

```rust
fn main() { istmo_build::emit(); }
```

## What gets generated at the app

After your app rebuilds, you'll see two new files per platform inside
your app tree:

```
android/app/src/main/java/com/myapp/gen/
├── ShareSheetTypes.kt
├── ShareSheetCodecsImpl.kt
└── ShareSheetDispatcher.kt           # exposes ShareSheetBackend interface
```

```
ios/MyApp/Plugins/ShareSheet/Generated/
├── ShareSheetTypes.swift
├── ShareSheetCodecsImpl.swift
└── ShareSheetDispatcher.swift        # exposes ShareSheetBackend protocol
```

The `Dispatcher` files are regenerated on every build — do not edit
them. The one file you write by hand is the `BackendImpl`.

## Implement the backend

### Android

```kotlin
package com.myapp.gen

import android.content.Context
import android.content.Intent

class ShareSheetBackendImpl(
    private val ctx: Context,
) : ShareSheetBackend {
    override suspend fun shareText(text: String, subject: String?): Unit {
        val intent = Intent(Intent.ACTION_SEND).apply {
            type = "text/plain"
            putExtra(Intent.EXTRA_TEXT, text)
            subject?.let { putExtra(Intent.EXTRA_SUBJECT, it) }
        }
        ctx.startActivity(Intent.createChooser(intent, null))
    }
}
```

### iOS

```swift
import UIKit

final class ShareSheetBackendImpl: ShareSheetBackend {
    func shareText(text: String, subject: String?) async throws {
        await MainActor.run {
            let vc = UIActivityViewController(
                activityItems: [text],
                applicationActivities: nil
            )
            if let subject { vc.setValue(subject, forKey: "subject") }
            UIApplication.shared.topViewController?.present(vc, animated: true)
        }
    }
}
```

## Ship the backend with the plugin

The backend above lives in the app. To ship it with the plugin crate
instead, put the sources in `native/android/` and `native/ios/` next to
`istmo.toml`: the Gradle plugin adds `native/android/` to the app's
source set, and `istmo-plugins.yml` adds `native/ios/` to the Xcode
target.

On Android, also declare the package those sources use:

```toml
[plugin]
id              = "acme.share_sheet"
client_type     = "::share_sheet::ShareSheetClient"
android_package = "com.acme.sharesheet"
```

The app then generates `ShareSheetDispatcher.kt`, `ShareSheetTypes.kt`
and `ShareSheetCodecsImpl.kt` into `com.acme.sharesheet` rather than its
own package, so `native/android/ShareSheetBackendImpl.kt` declares
`package com.acme.sharesheet`, names the generated types without
imports and compiles in every app, whatever its `[app] android_package`.
Runtime types (`BackendException`, `NativeHandleId`, `IstmoRuntime`,
`IstmoActivityBound`, …) still come from `dev.istmo.runtime`. The
registry builds `com.acme.sharesheet.ShareSheetBackendImpl` unless
`android_backend` names another class.

Swift has no packages: the generated Swift and `native/ios/` compile
into the same app module, so keep type names unique across plugins
(prefix them with the plugin name, as the generated `<T>Dispatcher`,
`<T>Types` and `<T>CodecsImpl` files do). `seed_backends` never seeds a
`<T>BackendImpl` for a plugin that ships native sources.

## Register the backend

By default `istmo-build` generates an `IstmoPluginRegistry` that wires
every eligible plugin dispatcher for you, and the runtime's entry points
call it before the Rust entry point runs:

```kotlin
// IstmoGameActivity, the template's launcher, already does this.
// An app with its own activity calls IstmoHost before super.onCreate:
override fun onCreate(savedInstanceState: Bundle?) {
    IstmoHost.onCreate(this) // starts the runtime + IstmoPluginRegistry
    super.onCreate(savedInstanceState)
}
```

```swift
// ios/<Name>App/main.swift
import IstmoRuntime

IstmoApp.run() // starts the runtime + IstmoPluginRegistry, then runs Rust
```

The registry builds the Android backend with a `Context` (or the host
activity, with `android_backend_arg`) and the iOS one with no
arguments. Plugins that need a
different constructor set `auto_register = false` in their
`istmo.toml` and expect the app to register manually. See
[Auto-registration](/istmo/build-scripts/auto-register/).

## Call from Rust

```rust
use share_sheet::ShareSheetClient;

let sheet = ShareSheetClient::from_runtime(&runtime)?;
sheet.share_text("Hello world".into(), None).await?;
```

## Seed the backend files

Iterating on a large plugin? Enable one-time skeleton generation with
`AppOpts::seed_backends`:

```rust
// build.rs (at the app crate root)
fn main() {
    istmo_build::emit_with(istmo_build::AppOpts {
        seed_backends: true,
        ..Default::default()
    });
}
```

The first build creates `ShareSheetBackendImpl.kt` and
`ShareSheetBackendImpl.swift` with `TODO()` / `fatalError` bodies. The
seed uses `write_if_absent`, so your subsequent edits are never
overwritten.

## Threading

Both Kotlin `suspend fun` and Swift `async` protocols are honored by
the generated dispatcher. Use whatever coroutine or task pattern is
idiomatic; Istmo does not impose an executor. If your work must land
on the main thread (UIKit APIs, view-lifecycle work), gate it with
`MainActor.run` / `withContext(Dispatchers.Main)` as usual.

## Next

- Streaming events: [Streams](/istmo/writing-plugins/streams/).
- Handing native resources back and forth: [Native handles](/istmo/writing-plugins/native-handles/).
- Long-running work: [Services and workers](/istmo/writing-plugins/services-workers/).
