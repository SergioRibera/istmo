---
title: Streams
description: Emit a sequence of items from a plugin method instead of a single response.
sidebar:
  order: 3
---

Some plugin methods produce a stream of values rather than a single
result — location updates, sensor samples, log tail, deep-link events.
Istmo models these as regular `async fn`s marked with
`#[istmo::stream]`. The trait author writes what looks like a normal
method; the macro rewrites it to return a receiver on Rust's side and a
`Flow<T>` / `AsyncStream<T>` on Kotlin / Swift.

## Declaring a stream

```rust
use istmo::plugin;

#[plugin]
pub trait LocationTracker {
    #[istmo::stream]
    async fn updates(&self) -> LocationFix;

    async fn last_known(&self) -> Result<LocationFix, TrackerError>;
}

#[istmo::message]
pub struct LocationFix {
    pub latitude:  f64,
    pub longitude: f64,
    pub accuracy:  f32,
}
```

The macro rewrites `updates` to return `flume::Receiver<LocationFix>`
on Rust's side. Every item you send goes over the wire as a
`Frame::Event`; dropping the sender emits `Frame::StreamEnd`.

Streams live on any thread — Istmo does not require them to be tied to
the current executor.

## Producing items — Rust-hosted

If the plugin is Rust-hosted, return a `Receiver` and push items from a
spawned task or thread:

```rust
impl LocationTracker for LocationTrackerImpl {
    fn updates(&self) -> flume::Receiver<LocationFix> {
        let (tx, rx) = flume::bounded(32);
        self.spawn_polling_loop(tx);
        rx
    }

    async fn last_known(&self) -> Result<LocationFix, TrackerError> { /* ... */ }
}
```

Overflow behaviour is controlled by the channel bound you pick. Prefer
bounded channels — an unbounded stream that produces faster than the
consumer reads will grow forever.

## Producing items — Native-hosted

The generated Kotlin dispatcher exposes a `Flow`, and Swift exposes an
`AsyncStream`. You emit items with whatever coroutine or task machinery
you prefer:

```kotlin
override fun updates(): Flow<LocationFix> = callbackFlow {
    val listener = LocationListener { loc ->
        trySend(LocationFix(loc.latitude, loc.longitude, loc.accuracy))
    }
    locationManager.requestLocationUpdates(...)
    awaitClose { locationManager.removeUpdates(listener) }
}
```

```swift
func updates() -> AsyncStream<LocationFix> {
    AsyncStream { continuation in
        let manager = CLLocationManager()
        // wire delegate → continuation.yield(fix)
        continuation.onTermination = { _ in manager.stopUpdatingLocation() }
    }
}
```

## Consuming a stream from Rust

```rust
use futures_util::StreamExt;
use location_tracker::LocationTrackerClient;

let tracker = LocationTrackerClient::from_runtime(&runtime)?;
let mut updates = tracker.updates();
while let Some(fix) = updates.recv_async().await.ok() {
    tracing::info!("{:?}", fix);
}
```

`updates` here is a `TypedStream<LocationFix>` wrapping a bincode
decoder. It implements `Stream<Item = LocationFix>`, so any executor
that speaks `futures::Stream` can drive it.

Dropping the receiver on the Rust side sends a `Frame::Cancel`
upstream — the native producer knows to stop.

## Cancellation

Explicit cancel is a single call:

```rust
let handle = tracker.updates_handle()?;
tokio::time::sleep(Duration::from_secs(30)).await;
handle.cancel();
```

Under the hood this emits `Frame::Cancel { call_id }`. Kotlin/Swift
observe cancellation via the standard cancellation mechanism their
stream API supports (`callbackFlow`'s `awaitClose`,
`AsyncStream.onTermination`).

See [Cancellation](/writing-plugins/cancellation/) for the cooperative
model in detail.

## Named types in stream items

If the stream item is a `#[istmo::message]` struct — as `LocationFix`
is above — it participates in the same `<T>Codecs` interface the
plugin's other methods use. You do not write extra codec logic.

## Next

- Long-lived subscribers: [Cancellation](/writing-plugins/cancellation/).
- Streaming platform sensors: [Native handles](/writing-plugins/native-handles/).
- Full example: [`live-activity` plugin](/plugins/live-activity/) streams
  activity state updates.
