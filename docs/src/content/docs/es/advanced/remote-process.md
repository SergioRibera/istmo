---
title: Bridge de proceso remoto
description: Corre el backend de un plugin en un proceso OS separado en Android.
sidebar:
  order: 1
---

El runtime Android soporta el patrón clásico de proceso `:remote` —
donde el backend de un plugin corre en un **proceso OS distinto** del
UI. Útil cuando:

- Los crashes del SDK del plugin son frecuentes y quieres que no
  tumben el proceso UI.
- El plugin sostiene un footprint de memoria residente grande (un
  modelo ML, un pipeline de media) que sobrevive a los estados Trim
  de tu UI.
- Quieres muerte de proceso independiente para protección OOM.

## Forma del wire

Cada proceso corre su propio `Runtime`. Un **bridge** — una interfaz
Binder AIDL — shuttlea bytes de `Envelope` de ida y vuelta. Cada frame
outbound del proceso UI cuyo plugin id sea declarado "remote" es
serializado, enviado sobre Binder, e inyectado en el runtime del
proceso `:remote` via `Runtime::inject_wire_envelope(bytes)`.

El lado Rust hace la clasificación; Kotlin sólo forwardea bytes.

```
   Proceso UI                    Proceso :remote
   ┌──────────────┐  Envelope     ┌──────────────┐
   │  Runtime     │──── bytes ───►│  Runtime     │
   │              │◄── bytes ─────│              │
   │  send_out    │               │  inject_wire │
   │  → matches   │               │  → dispatch  │
   │   remote id? │               │              │
   └──────────────┘               └──────────────┘
        │  sí → RemoteEnvelopeSink
        ▼
     Kotlin Binder shuttle
```

## Declarar qué plugins son remotos

Dos palancas:

1. **Default del plugin** — su propio `istmo.toml`:
   ```toml
   [plugin]
   default_deployment = "remote"
   ```
2. **Override de app** — aunque el plugin sea local por default, la
   app puede forzarlo remoto:
   ```toml
   # istmo.toml (en la raíz del crate app)
   [[remote_override]]
   plugin     = "acme.large_ml_model"
   deployment = "remote"
   ```

`emit_wiring_env()` (invocado por `istmo_build::emit()`) las lee y
emite env vars `ISTMO_AUTO_REMOTE` que consume la macro
`istmo::runtime!`.

## Wiring del lado Rust

```rust
istmo::runtime!(
    plugins:  [SignInClient, DataStoreClient],
    remote:   [LargeModelClient],
);
```

La macro:

- Registra los plugins locales normalmente.
- Marca el plugin id de `LargeModelClient` como remoto vía
  `Runtime::declare_remote_plugin`.
- Instala un `RemoteEnvelopeSink` closure (`Arc<dyn Fn(Vec<u8>)>`) —
  la app debe proveer esto en runtime con
  `install_remote_envelope_sink`, pasando el bridge Kotlin como
  destino.

Típicamente proveés el sink así:

```rust
istmo_runtime.install_remote_envelope_sink(Arc::new(|bytes| {
    // implementación del lado Kotlin forwardea al Binder
    unsafe { native_forward_to_bridge(bytes.as_ptr(), bytes.len()) }
}));
```

## Bridge del lado Kotlin

`IstmoRuntime` en ambos procesos expone:

- **`onRemoteEnvelope(bytes: ByteArray)`** — llamado por JNI cuando
  Rust emite bytes outbound para el proceso remoto.
- **`nativeInjectEnvelope(bytes: ByteArray)`** — llamado del lado
  receive para entregarle bytes inbound al runtime.

Junto con una interfaz AIDL two-way, forman el shuttle:

```aidl
// IIstmoBridge.aidl
interface IIstmoBridge {
    void submitEnvelope(in byte[] bytes);
}
```

Cablea el `IBinder` en ambos procesos, conecta
`onRemoteEnvelope` → `remote.submitEnvelope`, y `submitEnvelope` al
recibir → `nativeInjectEnvelope`. El lado Rust de cada runtime maneja
el resto.

## Qué cruza el bridge

Cada variante de frame es segura de cruzar:

- `Call` / `CreateInstance` / `Notify` — ruteados por plugin id +
  call id.
- `Respond` / `Cancel` — ruteados por call id (grabado via
  `mark_call_remote` en Call inbound).
- `Event` / `StreamEnd` — ruteados por stream id.
- `DestroyInstance` — ruteado por instance id.
- `EarlyEvent` / `ReleaseNativeHandle` — el lado receive
  cortocircuita al handler apropiado.

Sin casos especiales por variante-de-frame en Kotlin — sólo forwardea
bytes.

## Testing

Las primitivas Rust están cubiertas por unit tests sin emulador:

- `remote_bridge.rs` — levanta dos `Runtime`s, cablea un sink
  in-memory, verifica que los frames rutean correctamente.
- `mark_call_remote_routes_hosted_respond_through_sink` — verifica
  que las responses vuelven por donde vinieron.

End-to-end completo (un `:remote` service Android real) aterriza con
el primer consumer shipping; las primitivas son estables.

## Cuándo no llegar a esto

- Plugins que mayormente bouncean off APIs del sistema (permisos,
  share sheet) — el overhead es real y no hay ahorro de memoria.
- Plugins muy frecuentes, low-latency — cada call ahora cuesta un
  hop Binder.
- Plugins cross-plataforma — la forma `:remote` es Android-only.
