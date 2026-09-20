---
title: Protocolo de Frames
description: El único formato de wire bincode-encoded que fluye a través de JNI y FFI.
sidebar:
  order: 5
---

Todo lo que cruza entre Rust y el lado nativo de una app Istmo es un
**frame** envuelto en un **envelope**. Existe solo un formato, un codec
(`bincode 2`), y un byte de versión. Rust y Kotlin/Swift nunca encodean
envelopes a mano — se pasan payloads tipados y el runtime hace el
packing.

No necesitas leer esta página para escribir un plugin. Guárdala como
referencia si estás debuggeando un problema del wire o escribiendo un
backend de transport.

## Envelope

```rust
pub struct Envelope {
    pub version: u8,        // PROTOCOL_VERSION, actualmente 4
    pub frame: Frame,
}
```

El byte de versión permite a los receivers rechazar envelopes de un
build mismatched. Cada binario runtime conoce las versiones que puede
decodear.

## Variantes de Frame

| Variante                    | Dirección        | Propósito                                                       |
| --------------------------- | ---------------- | --------------------------------------------------------------- |
| `Call { call_id, ... }`     | Rust → nativo    | Invoca un método de plugin con una tupla de args serializada    |
| `Respond { call_id, ... }`  | nativo → Rust    | Retorna un `Ok(payload)` o `Err(payload)` para un Call          |
| `Event { stream_id, ... }`  | nativo → Rust    | Un item en un stream                                            |
| `StreamEnd { stream_id, kind }` | nativo → Rust | Termina un stream (`Complete`, `Cancelled`)                    |
| `Cancel { call_id }`        | Rust → nativo    | Cancel cooperativo de un Call o su stream downstream            |
| `EarlyEvent { channel, kind, payload }` | nativo → Rust | Estado publicado antes que Rust suscribiera             |
| `ReleaseNativeHandle { handle_id }` | Rust → nativo | Libera un ID de recurso native-owned                        |
| `Notify { plugin_id, ... }` | Rust → nativo    | Call fire-and-forget, sin respuesta esperada                    |
| `CreateInstance / DestroyInstance` | Rust → nativo | Plugins stateful que requieren config de init                |

## Identificadores

- `call_id`, `stream_id`, `instance_id`, `handle_id` son todos `u64`
  monotónicamente crecientes asignados en el runtime.
- `plugin_id` es el string que declaraste en `istmo.toml`.
- `channel` para `EarlyEvent` es un string domain-specific propiedad
  del plugin (típicamente el `id` del plugin más un nombre de
  sub-channel).

## Historial de versiones

| Versión | Cambio                                                            |
| ------- | ----------------------------------------------------------------- |
| 1       | Inicial: Call/Respond/Event/StreamEnd/Cancel                      |
| 2       | Añade `EarlyEvent { channel, kind, payload }`                     |
| 3       | Añade `ReleaseNativeHandle { handle_id }`                         |
| 4       | Añade `Notify { plugin_id, instance_id, method, payload }`        |

El protocolo es aditivo-solamente. Payloads viejos se decodean en
runtimes nuevos, pero variantes nuevas se rechazan en runtimes
viejos. Bumpea `PROTOCOL_VERSION` en `istmo-core` cuando extiendas el
enum.

## Dónde está la frontera

- **JNI (Android):** `IstmoRuntime.onCall(payload: ByteArray)` /
  `nativeSubmitCall(bytes)`. Solo los bytes de payload interno cruzan
  — Kotlin nunca encoda un `Envelope`.
- **FFI (iOS):** `istmo_ios_submit_call(payload, len)` y una tabla
  callback registrada en `istmo_ios_start` — misma regla, solo
  payload.

Todo lo demás — envelope wrapping, version stamping, serialización de
frames — pasa del lado Rust. Un wrapper tipado en cada plataforma
(`nativeSubmitEarlyLatest`, `istmo_ios_submit_notify`) expone las
variantes que tu app efectivamente usa.

## Debugging

Habilita `RUST_LOG=istmo_core=trace` para ver cada frame que el
runtime emite y recibe. Las funciones `Envelope::to_wire_bytes` /
`Envelope::from_wire_bytes` son públicas en `istmo-core`, así puedes
round-trip un frame en un unit test para verificar un cambio.

## Siguiente

- Ver cómo los services Rust streamean: [Streams](/istmo/es/writing-plugins/streams/).
- Ver cómo los handles nativos hacen roundtrip: [Native handles](/istmo/es/writing-plugins/native-handles/).
- Bridging cross-process: [Proceso remoto](/istmo/es/advanced/remote-process/).
