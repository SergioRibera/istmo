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

### Android — wrapping como `Service` (manual hoy)

Si querés que el service sobreviva al proceso de la app (media
playback, tracking de ubicación), pareálo con un `ForegroundService`
Android. `istmo-build` expone un helper `generate_android_service`
que podés llamar desde el `build.rs` de tu plugin; **no está cableado
en `emit()` automáticamente hoy**, así que tenés que invocarlo vos:

```rust
// build.rs (en la raíz del crate plugin)
use istmo_build::{ServiceContract, generate_android_service};

fn main() {
    istmo_build::emit();

    let out = generate_android_service(&ServiceContract {
        plugin_id: "myapp.sync".into(),
        service_class: "com.myapp.SyncForegroundService".into(),
        // …channel de notification, foreground type, etc.
    });
    std::fs::write("android/SyncForegroundService.kt", out.source).unwrap();
    std::fs::write("android/AndroidManifest.snippet.xml", out.manifest).unwrap();
}
```

Copiá los archivos emitidos al tree
`android/app/src/main/` de tu app. Auto-wireando el scaffolding del
`Service` a través de `emit()` está en el roadmap — hasta que
aterrice, esta invocación manual es la forma.

### iOS — `BGTaskScheduler` (manual hoy)

La historia iOS es simétrica — `generate_ios_background` produce el
fragmento `Info.plist` + handler `AppDelegate` para
`BGAppRefreshTask` / `BGProcessingTask`. Misma historia: lo llamás vos
desde `build.rs`, `emit()` no lo cablea. Ver la [página de deployment
de desktop](/es/advanced/desktop-deployment/) para el módulo hermano.

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

Ver [Cancelación](/es/writing-plugins/cancellation/) para los detalles.

## Siguiente

- Bridge de proceso `:remote`: [Avanzado → Proceso remoto](/es/advanced/remote-process/).
- Deployment de desktop: [Avanzado → Deployment de desktop](/es/advanced/desktop-deployment/).
