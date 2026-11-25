---
title: Frame sinks
description: Mueve frames de video, buffers de audio y otros buffers nativos de alta frecuencia de un plugin a Rust sin codificarlos ni copiarlos.
sidebar:
  order: 5
---

Un item de stream viaja por el wire: se codifica, se rutea y se
decodifica, y un productor Kotlin o Swift suma una copia JNI / FFI. Eso
sirve para eventos y muestras, y es demasiado para un frame 4K sesenta
veces por segundo. Un **frame sink** es el camino para buffers que no
deben serializarse nunca: DMA-BUFs, `HardwareBuffer`s, `CVPixelBuffer`s /
`IOSurface`s, texturas D3D11 o memoria de CPU que ya tienes.

El consumidor abre un sink y pasa su id al plugin como un argumento de
método normal. El productor — un host Rust en el mismo proceso, o un
backend Kotlin / Swift — empuja **frames**: metadatos del plugin más un
`NativeBuffer`. Solo se codifican los metadatos (y desde Rust ni eso), y
soltar un frame devuelve su buffer al productor.

## Declarar un argumento sink

```rust
use istmo::{FrameSinkId, plugin};

#[istmo::message]
#[derive(Debug, Clone)]
pub struct FrameInfo {
    pub width: u32,
    pub height: u32,
    pub timestamp_ns: u64,
}

#[plugin(name = "acme.capture")]
pub trait Capture {
    /// Captura en `frames` hasta `stop`.
    async fn start(&self, frames: FrameSinkId<FrameInfo>) -> Result<(), CaptureError>;
    async fn stop(&self);
}
```

`FrameSinkId<M>` es un `u64` en el wire; `M` nombra los metadatos de
cada frame, así productor y consumidor coinciden en compilación. Un id
de sink solo vale como argumento de método: un campo de mensaje, un
retorno o un contenedor que lo lleve se rechaza al extraer el contrato.

## Consumir frames (Rust)

```rust
use istmo::SinkPolicy;

let sink = runtime.open_frame_sink::<FrameInfo>(SinkPolicy::latest())?;
capture.start(sink.id()).await?;

while let Some(frame) = sink.recv_async().await {
    let info: &FrameInfo = &frame.meta;
    if frame.dropped_before() > 0 {
        tracing::debug!(lost = frame.dropped_before(), "el consumidor se atrasó");
    }
    if let Some(bytes) = frame.buffer.cpu_bytes() { /* píxeles en CPU */ }
    // `frame` se suelta aquí: el productor recupera su buffer.
}
```

`recv` (bloqueante), `recv_async` y `try_recv` devuelven `None` cuando
el sink está cerrado y vacío. Soltar el `FrameSink` lo cierra: los
productores se enteran en su siguiente push y los frames en cola se
liberan.

### Contrapresión

Los productores nunca se bloquean. La política decide qué hace un sink
lleno, y cada frame perdido se cuenta (`dropped_before()` en el
siguiente frame, `stats()` para totales):

| Política | Comportamiento | Para |
|---|---|---|
| `SinkPolicy::latest()` | profundidad 1, un frame nuevo reemplaza al encolado | previsualizaciones, mirroring |
| `SinkPolicy::queue(n)` | profundidad `n`, descarta el más viejo | encoders que toleran un tropiezo |
| `SinkPolicy::queue(n).with_overflow(Overflow::DropNewest)` | profundidad `n`, rechaza el nuevo | consumidores que necesitan un prefijo continuo |

Retén frames solo lo necesario: los productores con pool (`ImageReader`,
el pool de buffers de un compositor) reutilizan un buffer solo cuando su
frame se suelta, así que un consumidor que guarda todos frena la captura.

## Buffers

`NativeBuffer` posee un valor de respaldo y hooks de liberación
opcionales. El `Drop` del respaldo libera lo suyo; los hooks de
`with_release` corren después. Clonar comparte el buffer, así repartir
un frame a varios consumidores no copia nada; la liberación corre una
vez, al soltarse el último clon.

| `kind()` | Respaldo | Lo crea |
|---|---|---|
| `Cpu` | cualquier `AsRef<[u8]>` (`Vec<u8>`, `Bytes`, un mapeo) — `cpu_bytes()` | `NativeBuffer::from_cpu` |
| `DmaBuf` | `DmaBuf` (planos de `OwnedFd`, fourcc, modificador) — `dmabuf()` | `NativeBuffer::from_dmabuf` |
| `HardwareBuffer` | `istmo::android::HardwareBuffer` (`as_ptr()`, `desc()`) | Kotlin `IstmoFrameSink.submit(HardwareBuffer, …)` |
| `CvPixelBuffer` / `IoSurface` | `istmo::ios::RetainedBuffer` (`as_ptr()`) | Swift `IstmoFrameSink.submit(CVPixelBuffer, …)` |
| cualquiera | tu propio tipo | `NativeBuffer::new(kind, value)` |

