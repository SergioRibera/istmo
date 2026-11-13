---
title: Arquitectura
description: Cómo Istmo conecta un cdylib de Rust con Kotlin y Swift usando un solo protocolo binario.
sidebar:
  order: 1
---

Una app Istmo tiene tres actores:

1. **Tu código Rust.** Lógica de negocio, estado, UI (si usas un
   framework de UI en Rust como `eframe`), traits de plugins.
2. **El `Runtime`.** Un único objeto por proceso, propiedad de
   `istmo-core`. Enruta llamadas, gestiona el ciclo de vida de los
   plugins y serializa los envelopes que entran y salen del wire.
3. **El lado nativo.** Kotlin en Android, Swift en iOS. Presente sólo
   donde la plataforma lo exige — permisos, SDKs nativos, ejecución en
   segundo plano.

Todo lo que cruza entre (1) + (2) y (3) pasa por **un solo protocolo
binario** — un `Envelope` codificado con wincode que lleva una variante
de `Frame`. Sin trampolines por plugin, sin JSON, sin reflection.

## El Runtime

Declaras el runtime una vez, a nivel de módulo en el crate de la app,
con la macro `istmo::runtime!`:

```rust
istmo::runtime!(
    plugins: [SignInClient, BatteryClient],
    remote: [PushNotifications],
    hosts: [ImageHasher => ImageHasherImpl],
    services: [SyncService => Syncer],
    workers: [Backup => NightlyBackup],
);
```

Todas las secciones son opcionales:

- `plugins:` — clients que llamarás desde Rust (paths a tipos
  `<Trait>Client`).
- `remote:` — plugins cuyo backend vive en **otro proceso del SO**
  (patrón `:remote` de Android). Cada entrada es el trait del plugin o
  su `<Trait>Client`.
- `hosts:` — implementaciones Rust de traits de plugins, escritas
  `Trait => expr` y registradas como `<Trait>Host::new(expr)`.
- `services:` / `workers:` — implementaciones de `#[istmo::service]` /
  `#[istmo::worker]`, registradas como `<Trait>Adapter::new(expr)` /
  `<Trait>WorkerAdapter::new(expr)`.

La macro se expande a `__istmo_configure_runtime(init: RuntimeInit) ->
RuntimeInit`, que los transportes de Android e iOS llaman cuando la
plataforma arranca el runtime. Los plugins que `istmo_build::emit()`
encuentra en tus dependencias se suman encima
([Auto-wiring](/istmo/es/advanced/auto-wiring/)). Un binario de
escritorio construye su runtime él mismo con
`Runtime::init(RuntimeConfig::inline())` y registra sus hosts en el
runtime devuelto.

A partir de ahí, un client de plugin se enlaza a un runtime con
`SignInClient::from_runtime(&runtime)`, o al runtime global del proceso
con `SignInClient::acquire()`.

## El protocolo de Frames

Un `Frame` es la unidad de todo: llamadas, respuestas, eventos,
cancelaciones, liberación de handles. Está definido en `istmo-core` y
se serializa con wincode en el layout `standard()` de bincode 2. La
versión actual del protocolo es **4** (ver
[Protocolo de frames](/istmo/es/concepts/frame-protocol/) para el
historial).

| Variante              | La envía          | Significado                                     |
| --------------------- | ----------------- | ----------------------------------------------- |
| `Call`                | quien llama       | Invoca un método del plugin                     |
| `Respond`             | el host           | Resultado de un `Call`                          |
| `Event`               | el host           | Un item de un stream                            |
| `StreamEnd`           | el host           | Cierre de un stream                             |
| `Cancel`              | quien llama       | Cancelación cooperativa de una llamada o stream |
| `CreateInstance`      | quien llama       | Construye una instancia de un plugin con estado |
| `DestroyInstance`     | quien llama       | Libera una instancia de un plugin con estado    |
| `EarlyEvent`          | el lado nativo    | Estado publicado antes de que Rust lo pidiera   |
| `ReleaseNativeHandle` | Rust              | Hay que liberar un objeto propiedad del nativo  |
| `Notify`              | quien llama       | Llamada de una sola vía, sin respuesta          |

