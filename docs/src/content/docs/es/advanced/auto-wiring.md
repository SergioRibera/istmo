---
title: Auto-wiring
description: Cómo la macro runtime! recoge plugins de tus deps de Cargo sin una lista manual.
sidebar:
  order: 3
---

`istmo::runtime!` soporta **auto-wiring transparente** estilo Flutter.
Añades un plugin a tu `Cargo.toml`, y aparece en el runtime — sin
lista manual `plugins:`, sin línea `use`.

El truco usa el mecanismo `DEP_*` env var de Cargo, `emit_wiring_env`
en `build.rs`, y pickup de env vars al expandir la macro. Esta página
explica el mecanismo para que puedas debuggear o extenderlo.

## El mecanismo, end-to-end

### 1. `build.rs` del plugin

```rust
fn main() { istmo_build::emit(); }
```

`emit()` lee `istmo.toml`, emite el manifest a
`cargo:ISTMO_MANIFEST=<hex>`, que Cargo re-exporta a cada dependent
como `DEP_<links>_ISTMO_MANIFEST`.

### 2. `build.rs` de la app

```rust
fn main() { istmo_build::emit(); }
```

El `emit()` app-side — via `emit_wiring_env` — camina cada env var
`DEP_*_ISTMO_MANIFEST`, decoda el manifest, aplica cualquier
`[[remote_override]]` del `istmo.toml` propio de la app, y escribe:

- `cargo::rustc-env=ISTMO_AUTO_PLUGINS=Foo,Bar,Baz`
- `cargo::rustc-env=ISTMO_AUTO_REMOTE=Qux`

Cargo hace estas env vars disponibles al compile time (scope
`rustc-env`).

### 3. Macro `runtime!`

Al expandir, `istmo::runtime!` lee `ISTMO_AUTO_PLUGINS` y
`ISTMO_AUTO_REMOTE` via `std::env::var_os`, parsea cada entry como
`syn::Path`, y la agrega a lo que el usuario escribió:

```rust
// Tu source:
istmo::runtime!(
    plugins: [MyOwnClient],
);

// Post-expansión (conceptualmente):
Runtime::new(RuntimeInit {
    plugins: vec![MyOwnClient::expect(), SignInClient::expect(), DataStoreClient::expect()],
    remotes: vec![],
    services: vec![],
    workers: vec![],
})
```

Los duplicados (un plugin listado tanto manualmente como
auto-wireado) se colapsan via HashSet — safe escribir ambos.

## Qué se auto-wirea

Sólo plugins que:

- Tienen `client_type` seteado en su `istmo.toml`.
- Vienen de una dep de Cargo que envía `istmo.toml`.
- No están opt-out (ver abajo).

Plugins que sólo cargan tipos compartidos (sin `client_type`) se
saltean — no necesitan registración en runtime.

## Target de deployment

Cada plugin tiene un `default_deployment` — `local` o `remote`. El
auto-wiring lo respeta:

- `local` → el cliente va a `plugins`.
- `remote` → el cliente va a `remote`.

`[[remote_override]]` app-side flippea la ubicación de un plugin sin
tocar el crate del plugin.

## Opt-out por plugin

No hay un flag built-in "saltea este plugin" hoy — cada plugin en tu
grafo de dependencias auto-wirea. Si querés registrar
condicionalmente, dropea auto-wiring y lista cada plugin manualmente:

```rust
// build.rs (en la raíz del crate app) — skipea emit_wiring_env entero
fn main() {
    istmo_build::emit_manifest_metadata_with_contract("istmo.toml", &contract);
    // sin emit_wiring_env → ISTMO_AUTO_* nunca seteado → runtime! sólo ve lista manual
}
```

O:

```rust
// src/lib.rs (en la raíz del crate app)
istmo::runtime!(
    plugins: [SignInClient, DataStoreClient],   // sólo estos dos — omitir los demás
);
```

## Debugging

Los fallos de auto-wiring usualmente son alguno de:

- **El plugin no tiene `client_type`** — chequeá el `istmo.toml` del
  plugin.
- **La dep del plugin no tiene `links = "…"`** — Cargo dropea
  emisiones de metadata cuando falta `links`. Cada crate plugin debe
  cargar `links` en `[package]`.
- **`istmo::runtime!` se expande en un crate que no corre
  `emit_wiring_env`** — las env vars están seteadas sólo para el
  crate cuyo `build.rs` la llama.

Imprime el wiring efectivo durante build:

```rust
let resolved = istmo_build::emit_wiring_env(Some(&std::path::Path::new("istmo.toml")));
println!("cargo::warning=local: {:?}", resolved.local_clients);
println!("cargo::warning=remote: {:?}", resolved.remote_clients);
```

## Por qué env vars y no `inventory`?

`inventory` / `linkme` hacen collection en link-time — cada crate se
registra en una tabla global via una linker section. Funciona, pero:

- Builds debug pegan a quirks LLD/gold alrededor de merging de
  sections.
- Targets static-library (iOS) a veces dropean sections sin una
  referencia.
- `runtime!` con env vars es completamente determinístico al tiempo
  de `cargo build` — sin involvement de linker.

El trade-off es que auto-wiring sólo ve plugins de deps de Cargo
**directas**. Plugins transitivos no se auto-wirean — necesitas
depender de ellos vos mismo si los quieres registrados.

## Siguiente

- Forma de setup de runtime: [Conceptos → Arquitectura](/istmo/es/concepts/architecture/).
- Deployment a `:remote`: [Avanzado → Proceso remoto](/istmo/es/advanced/remote-process/).
