---
title: Streams
description: Emite una secuencia de items desde un método de plugin en vez de una sola respuesta.
sidebar:
  order: 3
---

Algunos métodos de plugin producen un stream de valores en vez de un
único resultado — actualizaciones de ubicación, muestras de sensor,
tail de log, eventos de deep-link. Istmo los modela como métodos del
trait marcados con `#[istmo::stream]`. Son `fn` planas (no `async`)
cuyo tipo de retorno declarado es el **item** del stream; la macro lo
convierte en un receiver del lado Rust y en un `Flow<T>` /
`AsyncThrowingStream<T, Error>` en Kotlin / Swift.

## Declarar un stream

```rust
use istmo::plugin;

#[plugin(name = "acme.location_tracker")]
pub trait LocationTracker {
    /// Actualizaciones de posición hasta que el suscriptor suelta el stream.
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

La macro reescribe `updates` en el trait para que retorne
`flume::Receiver<LocationFix>`. Cada item que envías viaja por el wire
como un `Frame::Event`; soltar el sender termina el stream con
`Frame::StreamEnd { Complete }`.

Marcar una `async fn` con `#[istmo::stream]` es un error de compilación
("`#[istmo::stream]` methods are not `async`"), y `#[istmo::owned]` no
se admite en métodos stream.

## Streams falibles

Un item escrito `Result<T, E>` hace que el stream sea falible:

```rust
#[istmo::stream]
fn checked_updates(&self) -> Result<LocationFix, TrackerError>;
```

Los items `Ok(t)` viajan como eventos; el primer `Err(e)` termina el
stream con `StreamEnd::Error` llevando `e`, y nada de lo enviado
después se lee. Igual que en los métodos unarios, `E` debe implementar
`std::error::Error` (salvo que sea `()`).

## Producir items — Rust-hosted

Si el plugin es Rust-hosted, retorna un `flume::Receiver` (agrega
`flume = "0.11"` al crate del plugin) y empuja items desde cualquier
thread:

```rust
impl LocationTracker for LocationTrackerImpl {
    fn updates(&self) -> flume::Receiver<LocationFix> {
        let (tx, rx) = flume::bounded(32);
        let source = self.source.clone();
        std::thread::spawn(move || {
            while let Some(fix) = source.next_fix() {
                // `send` falla cuando el suscriptor ya no está: deja de producir.
                if tx.send(fix).is_err() {
                    break;
                }
            }
        });
        rx
    }

    fn checked_updates(&self) -> flume::Receiver<Result<LocationFix, TrackerError>> {
        // misma forma; envía `Err(..)` para terminar el stream con ese error
        /* ... */
    }

    async fn last_known(&self) -> Result<LocationFix, TrackerError> { /* ... */ }
}
```

Un método stream también puede recibir un argumento `CancelToken` (ver
[Cancelación](/istmo/es/writing-plugins/cancellation/)); el runtime lo
dispara cuando el suscriptor cancela, lo que despierta a un productor
que está esperando en vez de enviando.

El comportamiento ante overflow lo controla el límite del canal que
elijas. Prefiere canales bounded — un stream unbounded que produce más
rápido de lo que el consumidor lee crecerá para siempre.

## Producir items — native-hosted

En Android la interfaz `<T>Backend` generada declara el stream como un
`Flow`; emite con la maquinaria de coroutines que prefieras:

```kotlin
override fun updates(): Flow<LocationFix> = callbackFlow {
    val listener = LocationListener { loc ->
        trySend(LocationFix(loc.latitude, loc.longitude, loc.accuracy))
    }
    locationManager.requestLocationUpdates(/* ... */)
    awaitClose { locationManager.removeUpdates(listener) }
}
```

Para un stream falible, lanza `BackendException(TrackerError.…)` desde
el flow: el dispatcher termina el stream con ese error de dominio.

En iOS el protocolo `<T>Backend` declara
`func updates() -> AsyncThrowingStream<LocationFix, Error>`, pero el
dispatcher Swift generado todavía no sirve métodos stream: una llamada
se responde con un error de método desconocido. Hostea en Rust los
streams que iOS deba servir.

## Consumir un stream desde Rust

El método del cliente abre el stream y retorna un
`TypedStream<Item>` (`TypedStream<Item, E>` para un stream falible):

```rust
use istmo::StreamItem;
use location_tracker::LocationTrackerClient;

let tracker = LocationTrackerClient::from_runtime(&runtime)?;
let updates = tracker.updates()?;
loop {
    match updates.recv_async().await? {
        StreamItem::Event(fix) => tracing::info!("{fix:?}"),
        StreamItem::Completed | StreamItem::Cancelled => break,
        StreamItem::Failed(()) => unreachable!("stream infalible"),
    }
}
```

`TypedStream` ofrece `recv` (bloqueante), `recv_async` y `try_recv`
(consulta no bloqueante), cada uno entregando un `StreamItem`:

| Variante | Significado |
|---|---|
| `Event(T)` | Un item decodificado. |
| `Completed` | El productor terminó el stream. |
| `Cancelled` | El stream fue cancelado. |
| `Failed(E)` | Un stream falible terminó con su error de dominio, decodificado. |

`recv*` retornan `Err(IstmoError::ChannelClosed)` cuando el canal ya no
existe, y `Err(IstmoError::Codec(..))` si un item no decodifica.

## Cancelación

Suelta el `TypedStream` para cancelarlo. Antes de que el stream
termine, el drop dispara el `CancelToken` del productor hosteado cuando
lo sirve un host en el mismo runtime, o envía `Frame::Cancel` al peer
en caso contrario. Un productor Kotlin ve su flow
cancelado (corre el `awaitClose` de `callbackFlow`). Soltar un stream que ya terminó
no cancela nada.

Ver [Cancelación](/istmo/es/writing-plugins/cancellation/) para el
modelo cooperativo en detalle.

## Tipos con nombre en items del stream

Si el item del stream es un struct `#[istmo::message]` — como
`LocationFix` arriba — participa en la misma interfaz `<T>Codecs` que
usan los otros métodos del plugin. No escribes lógica de codec extra.

## Siguiente

- Suscriptores de larga vida: [Cancelación](/istmo/es/writing-plugins/cancellation/).
- Pasar objetos nativos a Rust: [Native handles](/istmo/es/writing-plugins/native-handles/).
- Ejemplo completo: el [plugin `pen`](/istmo/es/plugins/pen/) hace
  stream de las muestras del stylus.