Quien llama y el host pueden estar en cualquier lado: Rust llama a
Kotlin/Swift en un plugin native-hosted, Kotlin/Swift llaman a Rust en
uno Rust-hosted.

Ni Kotlin ni Swift codifican un `Envelope`. Wrappers FFI tipados en cada
lado construyen el frame del lado de Rust, así que las fronteras JNI y
FFI sólo transportan los bytes del payload interno.

## Dónde viven los plugins

Cada plugin es un **crate de Rust** que:

- Declara la forma del trait con `#[istmo::plugin]`.
- Publica un `Contract` que describe esa forma en tiempo de build (vía
  `istmo_build::emit()`).
- Opcionalmente trae backends nativos en `native/android/` y
  `native/ios/`, que la app compila.

El build de la app regenera los dispatchers, tipos y codecs Kotlin/Swift
de cada contrato en cada compilación.

Hay dos modelos de hosting:

- [**Rust-hosted**](/istmo/es/concepts/native-vs-rust-hosted/#rust-hosted) —
  Rust implementa el trait. Kotlin/Swift llaman *hacia* Rust (típico
  para plugins de cómputo puro).
- [**Native-hosted**](/istmo/es/concepts/native-vs-rust-hosted/#native-hosted) —
  Kotlin/Swift implementan el trait. Rust llama *hacia afuera* (típico
  para plugins de integración con la plataforma).

Ambos se ven idénticos desde Rust — siempre tienes un `<T>Client`. La
única diferencia es dónde corre físicamente el código que atiende la
llamada.

## Traspaso de metadatos entre crates

Un plugin Istmo no importa un schema de su consumidor ni al revés. Los
contratos y las dependencias nativas viajan por el mecanismo de env vars
`DEP_<links>_*` de los build scripts de Cargo:

1. El `build.rs` del plugin llama a `istmo_build::emit()`.
2. Eso emite `DEP_<links>_ISTMO_CONTRACTS`, `DEP_<links>_ISTMO_MANIFEST`
   y, cuando se declaran, `DEP_<links>_ISTMO_NATIVE_DEPS` (coordenadas
   Gradle + SwiftPM), `DEP_<links>_ISTMO_ASSETS` y
   `DEP_<links>_ISTMO_NATIVE_{ANDROID,IOS,MACOS}`.
3. El `build.rs` de la app llama a `istmo_build::emit()`.
4. Eso recorre cada `DEP_*_ISTMO_CONTRACTS`, genera dispatchers Kotlin y
   Swift para cada uno y los escribe en `android/` e `ios/`.

Los archivos generados sólo se reescriben cuando cambia su contenido,
así que recompilar con las mismas entradas no los toca.

## Threading y executors

Istmo es **agnóstico al executor**. Usa canales `flume` y
`pollster::block_on` internamente, así que:

- Tu app puede combinarlo con `tokio`, `smol`, `async-std`, `pollster`
  o un loop propio. Nada en `istmo-core` te ata.
- Cada método unario de `<T>Client` es un `async fn`. Los métodos stream
  devuelven un `TypedStream`, que consumes con `recv()` (bloqueante),
  `recv_async()` o `try_recv()` desde cualquier hilo o executor.
- Cada transporte móvil usa un hilo del SO dedicado — un daemon
  enganchado a JNI en Android (`istmo-android-pump`), `istmo-ios-pump`
  en iOS — que entrega los frames salientes a los callbacks de
  Kotlin/Swift.

## Siguiente

- Profundiza en la división: [Native vs. Rust-hosted](/istmo/es/concepts/native-vs-rust-hosted/).
- Entiende los contratos: [Contratos](/istmo/es/concepts/contracts/).
- Mira los tipos del wire: [Protocolo de frames](/istmo/es/concepts/frame-protocol/).
