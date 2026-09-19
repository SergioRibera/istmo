---
title: Streams
description: Emite una secuencia de items desde un método de plugin en vez de una sola respuesta.
sidebar:
  order: 3
---

Algunos métodos de plugin producen un stream de valores en vez de un
único resultado — actualizaciones de ubicación, muestras de sensor,
tail de log, eventos de deep-link. Istmo los modela como `async fn`s
normales marcados con `#[istmo::stream]`. El autor del trait escribe
lo que parece un método normal; la macro lo reescribe para retornar un
receiver del lado Rust y un `Flow<T>` / `AsyncStream<T>` en
Kotlin / Swift.

## Declarar un stream

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

La macro reescribe `updates` para retornar `flume::Receiver<LocationFix>`
del lado Rust. Cada item que envías va por el wire como
`Frame::Event`; dropear el sender emite `Frame::StreamEnd`.

Los streams viven en cualquier thread — Istmo no requiere que estén
atados al executor actual.

## Producir items — Rust-hosted

Si el plugin es Rust-hosted, retorna un `Receiver` y empuja items
desde una task o thread spawneado:

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

El comportamiento de overflow se controla por el bound del channel que
elijas. Prefiere channels bounded — un stream unbounded que produce
más rápido que el consumer lee crece para siempre.

## Producir items — Native-hosted

El dispatcher Kotlin generado expone un `Flow`, y Swift expone un
`AsyncStream`. Emites items con la maquinaria de coroutine o task que
prefieras:

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
        // cablea delegate → continuation.yield(fix)
        continuation.onTermination = { _ in manager.stopUpdatingLocation() }
    }
}
```

## Consumir un stream desde Rust

```rust
use futures_util::StreamExt;
use location_tracker::LocationTrackerClient;

let tracker = LocationTrackerClient::from_runtime(&runtime)?;
let mut updates = tracker.updates();
while let Some(fix) = updates.recv_async().await.ok() {
    tracing::info!("{:?}", fix);
}
```

`updates` aquí es un `TypedStream<LocationFix>` envolviendo un
decoder bincode. Implementa `Stream<Item = LocationFix>`, así que
cualquier executor que hable `futures::Stream` puede driveelo.

Dropear el receiver del lado Rust emite `Frame::Cancel` upstream — el
productor nativo sabe que debe parar.

## Cancelación

Cancel explícito es una sola llamada:

```rust
let handle = tracker.updates_handle()?;
tokio::time::sleep(Duration::from_secs(30)).await;
handle.cancel();
```

Bajo el capó esto emite `Frame::Cancel { call_id }`. Kotlin/Swift
observan la cancelación via el mecanismo estándar de cancelación que
su API de stream soporte (`awaitClose` de `callbackFlow`,
`onTermination` de `AsyncStream`).

Ver [Cancelación](/es/writing-plugins/cancellation/) para el modelo
cooperativo en detalle.

## Tipos nombrados en items del stream

Si el item del stream es un struct `#[istmo::message]` — como
`LocationFix` arriba — participa en la misma interfaz `<T>Codecs` que
otros métodos del plugin usan. No escribes lógica extra de codec.

## Siguiente

- Suscriptores de larga vida: [Cancelación](/es/writing-plugins/cancellation/).
- Streaming de sensores nativos: [Native handles](/es/writing-plugins/native-handles/).
- Ejemplo completo: el [plugin `live-activity`](/es/plugins/live-activity/)
  streamea actualizaciones de estado de activity.
