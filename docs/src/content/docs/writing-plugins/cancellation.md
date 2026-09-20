---
title: Cancellation
description: Cooperatively cancel in-flight calls and long-running streams.
sidebar:
  order: 5
---

Istmo uses a **cooperative** cancellation model. A caller signals
"please stop", and the callee checks that signal at its own natural
suspension points. There is no forced termination.

## `CancelToken`

The type is `istmo::CancelToken` (`istmo-core::CancelToken`), a thin
wrapper over a shared `AtomicBool` and a `flume` receiver.

- `is_cancelled()` — sync poll, cheap.
- `cancelled()` — async wait, resolves when `cancel()` is called on the
  token's opposite side.

Every hosted call receives a `CancelToken` — the runtime injects it
into `Dispatch::dispatch`. Adapters (`#[istmo::service]`,
`#[istmo::worker]`) fold it into their context; the plugin macro DSL
lets you opt in per method.

## Opting in from a trait

Add an argument whose *type name* is `CancelToken`. The macro strips it
from the wire tuple and from the generated client signature, then fills
it from the runtime-provided token:

```rust
use istmo::CancelToken;
use istmo::plugin;

#[plugin]
pub trait LongTask {
    async fn process(&self, data: Vec<u8>, cancel: CancelToken)
        -> Result<Output, TaskError>;
}
```

The generated `LongTaskClient::process` takes only `data` — Rust
callers do not construct the token; they call `cancel_handle()` on the
returned `CallHandle`:

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

Or wait for cancel in parallel with your work:

```rust
tokio::select! {
    biased;
    _ = cancel.cancelled() => Err(TaskError { reason: "cancelled".into() }),
    result = do_the_thing() => result,
}
```

## Cancelling streams

Dropping a `TypedStream<T>` on the Rust side emits `Frame::Cancel`
for its `stream_id`. The native producer sees cancellation via its
platform-native mechanism:

```kotlin
override fun updates(): Flow<Fix> = callbackFlow {
    // ...
    awaitClose { /* runs on cancel */ }
}
```

```swift
AsyncStream { continuation in
    continuation.onTermination = { _ in /* cleanup */ }
}
```

## Services and workers

`#[istmo::service]` and `#[istmo::worker]` adapters bridge the token
automatically:

- `ServiceContext::stopped().await` resolves when either the
  `stop` method is called *or* `Frame::Cancel` arrives.
- `WorkerContext::cancel_token()` gives direct access.

The service adapter also holds the token so a subsequent `on_start`
cancels the previous instance cleanly.

## Why cooperative, not preemptive?

- **Safety.** Ripping an async task out of the middle of a critical
  section corrupts state; cooperative cancel lets the callee run
  cleanup before returning.
- **Cross-language honesty.** JVM and Objective-C runtimes both use
  cooperative cancel. Preemptive Rust would silently break the mental
  model on the native side.
- **Cheap primitives.** `AtomicBool` + one channel is enough. No async
  runtime dependency.

## Next

- Real cancellation in flight: [Services and workers](/istmo/writing-plugins/services-workers/).
- Frame variant reference: [Frame protocol → `Cancel`](/istmo/concepts/frame-protocol/).
