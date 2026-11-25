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
`IOSurface`s / `CMSampleBuffer`s, texturas D3D11 o memoria de CPU que ya
tienes.

El consumidor abre un sink y pasa su id al plugin como un argumento de
método normal. El productor — un host Rust en el mismo proceso, o un
backend Kotlin / Swift — empuja **frames**: metadatos del plugin más un
`NativeBuffer`. Solo se codifican los metadatos (y desde Rust ni eso), y
soltar un frame devuelve su buffer al productor.

## Un recorrido completo

Los fragmentos Rust de esta página se compilan como tests, así que
siguen siendo correctos.

```rust
use std::sync::Arc;

use istmo::{FrameSinkId, HostCtx, NativeBuffer, Runtime, SinkPolicy};

#[istmo::message]
#[derive(Debug, Clone)]
pub struct FrameInfo {
    pub width: u32,
    pub height: u32,
    pub timestamp_ns: u64,
}

#[istmo::message]
#[derive(Debug, Clone)]
pub enum CaptureError {
    SinkUnreachable,
}

impl std::fmt::Display for CaptureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("el frame sink no es alcanzable desde este host")
    }
}

impl std::error::Error for CaptureError {}

#[istmo::plugin(name = "acme.capture")]
pub trait Capture {
    /// Captura `count` frames en `frames`.
    async fn start(
        &self,
        ctx: HostCtx,
        count: u32,
        frames: FrameSinkId<FrameInfo>,
    ) -> Result<(), CaptureError>;
}

/// El backend de escritorio: un host Rust en el proceso del consumidor.
pub struct DesktopCapture;

impl Capture for DesktopCapture {
    async fn start(
        &self,
        ctx: HostCtx,
        count: u32,
        frames: FrameSinkId<FrameInfo>,
    ) -> Result<(), CaptureError> {
        // `HostCtx` lo rellena el host; nunca cruza el wire.
        let sender = ctx
            .frame_sender(frames)
            .map_err(|_| CaptureError::SinkUnreachable)?;
        std::thread::spawn(move || {
            for n in 0..count {
                let info = FrameInfo { width: 4, height: 4, timestamp_ns: u64::from(n) };
                let pixels = NativeBuffer::from_bytes(vec![0u8; 4 * 4 * 4]);
                if let Err(err) = sender.push(info, pixels) {
                    if err.is_closed() {
                        return; // el consumidor se fue: deja de capturar
                    }
                }
            }
            sender.close(); // el consumidor ve el fin, `EndReason::Completed`
        });
        Ok(())
    }
}

fn main() -> Result<(), istmo::IstmoError> {
    let runtime: Arc<Runtime> = Runtime::mock().runtime;
    runtime.register_host(CaptureHost::new(DesktopCapture));
    let capture = CaptureClient::from_runtime(&runtime)?;

    let sink = runtime.open_frame_sink::<FrameInfo>(SinkPolicy::queue(4))?;
    pollster::block_on(capture.start(3, sink.id()))?;

    let mut seen = 0;
    while let Some(frame) = sink.recv() {
        if frame.dropped_before() > 0 {
            eprintln!("se quedó atrás: {} frames desalojados", frame.dropped_before());
        }
        assert_eq!(frame.buffer.as_bytes().map(<[u8]>::len), Some(64));
        seen += 1;
        // `frame` se suelta aquí: el productor recupera su buffer.
    }
    assert!(seen > 0);
    assert!(sink.end_reason().is_some_and(|reason| reason.is_completed()));
    Ok(())
}
```

`FrameSinkId<M>` es un `u64` en el wire; `M` nombra los metadatos de
cada frame, así productor y consumidor coinciden en tiempo de
compilación. Un id de sink solo es válido como argumento de método, o un
`Option` de uno (`audio: Option<FrameSinkId<AudioInfo>>` para un segundo
stream opcional). `#[istmo::plugin]` lo rechaza en cualquier otro lugar
— un valor de retorno, un `Vec`, un campo de mensaje — señalando el
tipo, junto con todas las demás formas de argumento que Kotlin y Swift
no pueden mapear (tuplas, `usize`, `Duration`, referencias, mapas).

