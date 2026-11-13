---
title: Puente de proceso remoto
description: Ejecuta el backend de un plugin en un proceso del SO separado en Android.
sidebar:
  order: 1
---

El runtime soporta el patrón clásico de proceso `:remote` de Android —
donde el backend de un plugin corre en un **proceso del SO distinto** al
de la UI. Útil cuando:

- El SDK del plugin crashea seguido y no quieres que tumbe el proceso de
  la UI.
- El plugin ocupa mucha memoria residente (un modelo de ML, un pipeline
  de media) que quieres mantener aparte del proceso de la UI.
- Quieres muerte de procesos independiente como protección ante OOM.

## Forma del wire

Cada proceso corre su propio `Runtime`. Un **puente** — una interfaz
Binder AIDL — transporta los bytes de `Envelope` codificados en ambos
sentidos. Cada frame saliente de un plugin declarado remoto se
codifica, se entrega al `RemoteEnvelopeSink` del runtime, viaja por
Binder y se inyecta en el runtime del otro proceso con
`Runtime::inject_wire_envelope(bytes)`.

El lado Rust hace la clasificación; Kotlin sólo reenvía bytes.

```
   Proceso UI                     Proceso :remote
   ┌──────────────┐  Envelope     ┌──────────────┐
   │  Runtime     │──── bytes ───►│  Runtime     │
   │              │◄── bytes ─────│              │
   │  saliente    │               │  inject_wire │
   │  → ¿plugin   │               │  _envelope   │
   │   remoto?    │               │  → dispatch  │
   └──────────────┘               └──────────────┘
        │  sí → RemoteEnvelopeSink
        ▼
     Puente Binder en Kotlin
```

## Declarar qué plugins son remotos

Dos palancas alimentan el auto-wiring:

1. **Default del plugin** — el `istmo.toml` del propio plugin:
   ```toml
   [plugin]
   default_deployment = "remote"
   ```
2. **Override de la app** — aunque el plugin sea `local` por defecto,
   la app puede forzarlo a remoto (o de vuelta a local):
   ```toml
   # istmo.toml (en la raíz del crate de la app)
   [[remote_override]]
   plugin     = "acme.large_model"
   deployment = "remote"
   ```

`istmo_build::emit()` convierte esto en la lista `ISTMO_AUTO_REMOTE` que
consume la macro `istmo::runtime!`.

## Wiring del lado Rust

También puedes listar plugins remotos a mano:

```rust
istmo::runtime!(
    plugins: [SignInClient, DataStoreClient],
    remote:  [LargeModel],   // el trait, o `LargeModelClient`
);
```

Por cada entrada de `remote:` la configuración generada llama a
`RuntimeInit::remotes::<LargeModelClient>()`, es decir
`Runtime::declare_remote_plugin("acme.large_model")`. Desde ahí el
runtime manda los frames de ese plugin al `RemoteEnvelopeSink` instalado
(`Arc<dyn Fn(Vec<u8>) + Send + Sync>`) en lugar del transporte normal.

En Android no instalas el sink tú: `nativeStart` instala uno que pasa
los bytes a `IstmoRuntime.onRemoteEnvelope` en el hilo del pump. En
otros casos (escritorio, tests) instálalo con
`Runtime::install_remote_envelope_sink`:

```rust
use std::sync::Arc;

runtime.install_remote_envelope_sink(Arc::new(move |bytes: Vec<u8>| {
    // Pasa `bytes` a tu canal IPC; retorna rápido.
    let _ = to_peer.send(bytes);
}));
```

Sin sink, los frames de plugins remotos vuelven al canal saliente
normal.

## Puente del lado Kotlin

`dev.istmo.runtime.IstmoRuntime` expone:

- **`remoteEnvelopeSink: ((ByteArray) -> Unit)?`** — asígnale el proxy
  Binder; `onRemoteEnvelope(bytes)` le reenvía cada envelope remoto
  saliente (y lo descarta mientras no esté asignado).
- **`nativeInjectEnvelope(bytes: ByteArray)`** — llámalo del lado
  receptor para pasar al runtime los bytes del envelope entrante.

Junto con una interfaz AIDL de dos vías, forman el puente:

```aidl
// IIstmoBridge.aidl
package dev.istmo.remote;

interface IIstmoBridge {
    oneway void submitEnvelope(in byte[] envelope);
}
```

Ambos procesos se enlazan al servicio del otro, asignan
`IstmoRuntime.remoteEnvelopeSink` para reenviar a
`remote.submitEnvelope`, y llaman a `IstmoRuntime.nativeInjectEnvelope`
desde su propio `submitEnvelope`. Las fuentes de referencia para copiar
— el archivo AIDL y un par `IstmoRemoteBridgeService` /
`IstmoRemoteBridgeClient` — están en el repositorio en
`docs/remote-bridge/`. Cada proceso debe además declarar como remotos
los plugins que viven en el *otro* proceso.

## Qué cruza el puente

El clasificador enruta cada frame que pertenece a un plugin remoto:

- `Call` / `CreateInstance` / `Notify` — por id de plugin.
- `Cancel` — por el call id de una llamada remota saliente.
- `DestroyInstance` — por id de instancia (las instancias creadas con un
  `CreateInstance` remoto se registran solas).
- Del lado que hospeda, las llamadas que llegan por
  `inject_wire_envelope` se rastrean bajo su propio origen, así que sus
  frames `Respond`, `Event` y `StreamEnd` vuelven por el sink — sus ids
  pueden coincidir con ids que usa el transporte local sin chocar.

`EarlyEvent` y `ReleaseNativeHandle` nunca van al sink; pertenecen al
lado nativo del propio proceso. Kotlin no trata ninguna variante de
frame de forma especial — sólo reenvía bytes.

## Testing

Las primitivas de Rust están cubiertas por tests unitarios sin
emulador, en `crates/core/tests/remote_bridge.rs`:

- `app_to_remote_call_and_response_flow_over_bridge_bytes` — dos
  `Runtime` intercambiando bytes por canales en memoria.
- `declare_remote_plugin_routes_matching_outbound_through_sink` — los
  frames salientes de un plugin remoto llegan al sink.
- `mark_call_remote_routes_hosted_respond_through_sink` — las
  respuestas vuelven por donde vinieron.

Todavía no hay una app de ejemplo de punta a punta con un servicio
`:remote` real de Android.

## Cuándo no usarlo

- Plugins que casi sólo rebotan en APIs del sistema (permisos, share
  sheet) — el overhead es real y no ahorras memoria.
- Plugins muy frecuentes y de baja latencia — cada llamada ahora cuesta
  un salto de Binder.
- Plugins multiplataforma — la forma `:remote` es sólo de Android.
