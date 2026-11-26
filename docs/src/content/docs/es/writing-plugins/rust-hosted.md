---
title: Plugin Rust-hosted
description: Envía la implementación del trait en Rust; Kotlin y Swift opcionalmente llaman hacia adentro.
sidebar:
  order: 1
---

Un plugin Rust-hosted pone el trabajo real del trait en Rust. El lado
nativo o no toca el plugin (código Rust lo llama), o sostiene un
`<T>Client` generado y llama hacia adentro cuando necesita un
resultado.

## Cuándo elegirlo

- Computación pura — hashing, parsing, criptografía, procesamiento de
  imágenes.
- Lógica de negocio que quieres byte-idéntica en cada plataforma.
- Código que ya tienes en Rust que preferirías dejar quieto.

Si tu plugin necesita hablar con un SDK nativo, pedir permisos, o
tocar el main thread de la plataforma, probablemente quieras un
[plugin native-hosted](/istmo/es/writing-plugins/native-hosted/) en su
lugar.

:::note[Facade o crates core]
Los snippets de estas páginas usan el facade `istmo`, como lo hace el
código dentro de un crate de app. Un crate de plugin independiente
depende de `istmo-core` + `istmo-macros` (el facade elige el sabor de
activity Android, que es decisión de la app) y pasa
`crate = "::istmo_core"` a `#[plugin]` y `wincode = "::wincode"` a
`#[message]` — ver
[Tu primer plugin](/istmo/es/getting-started/first-plugin/).
:::

## Skeleton

```rust
// image-hasher/src/lib.rs
use istmo::plugin;

#[plugin(name = "acme.image_hasher")]
pub trait ImageHasher {
    async fn perceptual_hash(&self, bytes: Vec<u8>) -> Result<u64, HashError>;
    async fn dimensions(&self, bytes: Vec<u8>) -> Result<Dimensions, HashError>;
}

#[istmo::message]
pub struct Dimensions {
    pub width: u32,
    pub height: u32,
}

#[istmo::message]
#[derive(Debug)]
pub struct HashError {
    pub reason: String,
}

impl core::fmt::Display for HashError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "hasher error: {}", self.reason)
    }
}
impl std::error::Error for HashError {}
```

El tipo de error de un retorno `Result` debe implementar
`std::error::Error` (de ahí el derive de `Debug`); la macro lo
comprueba.

`istmo.toml`:

```toml
[plugin]
id          = "acme.image_hasher"
client_type = "::image_hasher::ImageHasherClient"
```

