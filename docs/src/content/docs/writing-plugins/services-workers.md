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

### Android — `Service` wrapping (manual today)

If you want the service to survive the app process (media playback,
location tracking), pair it with an Android `ForegroundService`.
`istmo-build` exposes a `generate_android_service` helper you can call
from your plugin's `build.rs`; **it is not wired into `emit()`
automatically today**, so you need to invoke it yourself:

```rust
// build.rs (at the plugin crate root)
use istmo_build::{ServiceContract, generate_android_service};

fn main() {
    istmo_build::emit();

    let out = generate_android_service(&ServiceContract {
        plugin_id: "myapp.sync".into(),
        service_class: "com.myapp.SyncForegroundService".into(),
        // …notification channel, foreground type, etc.
    });
    std::fs::write("android/SyncForegroundService.kt", out.source).unwrap();
    std::fs::write("android/AndroidManifest.snippet.xml", out.manifest).unwrap();
}
```

Copy the emitted files into your app's `android/app/src/main/`
tree. Auto-wiring the `Service` scaffolding through `emit()` is on the
roadmap — until it lands, this manual invocation is the shape.

### iOS — `BGTaskScheduler` (manual today)

The iOS story is symmetric — `generate_ios_background` produces the
`Info.plist` fragment + `AppDelegate` handler for `BGAppRefreshTask` /
`BGProcessingTask`. Same story: you call it yourself from `build.rs`,
`emit()` does not wire it. See the [desktop deployment page](/advanced/desktop-deployment/)
for the sibling module.

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

See [Cancellation](/writing-plugins/cancellation/) for the details.

## Next

- `:remote` process bridge: [Advanced → Remote process](/advanced/remote-process/).
- Desktop deployment: [Advanced → Desktop deployment](/advanced/desktop-deployment/).
