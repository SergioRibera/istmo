---
title: Contratos
description: Cómo Istmo describe la forma de un plugin para que Kotlin y Swift hablen con Rust usando código tipado.
sidebar:
  order: 2
---

Un **Contract** es la IR de Istmo para cómo se ve un plugin en el wire:
su identificador, las firmas de sus métodos, los items de sus streams y
cualquier tipo `#[istmo::message]` que esos métodos referencien.

Nunca escribes un `Contract` a mano. Se deriva de tu trait
`#[istmo::plugin]` en tiempo de build.

## Cuándo importan los contratos

Los contratos guían el **codegen del lado nativo**: son la entrada de
`generate_kotlin_host`, `generate_swift_client`, etc. Las apps 100% Rust
no los necesitan — el trait, una vez expandido por la macro, lleva toda
la información que Rust necesita.

Te importan los contratos si:

- Tu app tiene un directorio `android/` o `ios/` junto a `Cargo.toml`
  (entonces `istmo_build::emit()` recorre los contratos de las
  dependencias y regenera los dispatchers Kotlin/Swift).
- Publicas un crate de plugin (entonces tu `build.rs` emite tus
  contratos vía `DEP_<links>_ISTMO_CONTRACTS`).

## Cómo se emiten

Cada crate de plugin ejecuta `istmo_build::emit()` en su `build.rs`.
Por debajo, `emit()`:

1. Lee `istmo.toml` y encuentra `client_type = "::mycrate::FooClient"`.
2. Deriva el nombre del trait (`Foo`) quitando el sufijo `Client` del
   último segmento del path.
3. Parsea `src/lib.rs` (y los módulos que declara) con `syn`, encuentra
   `trait Foo` y construye un `Contract` con sus métodos y los tipos
   `#[istmo::message]` que usan.
4. Repite esto para cada entrada `[[plugin]]` con `client_type` y emite
   todos los contratos como una sola lista serializada con wincode y
   codificada en hex vía `cargo:ISTMO_CONTRACTS=<hex>` (Cargo guarda un
   solo valor por clave, así que un crate con varios plugins los
   entrega juntos).

Cargo lo reenvía a cada crate dependiente como
`DEP_<links>_ISTMO_CONTRACTS` — por eso cada crate de plugin declara
`links = "…"` en su `[package]`. El `build.rs` de tu app recorre esas
env vars con `collect_dep_contracts()` y pasa cada contrato a los
generadores. Si dos crates entregan el mismo id de plugin, se queda el
contrato del crate primero por nombre de `links`, con un
`cargo::warning`; un payload que no se puede decodificar se salta con un
warning.

Cero duplicación. El `Contract` no es algo que escribas; es un
subproducto del propio trait.

## Qué lleva un contrato

```rust
pub struct Contract {
    pub plugin_id: String,     // el `#[istmo::plugin(name = "…")]` del trait
    pub type_name: String,     // nombre del trait, p. ej. "Battery"
    pub methods: Vec<Method>,  // name, kind (Unary | Stream), args, returns, error
    pub init: Option<TypeRef>, // tipo de config de un plugin con estado (`init = Config`)
    pub types: Vec<TypeDef>,   // structs / enums con nombre que usan los métodos
}
```

El extractor convierte los nombres de argumentos y campos a `camelCase`
para el lado Kotlin/Swift (el wire depende del orden, así que el
renombre es cosmético), registra si cada método es unario o un
`#[istmo::stream]`, y guarda el `E` de un retorno `Result<T, E>` como
error tipado. Los argumentos `CancelToken` se descartan — nunca viajan
por el wire — y `Vec<u8>` se vuelve `Bytes` (`ByteArray` en Kotlin,
`Data` en Swift).

`extract_contract` valida el resultado, y el build falla nombrando el
método o tipo culpable cuando:

- un tipo con nombre no es ni un primitivo soportado ni un tipo
  `#[istmo::message]` del crate;
- un error de dominio `E` no es un tipo `#[istmo::message]`;
- un tipo anida `Option<Option<_>>` (Kotlin y Swift no distinguen los
  dos `None`) o pone `()` dentro de un `Vec` / `Option`;
- dos tipos message, o dos campos de un mismo struct, comparten nombre.

## Compartir tipos entre contratos

Cada contrato lleva sus propios tipos con nombre, y `emit()` escribe un
archivo `<T>Types` y uno `<T>CodecsImpl` por contrato. Cuando dos
contratos del mismo paquete deben compartir un tipo (un enum `Event`
que devuelven tanto `Timer` como `Notifier`), declara el tipo en un solo
contrato y llama a los generadores tú mismo: genera el segundo contrato
con `types = vec![]` y haz que use los codecs del primero. `emit()` no
lo hace por ti; el `build.rs` del ejemplo `android-demo` es la
referencia de la versión hecha a mano.

## Inspeccionar un contrato

Puedes imprimir cualquier contrato durante `build.rs` para depurar:

```rust
let contract = istmo_build::extract_contract("src/lib.rs", "Foo")
    .expect("extract Foo contract");
println!("cargo::warning=contract for Foo: {contract:#?}");
```

Cargo imprime el contrato en tu terminal en el siguiente build.

## Siguiente

- Mira `emit()` de punta a punta: [Visión general de build scripts](/istmo/es/build-scripts/emit-overview/).
- Aprende dónde vive el trait: [Native vs. Rust-hosted](/istmo/es/concepts/native-vs-rust-hosted/).
- Mira el formato de wire en el que se empaquetan: [Protocolo de frames](/istmo/es/concepts/frame-protocol/).
