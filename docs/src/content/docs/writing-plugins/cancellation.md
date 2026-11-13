---
title: Cancellation
description: Cooperatively cancel in-flight calls and long-running streams.
sidebar:
  order: 5
---

Istmo uses a **cooperative** cancellation model. A caller signals
"please stop", and the callee checks that signal at its own natural
suspension points. There is no forced termination.

## Cancelling from the caller

There is no cancel method to call: a Rust caller cancels by dropping.

- **Unary calls.** A client method is an `async fn`; dropping its
  future before the response arrives cancels the call. Wrap it in
  whatever timeout or `select` your executor offers, or drop the task
  that awaits it.
- **Streams.** Dropping the `TypedStream` cancels it (see
  [Streams](/istmo/writing-plugins/streams/#cancellation)).

When a host on the same runtime serves the call, the drop trips that
call's `CancelToken`; otherwise it sends `Frame::Cancel { call_id }`
to the peer (Kotlin, Swift, or a remote process). A call or stream that
already finished cancels nothing.

```rust
let task = LongTaskClient::from_runtime(&rt)?;
let call = task.process(data);
// Dropping `call` (here: when the timeout wins) cancels the work.
match with_timeout(Duration::from_secs(10), call).await {
    Some(result) => tracing::info!("done: {:?}", result?),
    None => tracing::info!("gave up"),
}
```

`with_timeout` stands for your executor's timeout combinator; Istmo
does not ship one.

## `CancelToken`

The callee's side is `istmo::CancelToken` (re-exported from
`istmo-core`): a shared flag plus a `flume` channel whose sender is
dropped on cancel.

- `is_cancelled()` — sync poll, cheap.
- `cancelled().await` — async wait, resolves once the token is
  cancelled.
- `cancel()` — trip it; tokens are `Clone` and every clone sees it.

The runtime creates one token per hosted call and hands it to
`Dispatch::dispatch`. Adapters (`#[istmo::service]`,
`#[istmo::worker]`) fold it into their context; plugin traits opt in
per method.

## Opting in from a trait

Add an argument whose type's last path segment is `CancelToken`. The
macro strips it from the wire tuple and from the generated client
signature, then fills it from the runtime-provided token:

```rust
use istmo::{CancelToken, plugin};

#[plugin(name = "acme.long_task")]
pub trait LongTask {
    async fn process(&self, data: Vec<u8>, cancel: CancelToken)
        -> Result<Output, TaskError>;
}
```

The generated `LongTaskClient::process` takes only `data`. Detection
is by name: `&CancelToken`, `Option<CancelToken>` or an alias under
another name are treated as wire arguments and fail to compile, since
the token is not a wire type. The same argument works on `#[stream]`
methods.

## Checking the token from an impl

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

Or wait for cancel in parallel with your work, with any `select`
(here `futures::select_biased!`):

```rust
use futures::FutureExt as _;

futures::select_biased! {
    () = cancel.cancelled().fuse() => Err(TaskError { reason: "cancelled".into() }),
    result = do_the_thing().fuse() => result,
}
```

A cancelled call sends no response, so what the impl returns after a
cancel is discarded.

## Cancelling native-hosted calls

On `Frame::Cancel` the native runtime cancels the call's coroutine
(`Job.cancel()`) on Android or its `Task` (`Task.cancel()`) on iOS.
React to it with the platform's own mechanism:

```kotlin
override suspend fun authenticate(prompt: AuthPrompt): AuthMethod =
    suspendCancellableCoroutine { cont ->
        cont.invokeOnCancellation { /* dismiss the dialog */ }
        // ...
    }
```

```swift
func authenticate(prompt: AuthPrompt) async throws -> AuthMethod {
    try await withTaskCancellationHandler {
        // ...
    } onCancel: { /* dismiss the dialog */ }
}
```

A Kotlin stream producer sees its `Flow` cancelled, so `callbackFlow`'s
`awaitClose` runs:

```kotlin
override fun updates(): Flow<Fix> = callbackFlow {
    // ...
    awaitClose { /* runs on cancel */ }
}
```

## Services and workers

`#[istmo::service]` and `#[istmo::worker]` adapters bridge the token
automatically:

- `ServiceContext::stopped().await` resolves when the service is asked
  to stop — the `stop` path or `Frame::Cancel` — and
  `ServiceContext::is_stopped()` polls the same; `cancel_token()` gives
  the token itself.
- `WorkerContext::is_cancelled()` polls; `cancel_token()` gives the
  token.

The service adapter also holds the token, so a second `on_start`
cancels and joins the previous run first.

## Why cooperative, not preemptive?

- **Safety.** Ripping an async task out of the middle of a critical
  section corrupts state; cooperative cancel lets the callee run
  cleanup before returning.
- **Cross-language honesty.** Kotlin coroutines and Swift tasks both
  cancel cooperatively. Preemptive Rust would silently break the
  mental model on the native side.
- **Cheap primitives.** An `AtomicBool` + one channel is enough. No
  async runtime dependency.

## Next

- Real cancellation in flight: [Services and workers](/istmo/writing-plugins/services-workers/).
- Frame variant reference: [Frame protocol → `Cancel`](/istmo/concepts/frame-protocol/).
