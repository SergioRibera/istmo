---
title: Streams
description: Emit a sequence of items from a plugin method instead of a single response.
sidebar:
  order: 3
---

Some plugin methods produce a stream of values rather than a single
result — location updates, sensor samples, log tail, deep-link events.
Istmo models these as trait methods marked `#[istmo::stream]`. They are
plain `fn`s (not `async`) whose declared return type is the stream
**item**; the macro turns that into a receiver on Rust's side and a
`Flow<T>` / `AsyncThrowingStream<T, Error>` on Kotlin / Swift.

## Declaring a stream

```rust
use istmo::plugin;

#[plugin(name = "acme.location_tracker")]
pub trait LocationTracker {
    /// Position updates until the subscriber drops the stream.
    #[istmo::stream]
    fn updates(&self) -> LocationFix;

    async fn last_known(&self) -> Result<LocationFix, TrackerError>;
}

#[istmo::message]
#[derive(Debug, Clone)]
pub struct LocationFix {
    pub latitude:  f64,
    pub longitude: f64,
    pub accuracy:  f32,
}
```

The macro rewrites `updates` in the trait to return
`flume::Receiver<LocationFix>`. Every item you send goes over the wire
as a `Frame::Event`; dropping the sender ends the stream with
`Frame::StreamEnd { Complete }`.

Marking an `async fn` with `#[istmo::stream]` is a compile error
("`#[istmo::stream]` methods are not `async`"), and `#[istmo::owned]`
is not supported on stream methods.

## Fallible streams

An item written `Result<T, E>` makes the stream fallible:

```rust
#[istmo::stream]
fn checked_updates(&self) -> Result<LocationFix, TrackerError>;
```

`Ok(t)` items travel as events; the first `Err(e)` ends the stream
with `StreamEnd::Error` carrying `e`, and nothing sent after it is
read. As for unary methods, `E` must implement `std::error::Error`
(unless it is `()`).

## Producing items — Rust-hosted

If the plugin is Rust-hosted, return a `flume::Receiver` (add `flume =
"0.11"` to the plugin crate) and push items from any thread:

```rust
impl LocationTracker for LocationTrackerImpl {
    fn updates(&self) -> flume::Receiver<LocationFix> {
        let (tx, rx) = flume::bounded(32);
        let source = self.source.clone();
        std::thread::spawn(move || {
            while let Some(fix) = source.next_fix() {
                // `send` fails once the subscriber is gone: stop producing.
                if tx.send(fix).is_err() {
                    break;
                }
            }
        });
        rx
    }

    fn checked_updates(&self) -> flume::Receiver<Result<LocationFix, TrackerError>> {
        // same shape; send `Err(..)` to end the stream with that error
        /* ... */
    }

    async fn last_known(&self) -> Result<LocationFix, TrackerError> { /* ... */ }
}
```

A stream method can also take a `CancelToken` argument (see
[Cancellation](/istmo/writing-plugins/cancellation/)); the runtime trips
it when the subscriber cancels, which wakes a producer that is waiting
rather than sending.

Overflow behaviour is controlled by the channel bound you pick. Prefer
bounded channels — an unbounded stream that produces faster than the
consumer reads will grow forever.

## Producing items — native-hosted

On Android the generated `<T>Backend` interface declares the stream as
a `Flow`; emit with whatever coroutine machinery you prefer:

```kotlin
override fun updates(): Flow<LocationFix> = callbackFlow {
    val listener = LocationListener { loc ->
        trySend(LocationFix(loc.latitude, loc.longitude, loc.accuracy))
    }
    locationManager.requestLocationUpdates(/* ... */)
    awaitClose { locationManager.removeUpdates(listener) }
}
```

For a fallible stream, throw `BackendException(TrackerError.…)` from
the flow: the dispatcher ends the stream with that domain error.

On iOS the `<T>Backend` protocol declares
`func updates() -> AsyncThrowingStream<LocationFix, Error>`, but the
generated Swift dispatcher does not serve stream methods yet: a call
is answered with an unknown-method error. Host streams that iOS must
serve in Rust.

## Consuming a stream from Rust

The client method opens the stream and returns a
`TypedStream<Item>` (`TypedStream<Item, E>` for a fallible stream):

```rust
use istmo::StreamItem;
use location_tracker::LocationTrackerClient;

let tracker = LocationTrackerClient::from_runtime(&runtime)?;
let updates = tracker.updates()?;
loop {
    match updates.recv_async().await? {
        StreamItem::Event(fix) => tracing::info!("{fix:?}"),
        StreamItem::Completed | StreamItem::Cancelled => break,
        StreamItem::Failed(()) => unreachable!("infallible stream"),
    }
}
```

`TypedStream` offers `recv` (blocking), `recv_async` and `try_recv`
(non-blocking peek), each yielding a `StreamItem`:

| Variant | Meaning |
|---|---|
| `Event(T)` | One decoded item. |
| `Completed` | The producer ended the stream. |
| `Cancelled` | The stream was cancelled. |
| `Failed(E)` | A fallible stream ended with its domain error, decoded. |

`recv*` return `Err(IstmoError::ChannelClosed)` once the channel is
gone, and `Err(IstmoError::Codec(..))` if an item does not decode.

## Cancellation

Drop the `TypedStream` to cancel it. Before the stream has ended, the
drop trips the hosted producer's `CancelToken` when a host on the same
runtime serves it, or sends `Frame::Cancel` to the peer otherwise.
A Kotlin producer sees its flow cancelled (`callbackFlow`'s
`awaitClose` runs).
Dropping a stream that already ended cancels nothing.

See [Cancellation](/istmo/writing-plugins/cancellation/) for the
cooperative model in detail.

## Named types in stream items

If the stream item is a `#[istmo::message]` struct — as `LocationFix`
is above — it participates in the same `<T>Codecs` interface the
plugin's other methods use. You do not write extra codec logic.

## Next

- Long-lived subscribers: [Cancellation](/istmo/writing-plugins/cancellation/).
- Handing native objects to Rust: [Native handles](/istmo/writing-plugins/native-handles/).
- Full example: the [`pen` plugin](/istmo/plugins/pen/) streams stylus
  samples.
