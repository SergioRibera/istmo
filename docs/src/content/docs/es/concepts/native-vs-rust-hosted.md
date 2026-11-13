---
title: Nativo vs. Rust-hosted
description: Cuándo implementar el trait del plugin en Rust y cuándo en Kotlin o Swift.
sidebar:
  order: 3
---

Cada trait de plugin Istmo tiene una implementación. La elección de
**dónde** vive esa implementación — Rust o la plataforma nativa —
decide si tu plugin es Rust-hosted o native-hosted.

Ambos se ven idénticos desde el punto de vista del caller: sostienes
un `<T>Client`, llamas sus métodos, awaiteas el resultado. La
distinción solo importa cuando escribes el plugin.

## Native-hosted

**El trait se implementa en Kotlin y Swift.** Rust solo llama hacia
afuera.

Elige esto cuando el trabajo debe pasar en el runtime de la
plataforma:

- Pedir permisos.
- Mostrar UI del sistema (dialogs, share sheets, flows de sign-in).
- Leer sensores, batería, orientación.
- Hablar con un SDK nativo que no puedes reconstruir en Rust (Google
  Play Services, Live Activities, HealthKit).

El crate del plugin en Rust contribuye:

- La definición del trait (`#[istmo::plugin]`).
- El `Contract` (auto-derivado).
- Opcionalmente, backends nativos bajo `native/{android,ios}/` (las
  fuentes Android en el `android_package` del plugin), que el plugin de
  Gradle y el fragmento xcodegen `istmo-plugins.yml` compilan en cada
  app que depende del crate.

La app contribuye:

- Un `<T>BackendImpl` concreto (Kotlin/Swift) que habla con la
  plataforma, cuando el plugin no trae uno para esa plataforma.
- Nada para el registro: el `IstmoPluginRegistry` generado, que llaman
  `IstmoGameActivity` / `IstmoHost.onCreate` e `IstmoApp.run()`,
  engancha en `IstmoRuntime` cada backend que el plugin trae o declara.
  Un backend que la app escribe para un plugin que no trae ninguno se
  registra cuando la app lo pide con `[[app.plugin]] auto_register =
  true` (ver [Auto-registro](/istmo/es/build-scripts/auto-register/#qué-significa-elegible)).

Plugins de referencia: [`google-sign-in`](/istmo/es/plugins/google-sign-in/),
[`data-store`](/istmo/es/plugins/data-store/),
[`live-activity`](/istmo/es/plugins/live-activity/). [`biometric`](/istmo/es/plugins/biometric/)
combina ambos: native-hosted en Android / iOS, Rust-hosted en escritorio.

## Rust-hosted

**El trait se implementa en Rust.** El crate Rust envía la lógica
real; Kotlin/Swift opcionalmente llaman *hacia adentro*.

Elige esto cuando:

- El trabajo es computación pura (procesamiento de imágenes,
  criptografía, parsing).
- Quieres compartir lógica de negocio entre todas las plataformas,
  incluyendo desktop y targets CLI headless.
- Necesitas exactamente el mismo comportamiento byte-por-byte en
  Android e iOS.

El crate Rust del plugin contribuye:

- La definición del trait **y** su impl concreto:
  ```rust
  #[istmo::plugin(name = "acme.image_hasher")]
  pub trait ImageHasher {
      async fn perceptual_hash(&self, bytes: Vec<u8>) -> Result<u64, HashError>;
  }

  pub struct ImageHasherImpl;

  impl ImageHasher for ImageHasherImpl {
      async fn perceptual_hash(&self, bytes: Vec<u8>) -> Result<u64, HashError> {
          // trabajo real aquí
      }
  }
  ```
- Registro en la declaración del runtime de la app, que envuelve la
  impl en el `ImageHasherHost` generado:
  ```rust
  istmo::runtime!(
      hosts: [ImageHasher => ImageHasherImpl],
  );
  ```
  (un binario de escritorio puede en su lugar llamar a
  `runtime.register_host(ImageHasherHost::new(ImageHasherImpl))`).

La app contribuye:

- **Nada del lado nativo, a menos que Kotlin/Swift quieran llamar al
  plugin directamente.** Para eso habilitas el override app-side
  `[[app.plugin]] role = "client"` para que Istmo genere un stub
  `<T>Client.kt` / `<T>Client.swift`.

## Matriz de decisión

|                       | Native-hosted                              | Rust-hosted                                |
| --------------------- | ------------------------------------------ | ------------------------------------------ |
| Código real vive en   | Kotlin / Swift                             | Rust                                       |
| Rust envía            | Trait, contract, docs, impl de referencia  | Trait, contract, impl real                 |
| Nativo envía          | Impl real (dir `native/` o tu app)         | Stub cliente opcional                      |
| Caso típico           | SDKs de plataforma, permisos, sensores     | Lógica de negocio, algoritmos, estado     |
| `role` del codegen    | `host` (default)                           | `client` (opt-in por plugin)               |
| Threading             | Runtime nativo, `suspend fun`/`async`      | Rust `async` en cualquier executor         |

## Cuál vas a escribir?

Para el 90% de plugins app-specific escribes native-hosted que envuelven
APIs de plataforma, porque ahí es donde las apps móviles gastan su
presupuesto de integración. Cuando te descubras escribiendo el mismo
Kotlin y Swift dos veces, esa lógica quiere bajar a Rust y
convertirse en un plugin Rust-hosted.

## Siguiente

- Native-hosted end-to-end: [Escribir un plugin native-hosted](/istmo/es/writing-plugins/native-hosted/).
- Rust-hosted end-to-end: [Escribir un plugin Rust-hosted](/istmo/es/writing-plugins/rust-hosted/).