## Consumir

- `recv()` bloquea el hilo, `recv_async()` espera en cualquier executor,
  `try_recv()` nunca espera. `FrameSink` también es un
  `futures_core::Stream` de frames: `while let Some(frame) =
  sink.next().await` funciona con `StreamExt`, y `select!` también.
- Toda recepción devuelve `None` cuando el sink terminó y está vacío;
  `end_reason()` dice por qué: `Completed` (un productor llamó a
  `close()`), `Failed(error)` (`close_with(error)`, o `closeWithError`
  nativo), `ProducerGone` (todos los productores se soltaron sin cerrar —
  uno que falló u olvidado), `ConsumerClosed` o `Shutdown`.
- Varias tareas pueden esperar en un mismo sink: cada frame y el fin las
  despiertan a todas.
- Soltar el `FrameSink` lo cierra: los productores se enteran en su
  siguiente push y los frames en cola se liberan.

### Contrapresión

Los productores nunca se bloquean. La política decide qué hace un sink
lleno:

| Política | Comportamiento | Para |
|---|---|---|
| `SinkPolicy::latest()` | profundidad 1, un frame nuevo reemplaza al encolado | previsualizaciones, mirroring |
| `SinkPolicy::queue(n)` | profundidad `n`, desaloja el más viejo | encoders que toleran un tropiezo |
| `SinkPolicy::queue(n).with_overflow(Overflow::DropNewest)` | profundidad `n`, rechaza el frame nuevo | consumidores que necesitan un prefijo contiguo |

`queue(0)` hace panic: un sink que no guarda nada lo tiraría todo.
`stats()` cuenta frames `accepted`, `delivered`, `evicted` (perdidos por
`DropOldest`; el `dropped_before()` del siguiente frame dice cuántos),
`refused` (devueltos por `DropNewest`; un productor que reintenta cuenta
una vez por intento) y `discarded` (liberados sin entregar cuando el
consumidor soltó el sink o el runtime se apagó).

Retén los frames solo mientras los necesites: los productores con pool
(`ImageReader`, el pool de buffers de un compositor) reutilizan un
buffer solo cuando su frame se suelta, así que un consumidor que guarda
todos los frames detiene la captura.

## Producir desde Rust

`ctx.frame_sender(id)` (o `runtime.frame_sender(id)`) resuelve el id en
un `FrameSender` tipado: sin bytes de por medio. Cada sender — cada
resolución y cada clon — cuenta como un productor, y el sink termina como
`ProducerGone` cuando se suelta el último, así que volver a comprobar un
id nunca cierra un stream vivo. Termínalo a propósito con `close()` o
`close_with(error)`.

Una resolución falla con `IstmoError::UnknownFrameSink` (cerrado, o
abierto por otro runtime) o `FrameSinkTypeMismatch`, que nombra ambos
tipos de metadatos.

## Buffers

`NativeBuffer` posee un valor de respaldo más hooks de liberación. El
`Drop` del respaldo libera lo que tiene; los hooks añadidos con
`with_release` corren después. Ninguno puede propagar un panic al
productor ni al consumidor: se registra y las demás liberaciones siguen
corriendo. Clonar comparte el buffer, así que repartir un frame no copia
nada; la liberación ocurre una vez, cuando se suelta el último clon.

| `kind()` | Respaldo | Lo crea |
|---|---|---|
| `Cpu` | cualquier `AsRef<[u8]>` (`Vec<u8>`, `Bytes`, un mapeo, un slot de pool) — `as_bytes()` | `NativeBuffer::from_bytes` |
| `DmaBuf` | `Arc<DmaBuf>` (planos: `OwnedFd`, offset, stride, `size` del objeto) — `dmabuf()` | `NativeBuffer::from_dmabuf` |
| `HardwareBuffer` | `AHardwareBuffer*` — `platform_ptr()` | Kotlin `IstmoFrameSender.submit(Image / HardwareBuffer, …)` |
| `CvPixelBuffer` / `IoSurface` / `CmSampleBuffer` | el objeto retenido — `platform_ptr()` | Swift `IstmoFrameSender.submit(…)` |
| `D3d11Texture` | un tipo propio del plugin que implementa `PlatformBuffer` | `NativeBuffer::from_platform` |
| `VideoFrame` | reservado: los productores web copian (`VideoFrame.copyTo`) a `from_bytes` por ahora | — |

