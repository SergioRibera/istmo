---
title: Services and workers
description: Run long-lived background code without turning your plugin into a service manager.
sidebar:
  order: 6
---

Two adapters extend the plugin macros to cover the two most common
long-running shapes:

- `#[istmo::service]` — an always-on background task that owns its own
  lifecycle (`on_start`, `on_stop`) and can be stopped from either
  side.
- `#[istmo::worker]` — a one-shot unit of work identified by a task id
  and a unique name (Android's `WorkManager` model).

Both are Rust-hosted: they plug into the same `Runtime` as regular
plugins and travel over the same frame protocol. Both take the
attributes `name = "…"` (required, the wire id), `crate = "…"` (runtime
path, default `::istmo`) and `plugins = "…"` (path to the first-party
plugin types, default `<crate>::plugins`; a crate that sets
`crate = "::istmo_core"` also sets `plugins = "::istmo_plugins"`).

## Services

```rust
use istmo::plugins::ServiceContext;

#[istmo::service(name = "myapp.sync")]
pub trait SyncService {
    async fn on_start(&self, ctx: ServiceContext) -> Result<(), SyncError>;
    async fn on_stop(&self);
}

#[istmo::message]
#[derive(Debug)]
pub struct SyncError { pub reason: String }
impl core::fmt::Display for SyncError { /* ... */ }
impl std::error::Error for SyncError {}

pub struct SyncImpl;

impl SyncService for SyncImpl {
    async fn on_start(&self, ctx: ServiceContext) -> Result<(), SyncError> {
        while !ctx.is_stopped() {
            self.sync_once().await?;
            // wait for the next round — or for `ctx.stopped()` — with
            // your executor's timer and `select`
        }
        Ok(())
    }

    async fn on_stop(&self) {
        self.flush().await;
    }
}
```

Rules the macro enforces:

- The trait declares only `on_start` and, optionally, `on_stop`; any
  other method is an error.
- `async fn on_start(&self, ctx: ServiceContext)` returns `()` or
  `Result<(), E>`. `async fn on_stop(&self)` takes no other argument.
- Plain `async fn`s; the macro desugars them, no `#[async_trait]`.

`ServiceContext` is what the service sees of its host:

| Method | Purpose |
|---|---|
| `stopped().await` / `is_stopped()` | Wait for / poll the stop signal (stop request, cancel, host gone). |
| `cancel_token()` | The run's `CancelToken`. |
| `service_id()`, `runtime()` | The service's id and runtime. |
| `set_foreground(spec)`, `update_notification(spec)`, `stop_foreground(remove)` | Foreground notification (`NotificationSpec`). |
| `acquire_wakelock(tag)` | A `WakeLock`, released on `release().await` or drop. |
| `stop_self()` | Ask the platform to stop the service. |

The platform calls go through the `istmo.service_control` plugin
(`ServiceControl` in `istmo::plugins`). No native backend for it ships
with the runtime: generate its dispatcher by passing
`istmo_plugins_schema::service_control()` in
`AppOpts::extra_contracts` and implement it in Kotlin / Swift.

Register the service with `istmo::runtime!`:

```rust
istmo::runtime!(
    services: [SyncService => SyncImpl],
);
```

The macro registers `SyncServiceAdapter::new(SyncImpl)`, so
`<Trait>Adapter` must resolve at that path. The adapter runs
`on_start` on its own OS thread (`pollster::block_on`), so it never
blocks an async caller; a second `on_start` stops and joins the
previous run first, and `on_stop` or a `Frame::Cancel` stops it through
the context.

### Android — `Service` shim

To run the service as an Android `Service` (media playback, location
tracking, sync), declare the shim in the `istmo.toml` of the crate that
defines the service:

```toml
[plugin]
id = "myapp.sync"   # same as #[istmo::service(name = "…")]

[plugin.android_service]
class_name = "SyncForegroundService"
foreground_service_type = "dataSync"          # optional
exported = false                              # default false
permission = "android.permission.FOREGROUND_SERVICE_DATA_SYNC" # optional
process = ":sync"                             # optional
```

Leave `client_type` out: the service macro emits no client. A plugin
entry without `client_type` always contributes its shims; one with a
`client_type` contributes them only when the app lists that client in
`istmo::runtime!`.

The app's `build.rs` (`istmo_build::emit()`) picks the spec up through
the manifest handover and writes:

- `android/app/src/main/java/<app package>/SyncForegroundService.kt` —
  a `LifecycleService` (so the app needs
  `androidx.lifecycle:lifecycle-service`) that loads the app's Rust
  library, starts the runtime, and forwards `onStartCommand` /
  `onDestroy` to the adapter's `on_start` / `on_stop` under the same
  plugin id. `<app package>` is the app's Kotlin package (`<namespace>.gen`
  or `[app] android_package`); the library name is the crate's
  `[lib] name`, or its package name with `-` replaced by `_`.
