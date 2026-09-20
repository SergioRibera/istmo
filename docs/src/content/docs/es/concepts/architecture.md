---
title: Arquitectura
description: Cómo Istmo conecta un cdylib de Rust con Kotlin y Swift usando un solo protocolo binario.
sidebar:
  order: 1
---

Una app Istmo tiene tres actores:

1. **Tu código Rust.** Lógica de negocio, estado, UI (si envías un
   framework Rust como `eframe`), traits de plugins.
2. **El `Runtime`.** Un objeto process-wide propiedad de `istmo-core`.
   Rutea calls, gestiona lifetimes de plugins, y serializa envelopes
   dentro y fuera del wire.
3. **El lado nativo.** Kotlin en Android, Swift en iOS. Presente
   sólo donde la plataforma lo demanda — permisos, SDKs nativos,
   ejecución en background.

Todo cruza entre (1) + (2) y (3) a través de **un solo protocolo
binario** — un `Envelope` `bincode`-encoded con length-prefix que
carga una variante `Frame`. Sin trampolines por plugin, sin JSON, sin
reflection.

## El Runtime

Instancias el runtime una vez con la macro `istmo::runtime!`:

```rust
istmo::runtime!(
    plugins: [SignInClient, BatteryClient],
    services: [SyncService],
    remote: [PushNotificationsClient],
);
```

La macro lee tres secciones opcionales:

- `plugins:` — clients que invocas directamente desde Rust.
- `services:` — servicios de larga duración que el runtime debe poseer.
- `remote:` — plugins cuyo backend vive en un **proceso OS separado**
  (patrón `:remote` de Android).

En compile time produce `Runtime::new()` retornando un
`Arc<Runtime>`. Desde ahí cada plugin client obtiene el runtime con
`SignInClient::from_runtime(&runtime)`.

## El protocolo de Frames

Un `Frame` es la unidad de todo: calls, respuestas, events, cancels,
releases de handles. Se define en `istmo-core` y se serializa con
`bincode 2`. La versión de protocolo actual es **4** (ver [Frame
protocol](/istmo/es/concepts/frame-protocol/) para el historial).

Variantes con las que te vas a encontrar:

| Variante              | Dirección        | Significado                              |
| --------------------- | ---------------- | ---------------------------------------- |
| `Call`                | Rust → nativo    | Invocar un método de plugin              |
| `Respond`             | nativo → Rust    | Resultado de un Call                     |
| `Event`               | nativo → Rust    | Un item de stream                        |
| `StreamEnd`           | nativo → Rust    | Terminator de stream                     |
| `Cancel`              | Rust → nativo    | Cancel cooperativo de Call o stream      |
| `EarlyEvent`          | nativo → Rust    | Estado publicado antes que Rust suscriba |
| `ReleaseNativeHandle` | Rust → nativo    | Objeto native-owned debe ser liberado    |
| `Notify`              | Rust → nativo    | Call fire-and-forget                     |

Rust nunca encoda un `Envelope` a mano y tampoco Kotlin/Swift. Wrappers
FFI tipados en cada lado construyen el frame del lado Rust, por lo que
las fronteras JNI y FFI solo cargan los bytes de payload interno.

## Dónde viven los plugins

Cada plugin es un **crate Rust** que:

- Declara la forma del trait con `#[istmo::plugin]`.
- Publica un `Contract` describiendo esa forma en build time (vía
  `istmo_build::emit()`).
- Opcionalmente envía templates de dispatcher Kotlin/Swift que la app
  regenera en cada compilación.

Existen dos modelos de hosting:

- [**Rust-hosted**](/istmo/es/concepts/native-vs-rust-hosted/#rust-hosted) —
  Rust implementa el trait. Kotlin/Swift llaman *hacia adentro*
  (típico para plugins de computación pura).
- [**Native-hosted**](/istmo/es/concepts/native-vs-rust-hosted/#native-hosted)
  — Kotlin/Swift implementan el trait. Rust llama *hacia afuera*
  (típico para plugins de integración con la plataforma).

Ambos lucen idénticos desde Rust — siempre sostienes un `<T>Client`.
La única diferencia es dónde físicamente corre el código que atiende
el call.

## Handover de metadata entre crates

Un plugin Istmo no importa un schema desde su consumer ni viceversa.
Contratos y deps nativas viajan por el mecanismo de env vars
`DEP_<links>_*` de Cargo:

1. `build.rs` del plugin invoca `istmo_build::emit()`.
2. Eso emite `DEP_<plugin>_ISTMO_CONTRACT` y
   `DEP_<plugin>_NATIVE_DEPS` (coordenadas Gradle + SwiftPM).
3. `build.rs` de la app invoca `istmo_build::emit()`.
4. Eso camina cada `DEP_*_ISTMO_CONTRACT`, genera dispatchers Kotlin y
   Swift para cada uno, y los escribe en `android/` e `ios/`.

Todo es determinístico y repetible — regenerar produce los mismos
bytes.

## Threading & executors

Istmo es **agnóstico de executor**. Usa channels `flume` y
`pollster::block_on` internamente, entonces:

- Tu app puede parearlo con `tokio`, `smol`, `async-std`, `pollster`,
  o un loop hand-rolled. Nada en `istmo-core` te encasilla.
- Cada método `<T>Client` retorna `impl Future`. Los streams retornan
  `impl Stream + Unpin` que puedes drivear desde cualquier lado.
- El lado nativo tiene su propio thread OS dedicado por plataforma —
  un daemon JNI en Android, un `Thread` en iOS — que bombea frames de
  salida hacia el runloop de la plataforma.

## Siguiente

- Zoom al split: [Native vs. Rust-hosted](/istmo/es/concepts/native-vs-rust-hosted/).
- Entiende los contratos: [Contratos](/istmo/es/concepts/contracts/).
- Ve los tipos del wire: [Frame protocol](/istmo/es/concepts/frame-protocol/).
