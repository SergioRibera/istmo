---
title: Protocolo de Frames
description: El único formato de wire que fluye a través de JNI y FFI.
sidebar:
  order: 5
---

Todo lo que cruza entre Rust y el lado nativo de una app Istmo es un
**frame** envuelto en un **envelope**. Existe un solo formato, un solo
codec (wincode, con el layout `standard()` de bincode 2: enteros varint,
little endian) y una sola versión de protocolo. Ni Kotlin ni Swift
codifican envelopes — le pasan a Rust payloads tipados y el runtime hace
el empaquetado.

No necesitas leer esta página para escribir un plugin. Guárdala como
referencia si estás depurando un problema del wire o escribiendo un
backend de transporte.

## Envelope

```rust
pub struct Envelope {
    pub version: u16,   // PROTOCOL_VERSION, actualmente 4
    pub frame: Frame,
}
```

`Envelope::from_wire_bytes` rechaza un envelope cuyo `version` difiere
del `PROTOCOL_VERSION` del receptor con
`IstmoError::ProtocolVersionMismatch`, así que dos builds con versiones
de protocolo distintas nunca se malinterpretan.

## Variantes de Frame

| Variante                                             | La envía       | Propósito                                                    |
| ---------------------------------------------------- | -------------- | ------------------------------------------------------------ |
| `Call { call_id, plugin_id, instance_id, method, payload }` | quien llama | Invoca un método del plugin con una tupla de args serializada |
| `Respond { call_id, result }`                        | el host        | `Ok(payload)` o `Err(payload)` para un `Call`                |
| `Cancel { call_id }`                                 | quien llama    | Cancelación cooperativa de una llamada o su stream           |
| `Event { stream_id, payload }`                       | el host        | Un item de un stream                                         |
| `StreamEnd { stream_id, reason }`                    | el host        | Cierra un stream: `Complete`, `Cancelled` o `Error(payload)` |
| `CreateInstance { call_id, plugin_id, payload }`     | quien llama    | Construye una instancia de un plugin con estado desde su config |
| `DestroyInstance { instance_id }`                    | quien llama    | Libera una instancia de un plugin con estado                 |
| `EarlyEvent { channel, kind, payload }`              | el lado nativo | Estado publicado antes de que Rust se suscribiera            |
| `ReleaseNativeHandle { handle_id }`                  | Rust           | Libera el id de un recurso propiedad del nativo              |
| `Notify { plugin_id, instance_id, method, payload }` | quien llama    | Llamada de una sola vía, sin respuesta esperada              |

Quien llama y el host pueden estar en cualquier lado: Rust llama en un
plugin native-hosted, Kotlin/Swift en uno Rust-hosted. Un stream reusa
el id de la llamada como su `stream_id`.

`EarlyEvent` lleva una política de retención, `kind`: `Latest` guarda
sólo el valor más reciente, `Queue { capacity }` guarda una FIFO acotada
que se vacía en el primer suscriptor.

## Identificadores

- `call_id`, `stream_id` e `instance_id` son newtypes de `u64` asignados
  por el lado que inicia la llamada.
- `handle_id` (`NativeHandleId`) lo asigna quien es dueño del objeto:
  los runtimes de Kotlin y Swift cuentan desde `1`, los hosts del lado
  Rust desde `1 << 63` (`Runtime::alloc_native_handle_id`), así que los
  dos rangos nunca chocan.
- `plugin_id` es el id del plugin: el `name` de su trait
  `#[istmo::plugin(name = "…")]`, que su `istmo.toml` repite como
  `[plugin] id`.
- `channel` en `EarlyEvent` es un string propiedad del plugin
  (típicamente el id del plugin más un nombre de subcanal).

## Historial de versiones

| Versión | Cambio                                                               |
| ------- | -------------------------------------------------------------------- |
| 1       | Conjunto inicial de frames                                           |
| 2       | Se agregó `EarlyEvent { channel, kind, payload }`                    |
| 3       | Se agregó `ReleaseNativeHandle { handle_id }`                        |
| 4       | Se agregó `Notify { plugin_id, instance_id, method, payload }`       |

`PROTOCOL_VERSION` sube cada vez que se agrega una variante de `Frame` o
una cambia de forma. Como la versión debe coincidir exactamente, la
librería Rust y el runtime Kotlin/Swift de una app siempre vienen del
mismo release.

## Dónde está la frontera

- **JNI (Android):** los frames entrantes llegan a Rust por funciones
  `external` tipadas de `dev.istmo.runtime.IstmoRuntime` —
  `nativeSubmitCall`, `nativeSubmitResponse`, `nativeSubmitEvent`,
  `nativeSubmitStreamEnd`, `nativeSubmitCancel`, `nativeSubmitNotify`,
  `nativeSubmitEarlyLatest`, `nativeSubmitEarlyQueue`. Los salientes
  llegan a Kotlin por callbacks estáticos del mismo objeto: `onCall`,
  `onRespond`, `onEvent`, `onStreamEnd`, `onCancel`, `onNotify`,
  `onCreateInstance`, `onDestroyInstance`, `onReleaseNativeHandle`.
- **FFI (iOS):** las funciones C declaradas en `istmo_ffi.h`
  (`istmo_ios_submit_call`, `istmo_ios_submit_response`,
  `istmo_ios_submit_notify`, …) reciben los campos del frame como
  argumentos, y los frames salientes llegan por la tabla
  `IstmoIosCallbacks` que se pasa a `istmo_ios_start`.

Cada argumento es un id, un string o un buffer de bytes de payload. El
envoltorio en envelope, el sello de versión y la serialización del frame
ocurren del lado de Rust.

## Depuración

`Envelope::to_wire_bytes` / `Envelope::from_wire_bytes` son públicas en
`istmo-core`, así que puedes hacer round-trip de un frame en un test
unitario para verificar un cambio. `Runtime::dispatch_inbound` y
`Runtime::inject_wire_envelope` meten un envelope decodificado o en
bytes directamente en un runtime, y el receiver `outbound` que devuelve
`Runtime::init` muestra cada frame que envía.

## Siguiente

- Mira cómo hacen stream los servicios Rust: [Streams](/istmo/es/writing-plugins/streams/).
- Mira cómo hacen round-trip los handles nativos: [Native handles](/istmo/es/writing-plugins/native-handles/).
- Puente entre procesos: [Proceso remoto](/istmo/es/advanced/remote-process/).
