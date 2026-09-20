---
title: Services and workers
description: Run long-lived background code without turning your plugin into a service manager.
sidebar:
  order: 6
---

Two adapters extend the plugin macros to cover the two most common
long-running shapes:

- `#[istmo::service]` — an always-on background task that owns its own
  lifecycle (`on_start`, `on_stop`) and can be cancelled from either
  side.
- `#[istmo::worker]` — a one-shot unit of work with a stable id
  (matches Android's `WorkManager` model).

Both plug into the same `Runtime` and travel over the same frame
protocol as regular plugins.

## Services

```rust
use istmo::plugins::ServiceContext;

#[istmo::service(name = "myapp.sync")]
pub trait SyncService {
    async fn on_start(&self, ctx: ServiceContext) -> Result<(), SyncError>;
    async fn on_stop(&self);
}

#[istmo::message]
pub struct SyncError { pub reason: String }
impl core::fmt::Display for SyncError { /* ... */ }
impl std::error::Error for SyncError {}

pub struct SyncImpl;

impl SyncService for SyncImpl {
    async fn on_start(&self, ctx: ServiceContext) -> Result<(), SyncError> {
        loop {
            tokio::select! {
                _ = ctx.stopped() => return Ok(()),
                _ = tokio::time::sleep(Duration::from_secs(60)) => {
                    self.tick().await;
                }
            }
        }
    }

    async fn on_stop(&self) {
        self.flush().await;
    }
}
```

Notes on the trait shape:

- `#[istmo::service(name = "…")]` requires a plugin id. It becomes the
  wire identifier for the service, exactly like `[plugin] id = "…"`.
- `on_start` may return `()` or `Result<(), E>` (with any
  `#[istmo::message]` error type). `on_stop` returns `()`.
- `ServiceContext` is a plain type (not generic). It carries a
  `CancelToken`, a stop signal, and a handle back to your impl's
  state.
- No `#[async_trait::async_trait]` needed. The macro desugars every
  `async fn` in the trait to `impl Future + Send + '_`, and Rust
  1.75+'s async-fn-in-trait accepts plain `async fn` impls.

Register with `istmo::runtime!`:

```rust
istmo::runtime!(
    services: [
        SyncService => SyncImpl,
    ],
);
```

The `Trait => Impl` mapping tells the macro which impl to construct
behind the auto-generated `<T>Adapter`. The adapter is spawned on its
own OS thread using `pollster::block_on`, so `on_start` never blocks
any async caller.

### Android — `Service` wrapping

If you want the service to survive the app process (media playback,
location tracking), pair it with an Android `ForegroundService`.
Declare the shim in the plugin's `istmo.toml`:

```toml
[plugin]
id = "myapp.sync"
client_type = "::myapp_sync::SyncServiceClient"

[plugin.android_service]
class_name = "SyncForegroundService"
foreground_service_type = "dataSync"          # optional
exported = false                              # default false
permission = "android.permission.FOREGROUND_SERVICE_DATA_SYNC" # optional
process = ":sync"                              # optional
```

The app's `build.rs` (a one-line `istmo_build::emit()`) picks the spec
up through the standard manifest handover and materialises:

- `android/app/src/main/java/<namespace>.gen/SyncForegroundService.kt`
  — the `LifecycleService` shim that forwards `onStartCommand` /
  `onDestroy` into the Rust adapter registered under the same plugin
  id. `System.loadLibrary(...)` uses your app crate's Cargo package
  name (override with `[app] lib_name = "..."`).
- `android/app/src/main/AndroidManifest.services.xml` — a ready-to-copy
  `<service …/>` fragment listing every plugin-declared service.

If your real `android/app/src/main/AndroidManifest.xml` contains the
markers

```xml
<application>
    …
    <!-- istmo:services:start -->
    <!-- istmo:services:end -->
</application>
```

`emit_app` patches the block between them idempotently on every build,
so you never touch the XML again. Without the markers, copy the
sidecar file's contents once by hand.

### iOS — `BGTaskScheduler`

The iOS story is symmetric — declare the background task in
`istmo.toml`:

```toml
[plugin.ios_background]
class_name = "SyncBackgroundHandler"
task_identifier = "com.myapp.sync.refresh"
kind = "refresh"           # or "processing" | "continuous"
interval_minutes = 15      # required for `refresh`
# requires_power = true    # for `processing`
# requires_network = true  # for `processing`
# continuous_mode = "audio"  # for `continuous`
```

`emit_app` writes:

- `ios/<AppDir>/Plugins/Background/SyncBackgroundHandler.swift` — the
  `BGTaskScheduler.register(...)` + `expirationHandler` shim that
  forwards to the Rust adapter's `on_start` and cooperatively cancels
  it on expiration.
- `ios/<AppDir>/Info.plist.background.xml` — a ready-to-copy fragment
  carrying `BGTaskSchedulerPermittedIdentifiers` (for refresh /
  processing) or `UIBackgroundModes` (for continuous).

Add the markers `<!-- istmo:background:start -->` /
`<!-- istmo:background:end -->` inside your app's real `Info.plist`
`<dict>` and `emit_app` will patch them on every build. See the
[desktop deployment page](/istmo/advanced/desktop-deployment/) for the
sibling systemd / launchd / Windows Service module.

## Workers

Use `#[istmo::worker]` when you want a **discrete** unit of work with a
stable unique id — the shape Android's `WorkManager` and iOS's
`URLSession.background` want:

```rust
#[istmo::worker(name = "myapp.image_upload")]
pub trait ImageUploadWorker {
    async fn run(&self, task_id: u64, unique_name: String, input: UploadInput)
        -> Result<UploadOutcome, UploadError>;
}

pub struct ImageUploadWorkerImpl;

impl ImageUploadWorker for ImageUploadWorkerImpl {
    async fn run(&self, task_id: u64, unique_name: String, input: UploadInput)
        -> Result<UploadOutcome, UploadError>
    { /* … */ }
}
```

Register:

```rust
istmo::runtime!(
    workers: [
        ImageUploadWorker => ImageUploadWorkerImpl,
    ],
);
```

The generated `ImageUploadWorkerClient` exposes a
`.schedule(unique_name, input, config)` method that hands the request
to the platform's job scheduler. `WorkManager` on Android will:

- Persist the job across process death.
- Retry with backoff on transient failure.
- De-duplicate by `unique_name` (existing job with the same name
  either replaces or is kept, depending on `WorkPolicy`).

## Cooperative shutdown

Both service and worker `Context`s expose a `CancelToken`. Watch for
it during your busy loop:

```rust
if ctx.cancel().is_cancelled() { return; }
tokio::select! {
    _ = ctx.stopped()          => { /* stop signal */ }
    _ = ctx.cancel().cancelled() => { /* platform cancel */ }
    result = self.do_work()      => { /* work done */ }
}
```

See [Cancellation](/istmo/writing-plugins/cancellation/) for the details.

## Next

- `:remote` process bridge: [Advanced → Remote process](/istmo/advanced/remote-process/).
- Desktop deployment: [Advanced → Desktop deployment](/istmo/advanced/desktop-deployment/).