`platform_ptr()` da el objeto de plataforma (con `kind()` diciendo cuál)
lo haya producido quien sea. `downcast_ref::<T>()` alcanza un respaldo
de tipo conocido, e `into_backing::<T>()` lo saca con un `BufferLease`
que mantiene reservado el slot del productor hasta que se suelta
(`IntoBackingError` distingue un buffer compartido de un tipo
equivocado). Los leases son `Send + Sync`.

**Sincronización**: forma parte de la entrega. Un productor empuja un
frame solo cuando sus escrituras terminaron (esperó su fence de
adquisición), y las lecturas de un consumidor terminaron cuando suelta el
buffer o su lease (un consumidor GPU espera primero su propio trabajo).
Ningún fence viaja con el frame.

**Pools.** Un productor con pool guarda un `Arc<DmaBuf>` por slot y
empuja clones: sin `dup` por frame, los números de fd no cambian, y
`with_identity(BufferIdentity::new(generation, slot))` permite a los
consumidores cachear lo que importan por slot (una generación nueva
cuando el pool se realoca). Los fds de los planos deben ser
close-on-exec (`O_CLOEXEC`, `DRM_CLOEXEC`).

```rust
#[cfg(unix)]
mod pool {
    use std::sync::Arc;

    use istmo::{BufferIdentity, DmaBuf, NativeBuffer};

    /// Entrega el slot `slot` de un productor con pool; `give_back`
    /// corre cuando todos los clones del frame se soltaron.
    pub fn frame(
        generation: u64,
        slot: u32,
        pooled: &Arc<DmaBuf>,
        give_back: impl FnOnce() + Send + 'static,
    ) -> NativeBuffer {
        NativeBuffer::from_dmabuf(Arc::clone(pooled))
            .with_identity(BufferIdentity::new(generation, slot))
            .with_release(give_back)
    }
}
```

## Producir desde Kotlin

El backend generado recibe un `IstmoFrameSender<FrameInfo>` en lugar de
un id: una sesión de productor, abierta al llegar la llamada y terminada
con `close()` / `closeWithError(message)` (o, como red de seguridad,
cuando el recolector de basura la reclama). Ciérrala cuando termine la
sesión.

```kotlin
override suspend fun start(count: UInt, frames: IstmoFrameSender<FrameInfo>) {
    // maxImages: lo que el consumidor puede retener, más uno escribiéndose.
    val reader = ImageReader.newInstance(w, h, PixelFormat.RGBA_8888, 4,
        HardwareBuffer.USAGE_GPU_SAMPLED_IMAGE or HardwareBuffer.USAGE_CPU_READ_RARELY)
    reader.setOnImageAvailableListener({ r ->
        val image = r.acquireLatestImage() ?: return@setOnImageAvailableListener
        // El sender mantiene `image` abierta hasta que Rust suelta el frame
        // y luego la cierra: el productor nunca dibuja en un frame en lectura.
        val status = frames.submit(image, FrameInfo(w.toUInt(), h.toUInt(), image.timestamp.toULong()))
        if (status.shouldStop) { reader.close(); frames.close() }
    }, handler)
}
```

| Llamada | Qué recibe Rust |
|---|---|
| `submit(image, meta)` (API 28+) | su propia referencia al `AHardwareBuffer`; la `Image` queda abierta hasta que Rust la libera (tras su fence de adquisición, API 33+) |
| `submit(hardwareBuffer, meta, owner)` (API 26+) | su propia referencia; `owner` (si hay) se cierra cuando Rust la libera |
| `openSlotPool(size, count)`, `acquireSlot()`, `submit(slot, length, meta)` | memoria propiedad de Rust que Kotlin llenó: sin copia. El `buffer` de un slot vale solo hasta enviarlo o devolverlo con `release(slot)` |
| `submit(byteBuffer, meta)`, `submitBytes(bytes, meta)` | una copia; tu buffer vuelve a ser tuyo al retornar la llamada |

