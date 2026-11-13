---
title: Auto-wiring
description: Cómo la macro runtime! toma los plugins de tus dependencias de Cargo sin una lista manual.
sidebar:
  order: 3
---

`istmo::runtime!` soporta **auto-wiring transparente** al estilo
Flutter. Agregas un plugin a tu `Cargo.toml` y aparece en el runtime —
sin lista manual en `plugins:`, sin línea `use`.

El truco usa el mecanismo de env vars `DEP_*` de Cargo,
`istmo_build::emit()` en `build.rs`, y la lectura de env vars en el
momento de expandir la macro. Esta página explica el mecanismo para que
puedas depurarlo o extenderlo.

## El mecanismo, de punta a punta

### 1. `build.rs` del plugin

```rust
fn main() { istmo_build::emit(); }
```

`emit()` lee `istmo.toml` e imprime el manifest parseado como
`cargo:ISTMO_MANIFEST=<hex>`. Como el crate del plugin declara
`links = "…"` en su `[package]`, Cargo lo reexporta a cada crate que
depende de él directamente como `DEP_<links>_ISTMO_MANIFEST`.

### 2. `build.rs` de la app

```rust
fn main() { istmo_build::emit(); }
```

El `emit()` del lado de la app recorre cada env var
`DEP_*_ISTMO_MANIFEST`, decodifica el manifest, aplica cualquier
`[[remote_override]]` del `istmo.toml` propio de la app y escribe los
tipos client que encontró:

- `cargo::rustc-env=ISTMO_AUTO_PLUGINS=::istmo_google_sign_in::SignInClient,::istmo_data_store::DataStoreClient`
- `cargo::rustc-env=ISTMO_AUTO_REMOTE=::acme_model::LargeModelClient`

Las dos se definen siempre, vacías cuando no aplica nada. `rustc-env`
las hace visibles para las macros mientras compila el crate de la app.

### 3. La macro `runtime!`

Al expandirse, `istmo::runtime!` lee `ISTMO_AUTO_PLUGINS` e
`ISTMO_AUTO_REMOTE`, parsea cada entrada como un path de Rust y la
agrega a lo que escribiste:

```rust
// Tu código:
istmo::runtime!(
    plugins: [MyOwnClient],
);

// Después de expandir (conceptualmente):
pub fn __istmo_configure_runtime(init: RuntimeInit) -> RuntimeInit {
    init.expects::<MyOwnClient>()
        .expects::<::istmo_google_sign_in::SignInClient>()
        .expects::<::istmo_data_store::DataStoreClient>()
        .remotes::<::acme_model::LargeModelClient>()
        .finish()
}
```

Un plugin listado a mano y también auto-conectado se declara una sola
vez — el runtime guarda sus declaraciones en un set, así que escribir
ambos es seguro. La expansión además nombra las dos variables con
`option_env!`, así que una lista que cambia recompila el crate.

## Qué se auto-conecta

Sólo los plugins que:

- Vienen de una dependencia de Cargo **directa** cuyo crate ejecuta
  `istmo_build::emit()` y declara `links = "…"`.
- Tienen `client_type` en su `istmo.toml`.

Los plugins sin `client_type` (sólo tipos compartidos, o un shim de
servicio / background) se omiten — no hay client que declarar.

## Destino del deployment

Cada plugin tiene un `default_deployment` — `local` o `remote`. El
auto-wiring lo respeta:

- `local` → el client va a `ISTMO_AUTO_PLUGINS` (`expects`).
- `remote` → el client va a `ISTMO_AUTO_REMOTE` (`remotes`).

`[[remote_override]]` del lado de la app cambia dónde va un plugin sin
tocar el crate del plugin.

## Desactivarlo

No hay un flag para "saltar" un plugin — se declara cada plugin con
`client_type` entre tus dependencias directas. Declarar un plugin sólo
permite que el runtime lo llame; no se construye nada hasta que
obtienes su client.

Para quitar el auto-wiring por completo, llama a la pasada de la app sin
el paso de wiring y lista cada plugin tú mismo:

```rust
// build.rs (en la raíz del crate de la app)
fn main() {
    // Sólo codegen + sincronización de proyectos nativos: ISTMO_AUTO_*
    // quedan sin definir y se omite la comprobación de [min_versions].
    istmo_build::emit_app();
}
```

```rust
// src/lib.rs (en la raíz del crate de la app)
istmo::runtime!(
    plugins: [SignInClient, DataStoreClient],   // sólo estos dos
);
```

## Depuración

Los fallos de auto-wiring suelen ser uno de estos:

- **El plugin no tiene `client_type`** — revisa el `istmo.toml` del
  plugin.
- **El crate del plugin no tiene `links = "…"`** — Cargo sólo reenvía
  metadatos de build scripts de crates que declaran `links`.
- **El plugin es una dependencia transitiva** — las variables `DEP_*`
  sólo llegan a los dependientes directos; agrega el plugin a tu propio
  `Cargo.toml`.
- **`istmo::runtime!` se expande en un crate cuyo `build.rs` no llama a
  `emit()`** — las env vars sólo se definen para el crate cuyo build
  script las imprime.

Imprime el wiring efectivo durante el build:

```rust
// build.rs
fn main() {
    istmo_build::emit();

    let app = istmo_build::Manifest::from_path("istmo.toml").ok();
    let wiring = istmo_build::resolve_wiring(
        &istmo_build::collect_dep_manifests(),
        app.as_ref(),
    );
    println!("cargo::warning=local: {:?}", wiring.local_clients);
    println!("cargo::warning=remote: {:?}", wiring.remote_clients);
}
```

## ¿Por qué env vars y no `inventory`?

`inventory` / `linkme` recolectan en tiempo de link — cada crate se
registra en una tabla global vía una sección del linker. Funciona, pero:

- Las librerías estáticas (el `staticlib` de iOS) pueden perder un
  registro al que el linker no ve ninguna referencia.
- El manejo de secciones cambia según linker y plataforma.
- `runtime!` guiado por env vars se resuelve por completo en
  `cargo build` — sin intervención del linker.

La contrapartida es que el auto-wiring sólo ve plugins de dependencias
de Cargo **directas**.

## Siguiente

- Forma del setup del runtime: [Conceptos → Arquitectura](/istmo/es/concepts/architecture/).
- Desplegar en `:remote`: [Avanzado → Proceso remoto](/istmo/es/advanced/remote-process/).