Llega a un respaldo de plataforma con `frame.buffer.downcast_ref::<T>()`,
o sácalo con `into_backing::<T>()`, que devuelve un `BufferLease` que
mantiene reservado el slot del productor hasta soltarse.

## Producir frames desde Rust

Un host Rust resuelve el id en un `FrameSender` tipado — sin bytes:

```rust
impl Capture for DesktopCapture {
    async fn start(&self, frames: FrameSinkId<FrameInfo>) -> Result<(), CaptureError> {
        // `self.runtime`: el `Arc<Runtime>` con el que se construyó el host.
        let sender = self
            .runtime
            .frame_sender(frames)
            .map_err(|_| CaptureError::SinkUnreachable)?;
        std::thread::spawn(move || {
            while let Some((pixels, info)) = next_frame() {
                let buffer = NativeBuffer::from_cpu(pixels);
                if let Err(err) = sender.push(info, buffer) {
                    if err.is_closed() { break; } // el consumidor se fue
                }
            }
            // soltar el último clon de `sender` termina el sink
        });
        Ok(())
    }
    /* ... */
}
```

Resuelve el id una vez por sesión y clona el sender: cada resolución es
un productor propio, y el sink se cierra cuando se suelta el último clon
de uno (o con `close()`).

## Producir frames desde Kotlin y Swift

El backend generado recibe un `IstmoFrameSink<FrameInfo>` en vez de un id:

```kotlin
override suspend fun start(frames: IstmoFrameSink<FrameInfo>) {
    reader.setOnImageAvailableListener({ r ->
        r.acquireLatestImage()?.use { image ->
            val status = frames.submit(image.hardwareBuffer!!, FrameInfo(...))
            if (status == IstmoSubmitStatus.CLOSED) stopCapture()
        }
    }, handler)
}
```

```swift
func start(frames: IstmoFrameSink<FrameInfo>) async throws {
    output.onSample = { sample in
        guard let pixels = CMSampleBufferGetImageBuffer(sample) else { return }
        if frames.submit(pixels, meta: FrameInfo(...)) == .closed { self.stop() }
    }
}
```

| Kotlin | Swift | Qué recibe Rust |
|---|---|---|
| `submit(HardwareBuffer, meta)` (API 26+) | `submit(CVPixelBuffer, meta:)` / `submit(IOSurfaceRef, meta:)` | su propia referencia al buffer; cierra la tuya cuando quieras |
| `submit(ByteBuffer, length, meta)` (directo) | — | la memoria del buffer, leída en sitio: no la escribas después |
| `submitBytes(ByteArray, meta)` | `submitBytes(Data, meta:)` | una copia |
| `close()` | `close()` | fin del stream |

Cada submit devuelve `IstmoSubmitStatus`: `ACCEPTED`, `DISPLACED` (se
descartó un frame más viejo), `FULL`, `CLOSED` (deja de producir),
`UNKNOWN_SINK`, `BAD_META`, `UNSUPPORTED` (un `HardwareBuffer` bajo API
26), `NOT_STARTED`, `INVALID`. Pasan por cuatro entradas genéricas del
transporte (`nativeSubmitHardwareBuffer` y compañía /
`istmo_ios_submit_native_buffer` y compañía); ningún plugin agrega
símbolos JNI o C propios.

## Entre procesos

Los ids de sink solo tienen sentido en el runtime que los abrió. Un
productor en otro proceso (un servicio `:remote`, u otro runtime) no
resuelve el id — `IstmoError::UnknownFrameSink` en Rust, `UNKNOWN_SINK`
en nativo — y debe fallar la llamada con un error de dominio.
`runtime.shares_native_frames(plugin_id)` es `false` para plugins
declarados remotos, así un cliente puede negarse antes de enviar el id.
Los plugins que quieran consumidores remotos ofrecen un camino explícito
con copia (un `#[stream]` de frames codificados) junto al sink.

## Costo

Por item, mismo proceso (`crates/core/benches/stream_paths.rs`, build
release, Ryzen 9 9950X3D): un stream por el bus paga ~1,06 ms por un
item de 8 MiB (dos copias más ruteo) contra ~65–71 µs del atajo tipado o
de un sink, que es la propia reserva de memoria del productor. Con items
de 16 bytes todos los caminos los limita el paso entre hilos
(~330–490 ns); en un solo hilo el sink cuesta ~43 ns por frame, como un
encode / decode, y un paso por flume ~23 ns.

## Siguiente

- Mover items de stream sin codificar: [Streams](/istmo/es/writing-plugins/streams/#atajo-en-proceso).
- Objetos que conserva el lado nativo: [Native handles](/istmo/es/writing-plugins/native-handles/).