No hace falta `auto_register = false`: el `IstmoPluginRegistry`
generado sólo registra plugins con un backend nativo (un directorio
`native/<plataforma>/`, `android_backend` / `ios_backend`, o un opt-in
de la app), y este crate no trae ninguno. Ver
[Auto-registro](/istmo/es/build-scripts/auto-register/#qué-significa-elegible).

`build.rs`:

```rust
fn main() { istmo_build::emit(); }
```

## Implementa el trait

Justo al lado de la definición del trait:

```rust
pub struct ImageHasherImpl;

impl ImageHasher for ImageHasherImpl {
    async fn perceptual_hash(&self, bytes: Vec<u8>) -> Result<u64, HashError> {
        // tu lógica real de hashing
        Ok(compute_phash(&bytes))
    }

    async fn dimensions(&self, bytes: Vec<u8>) -> Result<Dimensions, HashError> {
        let (w, h) = probe(&bytes).map_err(|e| HashError {
            reason: e.to_string(),
        })?;
        Ok(Dimensions { width: w, height: h })
    }
}
```

`#[istmo::plugin]` convierte cada `async fn` del trait en
`impl Future + Send + '_`, así que lo implementas con `async fn` plano.
No hace falta `#[async_trait]`.

## Conecta el host al runtime

En el crate de la app:

```rust
istmo::runtime!(
    hosts: [image_hasher::ImageHasher => image_hasher::ImageHasherImpl],
);
```

Cada entrada de `hosts:` es `Trait => expresión`: la macro registra
`<Trait>Host::new(expresión)`, así que `image_hasher::ImageHasherHost`
tiene que resolverse. El cliente sale del `client_type` del plugin vía
[auto-wiring](/istmo/es/advanced/auto-wiring/). Un plugin declarado
dentro del crate de la app no tiene manifest, así que lista también su
cliente — `plugins: [ImageHasherClient]` — o las llamadas fallan con
`IstmoError::PluginNotDeclared`.

## Llámalo desde Rust

En cualquier lugar donde tengas un `Arc<Runtime>`:

```rust
use image_hasher::ImageHasherClient;

let hasher = ImageHasherClient::from_runtime(&runtime)?;
let phash = hasher.perceptual_hash(pixels).await?;
```

Cada método del cliente retorna `Result<T, IstmoError>`. Un error de
dominio (`Err(HashError)` desde la implementación) llega como
`IstmoError::PluginError { bytes }`:

```rust
match hasher.dimensions(pixels).await {
    Ok(dims) => { /* ... */ }
    Err(err) => match err.domain_error::<HashError>() {
        Some(hash_error) => log::warn!("{hash_error}"),
        None => return Err(err.into()),
    },
}
```

## Llamadas desde el mismo proceso

Un host registrado con `runtime!(hosts: …)`, `RuntimeInit::host` o
`Runtime::register_host` instala también un **atajo tipado en proceso**
(plugins sin estado): un cliente del mismo proceso llama a la
implementación directamente — sin codificar, sin rutear, sin hilo de
dispatcher — y hace poll de su future en la propia tarea del llamador.
`client.is_local()` dice si ocurre. Las llamadas desde Kotlin, Swift u
otro proceso siguen por el wire, así que ambos caminos conviven.

El wire tampoco salta de hilo: una llamada desde Kotlin o Swift se
ejecuta en el hilo que la envió (el hilo del llamador en Kotlin, el
principal incluido; en iOS la tarea del llamador en Swift), y la que
termina sin esperar se responde antes de que ese hilo regrese. Un
método que espera (un `.await` sobre otra llamada, un temporizador, un
canal) libera ese hilo en su primera espera y termina en uno de los
pocos workers asíncronos del runtime. Ninguna llamada crea un hilo. Lo
mismo vale para los plugins con estado (`init = …`), que no tienen
atajo: una llamada desde este proceso ejecuta el método dentro de
`Runtime::call`, en el hilo del llamador, hasta su primera espera. Las
notificaciones corren como las llamadas.

Una llamada que vuelve a entrar al runtime corre en la misma pila: un
host que llama a un plugin de Kotlin desde su cuerpo puede ejecutar ese
backend de Kotlin en su propio hilo antes de que `Runtime::call`
regrese, y el backend puede volver a llamar a Rust. No sostengas a
través de esa llamada un lock que la llamada anidada pueda tomar.

La implementación corre donde está el llamador, lo que le pide dos
cosas:

- **No bloquear.** Un método que espera en un hilo (un diálogo síncrono,
  `std::fs` sobre archivos grandes, `thread::sleep`, un salto síncrono
  al hilo principal) o que solo corre en un executor (necesita un
  reactor de tokio, por ejemplo) detendría o haría fallar al llamador, o
  a un worker asíncrono que comparten todas las llamadas en espera.
  Márcalo `#[istmo::blocking]`: sus llamadas se codifican y se atienden
  en uno de los workers bloqueantes persistentes del runtime, que sí
  pueden bloquear, volver a llamar al runtime y esperar (se reusa un
  worker libre, o se arranca uno nuevo si no hay, así que las llamadas
  bloqueantes anidadas nunca esperan el hilo de otra).

  ```rust
  #[istmo::plugin(name = "acme.picker")]
  pub trait Picker {
      #[istmo::blocking]
      async fn pick(&self) -> Option<String>;
  }
  ```

- **Cancelar mediante el token.** Soltar la llamada suelta el future. Un
  método que recibe un `CancelToken` lo ve dispararse y aún termina su
  limpieza (el runtime pasa el future abandonado a sus workers
  asíncronos, como lo habría hecho el dispatcher); uno sin token
  simplemente se detiene en su último `.await`.

Para dejar todo un plugin en el wire, declara
`#[plugin(name = "…", shortcut = false)]`; el wire igual ejecuta cada
método donde dice su modo, así que esto solo no saca nada del hilo del
llamador. Para que todas las llamadas de un registro corran en un worker
bloqueante, marca la entrada
`runtime!(hosts: [#[wire_only] Picker => DesktopPicker])` (o llama a
`Runtime::register_host_wire_only`).

El runtime nunca bloquea con el canal saliente lleno un hilo del que
puede depender el avance de ese canal (el pump de un transporte, el
lector de un puente entre procesos, un hilo que entrega frames a un
sink, un envío síncrono en un worker asíncrono): lo que ese hilo envía,
incluido un host inline al que hace poll, se encola detrás del canal en
orden, y un hilo de drenaje lo mueve. Cualquier otro hilo, tuyo o un
llamador de Kotlin / Swift que hace poll de un host inline, espera
lugar: la contrapresión que da el canal. La cola no tiene límite;
`Runtime::outbound_spill()` la cuenta y el runtime avisa cuando pasa de
cuatro canales. Un transporte marca un hilo así con
`istmo::DeliveryScope`.

### Quién llamó

Un argumento de tipo `istmo::HostCtx` lo rellena el host y nunca cruza
el wire. Le dice a la implementación quién llamó (`ctx.origin()`:
`Local`, `Peer` — Kotlin o Swift en este proceso — o `Remote`) y alcanza
el runtime por el que llegó la llamada (`ctx.runtime()`,
`ctx.frame_sender(sink)` para
[frame sinks](/istmo/es/writing-plugins/frame-sinks/)).

```rust
#[istmo::plugin(name = "acme.vault")]
pub trait Vault {
    async fn read(&self, ctx: istmo::HostCtx, key: String) -> Option<String>;
}
```

## Opcionalmente, llámalo desde Kotlin o Swift

Si el lado nativo necesita invocar el plugin, actívalo con un override
del lado de la app:

```toml
# istmo.toml (en la raíz del crate de la app)
[[app.plugin]]
id   = "acme.image_hasher"
role = "client"
```

El siguiente `cargo build` genera `ImageHasherClient.kt`,
`ImageHasherCodecs.kt`, `ImageHasherCodecsImpl.kt` e
`ImageHasherTypes.kt` en el paquete Kotlin de la app, y
`ImageHasherClient.swift` con sus codecs y tipos bajo
`ios/<App>/Plugins/ImageHasher/Generated/`. Los métodos conservan el
nombre de Rust:

```kotlin
val hasher = ImageHasherClient(ImageHasherCodecsImpl())
val phash: ULong = hasher.perceptual_hash(bytes)
```

```swift
let hasher = ImageHasherClient(codecs: ImageHasherCodecsImpl())
let phash = try await hasher.perceptual_hash(bytes: data)
```

Los errores de dominio se lanzan como `ImageHasherException`, con el
payload crudo y el `HashError` decodificado cuando decodifica.

## Testing

Los plugins Rust-hosted se testean como Rust async plano; cualquier
executor maneja los futures:

```rust
#[test]
fn hashes_a_png() {
    let png = include_bytes!("../tests/sample.png").to_vec();
    let out = pollster::block_on(ImageHasherImpl.perceptual_hash(png)).unwrap();
    assert_ne!(out, 0);
}
```

Para tests de integración, registra el host en un runtime mock y llama
al cliente igual que lo haría tu app:

```rust
let rt = istmo::Runtime::mock()
    .host(ImageHasherHost::new(ImageHasherImpl))
    .runtime;
let hasher = ImageHasherClient::from_runtime(&rt)?;
let phash = pollster::block_on(hasher.perceptual_hash(png))?;
```

## Siguiente

- Agrega un stream: [Streams](/istmo/es/writing-plugins/streams/).
- Cancelación cooperativa: [Cancelación](/istmo/es/writing-plugins/cancellation/).
- Si realmente quieres hablar con Kotlin/Swift: [Plugin native-hosted](/istmo/es/writing-plugins/native-hosted/).