- `android/app/src/main/AndroidManifest.services.xml` — the
  `<service …/>` element for every declared service.

If your `android/app/src/main/AndroidManifest.xml` contains the
markers

```xml
<application>
    …
    <!-- istmo:services:start -->
    <!-- istmo:services:end -->
</application>
```

the build patches the block between them on every build, so you never
touch the XML again (the template's manifest ships them). Without the
markers, copy the sidecar's contents once by hand.

### iOS — `BGTaskScheduler`

Declare the background task in the same `istmo.toml`:

```toml
[plugin.ios_background]
class_name = "SyncBackgroundHandler"
task_identifier = "com.myapp.sync.refresh"
kind = "refresh"           # or "processing" | "continuous"
interval_minutes = 15      # required for `refresh`
# requires_power = true    # for `processing` (default false)
# requires_network = true  # for `processing` (default false)
# continuous_mode = "audio"  # required for `continuous`: audio | location | voip |
#                            # external_accessory | bluetooth_central | bluetooth_peripheral
```

The build writes:

- `ios/<AppDir>/Plugins/Background/SyncBackgroundHandler.swift` — an
  enum with `register()` (registers the `BGTaskScheduler` handler) and
  `schedule()` (submits the next request). The handler calls the
  adapter's `on_start` and cancels it from the task's
  `expirationHandler`. Call `SyncBackgroundHandler.register()` once at
  launch, before the app finishes launching, and `schedule()` to queue
  the first run.
- `ios/<AppDir>/Info.plist.background.xml` — the
  `BGTaskSchedulerPermittedIdentifiers` (refresh / processing) or
  `UIBackgroundModes` (continuous) entries.

With `<!-- istmo:background:start -->` / `<!-- istmo:background:end -->`
inside your `Info.plist` `<dict>` (the template's has them), the build
patches those entries in on every build. See the
[desktop deployment page](/istmo/advanced/desktop-deployment/) for the
desktop side.

## Workers

Use `#[istmo::worker]` for a **discrete** unit of work:

```rust
use istmo::plugins::{TaskOutcome, WorkerContext};

#[istmo::worker(name = "myapp.backup")]
pub trait Backup {
    async fn run(&self, ctx: WorkerContext) -> TaskOutcome;
}

pub struct Nightly;

impl Backup for Nightly {
    async fn run(&self, ctx: WorkerContext) -> TaskOutcome {
        if ctx.is_cancelled() {
            return TaskOutcome::Retry;
        }
        match upload(ctx.input()).await {
            Ok(()) => TaskOutcome::Success,
            Err(_) => TaskOutcome::Retry,
        }
    }
}
```

The trait declares exactly `async fn run(&self, ctx: WorkerContext)`,
returning `TaskOutcome` (`Success`, `Retry`, `Failure`) or
`Result<TaskOutcome, E>`. `WorkerContext` carries `task_id()`,
`unique_name()`, the raw `input()` bytes (`into_input()` to take them),
`is_cancelled()` / `cancel_token()` and `runtime()`.

Register it:

```rust
istmo::runtime!(
    workers: [Backup => Nightly],
);
```

The macro registers `BackupWorkerAdapter::new(Nightly)`, which answers
the `run` method of plugin `myapp.backup` with a payload of
`(task_id, unique_name, input)`.

Scheduling is the platform's job. `istmo::plugins::TaskSchedulerClient`
(`istmo.task_scheduler`) declares the API — `enqueue(TaskRequest)` with
`Constraints`, an initial delay, tags and an `ExistingWorkPolicy`, plus
`cancel`, `cancel_by_tag` and `cancel_by_unique_name` — but no native
backend ships with the runtime: generate its dispatcher by passing
`istmo_plugins_schema::task_scheduler()` in `AppOpts::extra_contracts`
and implement it with `WorkManager` / `BGTaskScheduler`. On Android,
`istmo_build::generate_android_worker(&WorkerContract::new(..))`
returns the source of a `CoroutineWorker` that calls the worker's `run`
and maps the `TaskOutcome` to `Result.success()` / `retry()` /
`failure()`; write it from your `build.rs`.

## Cooperative shutdown

Both contexts expose the run's `CancelToken`. Poll it in a busy loop
and await it when idle:

```rust
// service
if ctx.is_stopped() { return Ok(()); }
ctx.stopped().await;

// worker
if ctx.is_cancelled() { return TaskOutcome::Retry; }
ctx.cancel_token().cancelled().await;
```

See [Cancellation](/istmo/writing-plugins/cancellation/) for the details.

## Next

- `:remote` process bridge: [Advanced → Remote process](/istmo/advanced/remote-process/).
- Desktop deployment: [Advanced → Desktop deployment](/istmo/advanced/desktop-deployment/).
