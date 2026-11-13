---
title: Servicios y workers
description: Corre código de background de larga vida sin convertir tu plugin en un service manager.
sidebar:
  order: 6
---

Dos adapters extienden las macros de plugin para cubrir las dos formas
de larga duración más comunes:

- `#[istmo::service]` — una tarea de background siempre activa que
  maneja su propio ciclo de vida (`on_start`, `on_stop`) y se puede
  detener desde cualquiera de los dos lados.
- `#[istmo::worker]` — una unidad de trabajo única identificada por un
  task id y un nombre único (el modelo de `WorkManager` de Android).

Ambos son Rust-hosted: se conectan al mismo `Runtime` que los plugins
normales y viajan por el mismo protocolo de frames. Ambos aceptan los
atributos `name = "…"` (obligatorio, el id del wire), `crate = "…"`
(path del runtime, por defecto `::istmo`) y `plugins = "…"` (path a los
tipos de plugins first-party, por defecto `<crate>::plugins`; un crate
que pone `crate = "::istmo_core"` pone también
`plugins = "::istmo_plugins"`).

## Servicios

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
            // espera la siguiente ronda — o `ctx.stopped()` — con el
            // timer y el `select` de tu executor
        }
        Ok(())
    }

    async fn on_stop(&self) {
        self.flush().await;
    }
}
```

Reglas que la macro aplica:

- El trait declara solo `on_start` y, opcionalmente, `on_stop`;
  cualquier otro método es un error.
- `async fn on_start(&self, ctx: ServiceContext)` retorna `()` o
  `Result<(), E>`. `async fn on_stop(&self)` no recibe otro argumento.
- `async fn` planas; la macro las desazucara, sin `#[async_trait]`.

`ServiceContext` es lo que el servicio ve de su host:

| Método | Propósito |
|---|---|
| `stopped().await` / `is_stopped()` | Espera / consulta la señal de stop (pedido de stop, cancelación, host desaparecido). |
| `cancel_token()` | El `CancelToken` de la ejecución. |
| `service_id()`, `runtime()` | El id y el runtime del servicio. |
| `set_foreground(spec)`, `update_notification(spec)`, `stop_foreground(remove)` | Notificación de foreground (`NotificationSpec`). |
| `acquire_wakelock(tag)` | Un `WakeLock`, liberado con `release().await` o al soltarlo. |
| `stop_self()` | Pide a la plataforma que detenga el servicio. |

Las llamadas a la plataforma pasan por el plugin
`istmo.service_control` (`ServiceControl` en `istmo::plugins`). El
runtime no trae backend nativo para él: genera su dispatcher pasando
`istmo_plugins_schema::service_control()` en
`AppOpts::extra_contracts` e impleméntalo en Kotlin / Swift.

Registra el servicio con `istmo::runtime!`:

```rust
istmo::runtime!(
    services: [SyncService => SyncImpl],
);
```

La macro registra `SyncServiceAdapter::new(SyncImpl)`, así que
`<Trait>Adapter` tiene que resolverse en ese path. El adapter corre
`on_start` en su propio thread del sistema (`pollster::block_on`), así
que nunca bloquea a un caller async; un segundo `on_start` detiene y
espera (join) la ejecución anterior primero, y `on_stop` o un
`Frame::Cancel` lo detienen a través del contexto.

### Android — shim de `Service`

Para correr el servicio como un `Service` de Android (reproducción de
media, seguimiento de ubicación, sincronización), declara el shim en el
`istmo.toml` del crate que define el servicio:

```toml
[plugin]
id = "myapp.sync"   # igual que #[istmo::service(name = "…")]

[plugin.android_service]
class_name = "SyncForegroundService"
foreground_service_type = "dataSync"          # opcional
exported = false                              # por defecto false
permission = "android.permission.FOREGROUND_SERVICE_DATA_SYNC" # opcional
process = ":sync"                             # opcional
```

Deja fuera `client_type`: la macro de servicio no emite cliente. Una
entrada de plugin sin `client_type` siempre aporta sus shims; una con
`client_type` los aporta solo cuando la app conecta ese cliente: basta
con que se auto-conecte (una dependencia directa, ver
[Auto-wiring](/istmo/es/advanced/auto-wiring/)) o con nombrarlo en las
secciones `plugins:` / `remote:` de `istmo::runtime!`.

El `build.rs` de la app (`istmo_build::emit()`) toma la spec del
handover de manifests y escribe:

- `android/app/src/main/java/<paquete de la app>/SyncForegroundService.kt`
  — un `LifecycleService` (así que la app necesita
  `androidx.lifecycle:lifecycle-service`) que carga la librería Rust de
  la app, arranca el runtime y reenvía `onStartCommand` / `onDestroy` a
  `on_start` / `on_stop` del adapter bajo el mismo id de plugin. El
  paquete es el paquete Kotlin de la app (`<namespace>.gen` o
  `[app] android_package`); el nombre de la librería es el `[lib] name`
  del crate, o su nombre de paquete con `-` reemplazado por `_`.
