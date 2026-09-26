---
title: Cancelación
description: Cancela cooperativamente calls en vuelo y streams de larga duración.
sidebar:
  order: 5
---

Istmo usa un modelo de cancelación **cooperativo**. Un caller señala
"por favor pará", y el callee revisa esa señal en sus puntos naturales
de suspensión. No existe terminación forzada.

## `CancelToken`

El tipo es `istmo::CancelToken` (`istmo-core::CancelToken`), un wrapper
delgado sobre un `AtomicBool` compartido y un receiver `flume`.

- `is_cancelled()` — poll sync, barato.
- `cancelled()` — wait async, resuelve cuando se llama `cancel()` en
  el lado opuesto del token.

Cada call hospedado recibe un `CancelToken` — el runtime lo inyecta en
`Dispatch::dispatch`. Los adapters (`#[istmo::service]`,
`#[istmo::worker]`) lo pliegan en su contexto; la DSL de la macro de
plugin te deja opt-in por método.

## Opt-in desde un trait

Añade un argumento cuyo *nombre de tipo* sea `CancelToken`. La macro lo
pela del wire tuple y del signature del cliente generado, luego lo
llena desde el token runtime-provisto:

```rust
use istmo::CancelToken;
use istmo::plugin;

#[plugin]
pub trait LongTask {
    async fn process(&self, data: Vec<u8>, cancel: CancelToken)
        -> Result<Output, TaskError>;
}
```

El `LongTaskClient::process` generado toma sólo `data` — los callers
Rust no construyen el token; llaman `cancel_handle()` en el
`CallHandle` retornado:

```rust
let call = LongTaskClient::from_runtime(&rt)?.process_handle(data);
let handle = call.cancel_handle();

tokio::time::sleep(Duration::from_secs(10)).await;
handle.cancel();

match call.await {
    Ok(out) => tracing::info!("done: {out:?}"),
    Err(err) if err.is_cancelled() => tracing::info!("cancelled"),
    Err(err) => tracing::error!("{err}"),
}
```

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

O espera cancel en paralelo con tu trabajo:

```rust
tokio::select! {
    biased;
    _ = cancel.cancelled() => Err(TaskError { reason: "cancelled".into() }),
    result = do_the_thing() => result,
}
```

## Cancelar streams

Dropear un `TypedStream<T>` del lado Rust emite `Frame::Cancel` para
su `stream_id`. El productor nativo ve la cancelación via su mecanismo
nativo de plataforma:

```kotlin
override fun updates(): Flow<Fix> = callbackFlow {
    // ...
    awaitClose { /* corre en cancel */ }
}
```

```swift
AsyncStream { continuation in
    continuation.onTermination = { _ in /* cleanup */ }
}
```

## Cancelar llamadas native-hosted

Soltar el future de una llamada unaria atendida por Kotlin o Swift
envía `Frame::Cancel` al runtime nativo, que cancela la corrutina
(`Job.cancel()`) o la `Task` (`Task.cancel()`) de la llamada.
Reaccioná con el mecanismo propio de la plataforma; la llamada termina
sin respuesta:

```kotlin
override suspend fun authenticate(prompt: AuthPrompt): AuthMethod =
    suspendCancellableCoroutine { cont ->
        cont.invokeOnCancellation { /* cerrar el diálogo */ }
        // ...
    }
```

```swift
func authenticate(prompt: AuthPrompt) async throws -> AuthMethod {
    try await withTaskCancellationHandler {
        // ...
    } onCancel: { /* cerrar el diálogo */ }
}
```

## Services y workers

Los adapters `#[istmo::service]` y `#[istmo::worker]` bridge el token
automáticamente:

- `ServiceContext::stopped().await` resuelve cuando o el método `stop`
  se llama, *o* `Frame::Cancel` llega.
- `WorkerContext::cancel_token()` da acceso directo.

El service adapter también retiene el token para que un `on_start`
subsecuente cancele la instancia previa limpiamente.

## Por qué cooperativo, no preemptivo?

- **Safety.** Arrancar una task async de una sección crítica corrompe
  estado; el cancel cooperativo deja al callee correr cleanup antes
  de retornar.
- **Honestidad cross-language.** Los runtimes de JVM y Objective-C
  ambos usan cancel cooperativo. Rust preemptivo silenciosamente
  rompería el modelo mental del lado nativo.
- **Primitivas baratas.** `AtomicBool` + un channel es suficiente. Sin
  dependencia de async runtime.

## Siguiente

- Cancel real en vuelo: [Services y workers](/istmo/es/writing-plugins/services-workers/).
- Referencia de variante del frame: [Frame protocol → `Cancel`](/istmo/es/concepts/frame-protocol/).