## Producir desde Swift

```swift
func start(count: UInt32, frames: IstmoFrameSender<FrameInfo>) async throws {
    output.onSample = { [weak self] sample in
        // Rust retiene el sample buffer hasta que el frame se suelta.
        let status = frames.submit(sample, meta: FrameInfo(width: w, height: h, timestampNs: 0))
        if status.shouldStop { self?.stop() }
    }
}
```

`IstmoFrameSender` es una clase `Sendable` (los envíos se serializan),
así que los handlers de captura de cualquier cola pueden guardarla; la
sesión termina con `close()` / `close(error:)` o cuando el sender se
desinicializa. `submit(CVPixelBuffer | IOSurfaceRef | CMSampleBuffer,
meta:)` le dan a Rust una referencia retenida; `submitBytes(_:meta:)`
copia.

### Estados

Cada envío devuelve un `IstmoSubmitStatus`: `ACCEPTED`, `DISPLACED`
(aceptado; se desalojó un frame más viejo), `FULL` (rechazado), `CLOSED`,
`UNKNOWN_SINK`, `TYPE_MISMATCH` (el sender se generó para otro tipo de
metadatos), `BAD_META`, `UNSUPPORTED`, `NOT_STARTED`, `INVALID`,
`INTERNAL` (un bug de Rust; Rust libera lo que tomó). `isTaken` dice si
el sink se quedó el frame, `shouldStop` si todo envío posterior fallará
igual.

Todo pasa por un conjunto fijo de entradas del transporte
(`IstmoFrames.native*` en Android, `istmo_ios_*_frame_producer` e
`istmo_ios_submit_*` en Apple); ningún plugin añade símbolos JNI o C.
Los runtimes Kotlin y Swift comprueban la versión de entradas de la
biblioteca nativa al arrancar y rechazan una distinta con un mensaje que
nombra ambas versiones.

## Quién puede alimentar un sink

Los ids de sink son valores aleatorios de 64 bits, con sentido solo en
el runtime que los abrió. Un host generado rechaza una llamada que lleva
uno salvo que venga de código Rust del mismo proceso
(`DispatchError::OriginRefused` para Kotlin, Swift u otro proceso); un
cliente se niega a enviarlo a un plugin declarado remoto
(`IstmoError::FrameSinkNotShareable`); y los clientes Kotlin / Swift
generados no tienen métodos que reciban sinks. Un productor nativo
presenta una huella de su tipo de metadatos al abrir el sink, así uno
generado para otro tipo recibe `TYPE_MISMATCH` en vez de mandar bytes
que casualmente decodifican.

Los plugins que quieren consumidores en otro proceso ofrecen un camino
explícito con copia (un `#[stream]` de frames codificados) junto al sink.

## Costo

Build release, Ryzen 9 9950X3D, mismo proceso
(`crates/core/benches/stream_paths.rs`):

| | item de 16 B | item de 8 MiB |
|---|---|---|
| stream por el bus (codificar, rutear, decodificar) | ~390 ns | ~1,06 ms |
| atajo tipado de stream / frame sink | ~330–360 ns | ~65–70 µs (la asignación propia del productor) |
| frame sink, buffers con pool | | ~18 µs |

En un solo hilo, un push más su recepción cuesta ~50–60 ns (dos
asignaciones: el buffer y su `Vec`); el envío de un productor nativo, con
los metadatos decodificados, ~60 ns, y escala con los hilos como un
productor Rust (sin lock global).

## Siguiente

- Mover items de stream sin codificar: [Streams](/istmo/es/writing-plugins/streams/#atajo-en-proceso).
- Objetos que el lado nativo conserva: [Native handles](/istmo/es/writing-plugins/native-handles/).