- `android/app/src/main/AndroidManifest.services.xml` — el elemento
  `<service …/>` de cada servicio declarado.

Si tu `android/app/src/main/AndroidManifest.xml` contiene los
marcadores

```xml
<application>
    …
    <!-- istmo:services:start -->
    <!-- istmo:services:end -->
</application>
```

el build parchea el bloque entre ellos en cada build, así que nunca
vuelves a tocar el XML (el manifest del template los trae). Sin los
marcadores, copia el contenido del sidecar una vez a mano.

### iOS — `BGTaskScheduler`

Declara la tarea de background en el mismo `istmo.toml`:

```toml
[plugin.ios_background]
class_name = "SyncBackgroundHandler"
task_identifier = "com.myapp.sync.refresh"
kind = "refresh"           # o "processing" | "continuous"
interval_minutes = 15      # obligatorio para `refresh`
# requires_power = true    # para `processing` (por defecto false)
# requires_network = true  # para `processing` (por defecto false)
# continuous_mode = "audio"  # obligatorio para `continuous`: audio | location | voip |
#                            # external_accessory | bluetooth_central | bluetooth_peripheral
```

El build escribe:

- `ios/<AppDir>/Plugins/Background/SyncBackgroundHandler.swift` — un
  enum con `register()` (registra el handler en `BGTaskScheduler`) y
  `schedule()` (envía el siguiente pedido). El handler llama a
  `on_start` del adapter y lo cancela desde el `expirationHandler` de
  la tarea. Llama a `SyncBackgroundHandler.register()` una vez al
  arrancar, antes de que la app termine de lanzarse, y a `schedule()`
  para encolar la primera ejecución.
- `ios/<AppDir>/Info.plist.background.xml` — las entradas
  `BGTaskSchedulerPermittedIdentifiers` (refresh / processing) o
  `UIBackgroundModes` (continuous).

Con `<!-- istmo:background:start -->` / `<!-- istmo:background:end -->`
dentro del `<dict>` de tu `Info.plist` (el del template los trae), el
build parchea esas entradas en cada build. Ver la
[página de despliegue en desktop](/istmo/es/advanced/desktop-deployment/)
para el lado desktop.

## Workers

Usa `#[istmo::worker]` para una unidad de trabajo **discreta**:

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

El trait declara exactamente `async fn run(&self, ctx: WorkerContext)`,
que retorna `TaskOutcome` (`Success`, `Retry`, `Failure`) o
`Result<TaskOutcome, E>`. `WorkerContext` trae `task_id()`,
`unique_name()`, los bytes crudos de `input()` (`into_input()` para
tomarlos), `is_cancelled()` / `cancel_token()` y `runtime()`.

Regístralo:

```rust
istmo::runtime!(
    workers: [Backup => Nightly],
);
```

La macro registra `BackupWorkerAdapter::new(Nightly)`, que responde al
método `run` del plugin `myapp.backup` con un payload
`(task_id, unique_name, input)`.

Programar la ejecución es trabajo de la plataforma.
`istmo::plugins::TaskSchedulerClient` (`istmo.task_scheduler`) declara
la API — `enqueue(TaskRequest)` con `Constraints`, un retraso inicial,
tags y una `ExistingWorkPolicy`, más `cancel`, `cancel_by_tag` y
`cancel_by_unique_name` — pero el runtime no trae backend nativo:
genera su dispatcher pasando `istmo_plugins_schema::task_scheduler()`
en `AppOpts::extra_contracts` e impleméntalo con `WorkManager` /
`BGTaskScheduler`. En Android,
`istmo_build::generate_android_worker(&WorkerContract::new(..))`
retorna el código de un `CoroutineWorker` que llama a `run` del worker
y mapea el `TaskOutcome` a `Result.success()` / `retry()` /
`failure()`; escríbelo desde tu `build.rs`.

## Apagado cooperativo

Ambos contextos exponen el `CancelToken` de la ejecución. Consúltalo en
un loop ocupado y espéralo cuando estés ocioso:

```rust
// servicio
if ctx.is_stopped() { return Ok(()); }
ctx.stopped().await;

// worker
if ctx.is_cancelled() { return TaskOutcome::Retry; }
ctx.cancel_token().cancelled().await;
```

Ver [Cancelación](/istmo/es/writing-plugins/cancellation/) para los
detalles.

## Siguiente

- Puente de proceso `:remote`: [Avanzado → Proceso remoto](/istmo/es/advanced/remote-process/).
- Despliegue en desktop: [Avanzado → Despliegue en desktop](/istmo/es/advanced/desktop-deployment/).
