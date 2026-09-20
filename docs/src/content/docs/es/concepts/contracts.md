---
title: Contratos
description: Cómo Istmo describe la forma de un plugin para que Kotlin y Swift hablen con Rust usando código tipado.
sidebar:
  order: 2
---

Un **Contract** es la IR de Istmo para cómo se ve un plugin en el
wire: su identificador, sus signatures de método, sus items de stream,
y cualquier tipo `#[istmo::message]` que esos métodos referencien.

Nunca escribes un `Contract` a mano. Se deriva de tu trait
`#[istmo::plugin]` en build time.

## Cuándo importan los contratos

Los contratos guían el **codegen del lado nativo**: son el input para
`generate_kotlin_host`, `generate_swift_client`, etc. Apps 100% Rust
no los necesitan — el trait, una vez expandido por la macro, carga
toda la información que Rust necesita.

Te importan los contratos si:

- Tu app tiene un directorio `android/` o `ios/` al lado de
  `Cargo.toml` (entonces `istmo_build::emit()` camina contratos
  upstream y regenera dispatchers Kotlin/Swift).
- Publicas un crate de plugin (entonces tu `build.rs` emite tu
  contrato vía `DEP_<links>_ISTMO_CONTRACT`).

## Cómo se emiten

Cada crate de plugin corre `istmo_build::emit()` en `build.rs`. Bajo
el capó, `emit()`:

1. Lee `istmo.toml` y encuentra
   `client_type = "::mycrate::FooClient"`.
2. Deriva el nombre del trait (`Foo`) quitando el sufijo `Client` del
   último segmento del path.
3. Parsea `src/lib.rs` con `syn`, encuentra `trait Foo`, y construye
   un `Contract` desde sus métodos y cualquier item hermano
   `#[istmo::message]`.
4. Emite el contrato como payload hex-encoded y `bincode`-serialized
   vía `cargo:CONTRACT=<hex>`.

Cargo lo forwardea a cada crate downstream como
`DEP_<links>_ISTMO_CONTRACT`. El `build.rs` de tu app camina esos env
vars via `collect_dep_contracts()` y alimenta cada contrato a los
generadores.

Cero duplicación. El `Contract` no es algo que autores; es un
subproducto del trait.

## Qué carga un contrato

```rust
pub struct Contract {
    pub plugin_id: String,          // matchea istmo.toml `[plugin] id`
    pub type_name: String,          // nombre del trait, ej. "Battery"
    pub methods: Vec<Method>,       // cada entrada: name, args, return, stream?
    pub init: Option<TypeRef>,      // plugins stateful declaran su config aquí
    pub types: Vec<TypeDef>,        // structs/enums nombrados referenciados por args o returns
}
```

Los signatures de método preservan los nombres de args (convertidos a
`camelCase` del lado Kotlin/Swift para matchear naming idiomático), si
el return es `unary` o `stream`, y cualquier error tipado.

## Compartir tipos entre contratos

A veces dos contratos en el mismo paquete necesitan compartir un tipo
nombrado (ej. un `Event` enum que retornan `Timer` y `Notifier`). Istmo
resuelve esto en la capa de codegen:

- Un contrato emite el archivo `<T>Types.kt` / `<T>Types.swift`.
- El contrato hermano fija `types = vec![]` en su `Contract`, así no
  se escriben declaraciones duplicadas.
- El hermano igual emite su propio dispatcher y codecs; solo importa
  los tipos compartidos del contrato primario.

El helper [`emit_app`](/istmo/es/build-scripts/emit-overview/) maneja esto
automáticamente cuando los contratos vienen del handover estándar
`DEP_*`. Para contratos inline de testbed, fijas
`Contract::types = vec![]` manualmente — ver el ejemplo `android-demo`
en el repo como referencia.

## Inspeccionar un contrato

Puedes imprimir cualquier contrato durante `build.rs` para debug:

```rust
let contract = istmo_build::extract_contract("src/lib.rs", "Foo")
    .expect("extract Foo contract");
println!("cargo::warning=contract for Foo: {contract:#?}");
```

Cargo imprime el contrato en tu terminal en el próximo build.

## Siguiente

- Ver `emit()` end-to-end: [Vista general de build scripts](/istmo/es/build-scripts/emit-overview/).
- Aprende dónde vive el trait: [Native vs. Rust-hosted](/istmo/es/concepts/native-vs-rust-hosted/).
- Ver el formato del wire en el que se empaquetan los contratos: [Frame protocol](/istmo/es/concepts/frame-protocol/).
