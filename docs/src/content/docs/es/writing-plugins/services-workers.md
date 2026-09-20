---
title: Services y workers
description: Corre código de background de larga duración sin convertir tu plugin en un gestor de services.
sidebar:
  order: 6
---

Dos adapters extienden las macros de plugin para cubrir las dos formas
más comunes de larga duración:

- `#[istmo::service]` — una task always-on de background que posee su
  ciclo de vida (`on_start`, `on_stop`) y puede cancelarse desde
  cualquier lado.
- `#[istmo::worker]` — una unidad one-shot de trabajo con un id
  estable (matchea el modelo de `WorkManager` de Android).

Ambos se enchufan al mismo `Runtime` y viajan por el mismo protocolo
de frames que los plugins regulares.

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

Notas sobre la forma del trait:

- `#[istmo::service(name = "…")]` requiere un plugin id. Se vuelve el
  identificador de wire para el service, exactamente como
  `[plugin] id = "…"`.
- `on_start` puede retornar `()` o `Result<(), E>` (con cualquier tipo
  de error `#[istmo::message]`). `on_stop` retorna `()`.
- `ServiceContext` es un tipo plano (no genérico). Carga un
  `CancelToken`, una señal de stop, y un handle de vuelta al estado de
  tu impl.
- No hace falta `#[async_trait::async_trait]`. La macro desugara cada
  `async fn` del trait a `impl Future + Send + '_`, y el async-fn-in-trait
  de Rust 1.75+ acepta impls con `async fn` plano.

Registralo con `istmo::runtime!`:

```rust
istmo::runtime!(
    services: [
        SyncService => SyncImpl,
    ],
);
```

El mapping `Trait => Impl` le dice a la macro qué impl construir
detrás del `<T>Adapter` auto-generado. El adapter se spawnea en su
propio thread OS usando `pollster::block_on`, así que `on_start`
nunca bloquea a un caller async.

### Android — wrapping como `Service`

Si querés que el service sobreviva al proceso de la app (media
playback, tracking de ubicación), pareálo con un `ForegroundService`
Android. Declará el shim en el `istmo.toml` del plugin:

```toml
[plugin]
id = "myapp.sync"
client_type = "::myapp_sync::SyncServiceClient"

[plugin.android_service]
class_name = "SyncForegroundService"
foreground_service_type = "dataSync"           # opcional
exported = false                               # default false
permission = "android.permission.FOREGROUND_SERVICE_DATA_SYNC" # opcional
process = ":sync"                              # opcional
```

El `build.rs` de la app (un one-liner `istmo_build::emit()`) recoge
el spec a través del handover estándar del manifest y materializa:

- `android/app/src/main/java/<namespace>.gen/SyncForegroundService.kt`
  — el shim `LifecycleService` que forwardea `onStartCommand` /
  `onDestroy` al adapter Rust registrado bajo el mismo plugin id.
  `System.loadLibrary(...)` usa el `CARGO_PKG_NAME` de tu crate app
  (overrideable con `[app] lib_name = "..."`).
- `android/app/src/main/AndroidManifest.services.xml` — un fragmento
  `<service …/>` listo-para-copiar listando todos los services
  declarados por plugins.

Si tu `android/app/src/main/AndroidManifest.xml` real contiene los
marcadores

```xml
<application>
    …
    <!-- istmo:services:start -->
    <!-- istmo:services:end -->
</application>
```

`emit_app` parcha el bloque entre ellos idempotentemente en cada
build, así nunca más tocás el XML. Sin los marcadores, copiá los
contenidos del sidecar una vez a mano.

### iOS — `BGTaskScheduler`

La historia iOS es simétrica — declará el background task en
`istmo.toml`:

```toml
[plugin.ios_background]
class_name = "SyncBackgroundHandler"
task_identifier = "com.myapp.sync.refresh"
kind = "refresh"           # o "processing" | "continuous"
interval_minutes = 15      # requerido para `refresh`
# requires_power = true    # para `processing`
# requires_network = true  # para `processing`
# continuous_mode = "audio"  # para `continuous`
```

`emit_app` escribe:

- `ios/<AppDir>/Plugins/Background/SyncBackgroundHandler.swift` — el
  shim `BGTaskScheduler.register(...)` + `expirationHandler` que
  forwardea al `on_start` del adapter Rust y lo cancela
  cooperativamente cuando expira.
- `ios/<AppDir>/Info.plist.background.xml` — un fragmento listo-para-
  copiar con `BGTaskSchedulerPermittedIdentifiers` (para refresh /
  processing) o `UIBackgroundModes` (para continuous).

Agregá los marcadores `<!-- istmo:background:start -->` /
`<!-- istmo:background:end -->` dentro del `<dict>` de tu `Info.plist`
real y `emit_app` los parcha en cada build. Ver la [página de
deployment de desktop](/istmo/es/advanced/desktop-deployment/) para el
módulo hermano systemd / launchd / Windows Service.

## Workers

Usa `#[istmo::worker]` cuando querés una unidad **discreta** de trabajo
con un id estable único — la forma que quieren `WorkManager` de
Android y `URLSession.background` de iOS:

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

Registralo:

```rust
istmo::runtime!(
    workers: [
        ImageUploadWorker => ImageUploadWorkerImpl,
    ],
);
```

El `ImageUploadWorkerClient` generado expone un método
`.schedule(unique_name, input, config)` que entrega el request al job
scheduler de la plataforma. `WorkManager` en Android va a:

- Persistir el job a través de muerte de proceso.
- Reintentar con backoff en fallo transiente.
- De-duplicar por `unique_name` (job existente con el mismo nombre o
  reemplaza o se mantiene, dependiendo de `WorkPolicy`).

## Shutdown cooperativo

Ambos `Context`s de service y worker exponen un `CancelToken`.
Chequealo durante tu loop:

```rust
if ctx.cancel().is_cancelled() { return; }
tokio::select! {
    _ = ctx.stopped()          => { /* stop signal */ }
    _ = ctx.cancel().cancelled() => { /* platform cancel */ }
    result = self.do_work()      => { /* work done */ }
}
```

Ver [Cancelación](/istmo/es/writing-plugins/cancellation/) para los detalles.

## Siguiente

- Bridge de proceso `:remote`: [Avanzado → Proceso remoto](/istmo/es/advanced/remote-process/).
- Deployment de desktop: [Avanzado → Deployment de desktop](/istmo/es/advanced/desktop-deployment/).
