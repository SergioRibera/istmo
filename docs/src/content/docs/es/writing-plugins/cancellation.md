---
title: Cancelación
description: Cancela cooperativamente llamadas en vuelo y streams de larga duración.
sidebar:
  order: 5
---

Istmo usa un modelo de cancelación **cooperativo**. Un caller señala
"por favor detente", y el callee chequea esa señal en sus propios
puntos de suspensión naturales. No hay terminación forzada.

## Cancelar desde el caller

No hay un método de cancelación que llamar: un caller Rust cancela
soltando (drop).

- **Llamadas unarias.** Un método del cliente es una `async fn`;
  soltar su future antes de que llegue la respuesta cancela la
  llamada. Envuélvela en el timeout o `select` que ofrezca tu executor,
  o suelta la tarea que la espera.
- **Streams.** Soltar el `TypedStream` lo cancela (ver
  [Streams](/istmo/es/writing-plugins/streams/#cancelación)).

Cuando un host en el mismo runtime sirve la llamada, el drop dispara
el `CancelToken` de esa llamada; si no, envía
`Frame::Cancel { call_id }` al peer (Kotlin, Swift o un proceso
remoto). Una llamada o stream que ya terminó no cancela nada.

```rust
let task = LongTaskClient::from_runtime(&rt)?;
let call = task.process(data);
// Soltar `call` (aquí: cuando gana el timeout) cancela el trabajo.
match with_timeout(Duration::from_secs(10), call).await {
    Some(result) => tracing::info!("listo: {:?}", result?),
    None => tracing::info!("me rendí"),
}
```

`with_timeout` representa el combinador de timeout de tu executor;
Istmo no trae uno.

## `CancelToken`

El lado del callee es `istmo::CancelToken` (re-exportado de
`istmo-core`): un flag compartido más un canal `flume` cuyo sender se
suelta al cancelar.

- `is_cancelled()` — poll síncrono, barato.
- `cancelled().await` — espera async, resuelve una vez cancelado el
  token.
- `cancel()` — lo dispara; los tokens son `Clone` y cada clon lo ve.

El runtime crea un token por llamada hosteada y lo pasa a
`Dispatch::dispatch`. Los adapters (`#[istmo::service]`,
`#[istmo::worker]`) lo integran en su contexto; los traits de plugin
lo activan por método.

## Activarlo desde un trait

Agrega un argumento cuyo último segmento de path de tipo sea
`CancelToken`. La macro lo quita de la tupla del wire y de la firma
del cliente generado, y lo llena con el token que provee el runtime:

```rust
use istmo::{CancelToken, plugin};

#[plugin(name = "acme.long_task")]
pub trait LongTask {
    async fn process(&self, data: Vec<u8>, cancel: CancelToken)
        -> Result<Output, TaskError>;
}
```

El `LongTaskClient::process` generado solo recibe `data`. La detección
es por nombre: `&CancelToken`, `Option<CancelToken>` o un alias con
otro nombre se tratan como argumentos del wire y no compilan, porque el
token no es un tipo del wire. El mismo argumento funciona en métodos
`#[stream]`.

## Chequear el token desde un impl

```rust
impl LongTask for LongTaskImpl {
    async fn process(
        &self,
        data: Vec<u8>,
        cancel: CancelToken,
    ) -> Result<Output, TaskError> {
        for chunk in data.chunks(64 * 1024) {
            if cancel.is_cancelled() {
                return Err(TaskError { reason: "cancelled".into() });
            }
            self.process_chunk(chunk).await?;
        }
        Ok(self.finalize())
    }
}
```

O espera la cancelación en paralelo con tu trabajo, con cualquier
`select` (aquí `futures::select_biased!`):

```rust
use futures::FutureExt as _;

futures::select_biased! {
    () = cancel.cancelled().fuse() => Err(TaskError { reason: "cancelled".into() }),
    result = do_the_thing().fuse() => result,
}
```

Una llamada cancelada no envía respuesta, así que lo que retorne el
impl después de una cancelación se descarta.

## Cancelar llamadas native-hosted

Ante un `Frame::Cancel` el runtime nativo cancela la coroutine de la
llamada (`Job.cancel()`) en Android o su `Task` (`Task.cancel()`) en
iOS. Reacciona con el mecanismo propio de la plataforma:

```kotlin
override suspend fun authenticate(prompt: AuthPrompt): AuthMethod =
    suspendCancellableCoroutine { cont ->
        cont.invokeOnCancellation { /* cierra el diálogo */ }
        // ...
    }
```

```swift
func authenticate(prompt: AuthPrompt) async throws -> AuthMethod {
    try await withTaskCancellationHandler {
        // ...
    } onCancel: { /* cierra el diálogo */ }
}
```

Un productor de stream Kotlin ve su `Flow` cancelado, así que corre el
`awaitClose` de `callbackFlow`:

```kotlin
override fun updates(): Flow<Fix> = callbackFlow {
    // ...
    awaitClose { /* corre al cancelar */ }
}
```

## Servicios y workers

Los adapters `#[istmo::service]` y `#[istmo::worker]` conectan el token
automáticamente:

- `ServiceContext::stopped().await` resuelve cuando se pide detener el
  servicio — por la vía `stop` o por `Frame::Cancel` — y
  `ServiceContext::is_stopped()` consulta lo mismo; `cancel_token()` da
  el token.
- `WorkerContext::is_cancelled()` consulta; `cancel_token()` da el
  token.

El adapter de servicio también guarda el token, así que un segundo
`on_start` cancela y espera (join) la ejecución anterior primero.

## ¿Por qué cooperativo y no preventivo?

- **Seguridad.** Arrancar una tarea async a la mitad de una sección
  crítica corrompe el estado; la cancelación cooperativa deja al
  callee correr su limpieza antes de retornar.
- **Honestidad entre lenguajes.** Las coroutines de Kotlin y las tasks
  de Swift cancelan cooperativamente. Un Rust preventivo rompería en
  silencio el modelo mental del lado nativo.
- **Primitivas baratas.** Un `AtomicBool` + un canal alcanzan. Sin
  dependencia de un runtime async.

## Siguiente

- Cancelación real en vuelo: [Servicios y workers](/istmo/es/writing-plugins/services-workers/).
- Referencia del frame: [Protocolo de frames → `Cancel`](/istmo/es/concepts/frame-protocol/).
