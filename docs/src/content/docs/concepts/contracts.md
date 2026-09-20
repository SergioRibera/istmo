---
title: Contracts
description: How Istmo describes a plugin's shape so Kotlin and Swift can talk to Rust with typed code.
sidebar:
  order: 2
---

A **Contract** is Istmo's IR for what a plugin looks like on the wire:
its identifier, its method signatures, its stream items, and any
`#[istmo::message]` types those methods reference.

You never write a `Contract` by hand. It is derived from your
`#[istmo::plugin]` trait at build time.

## When contracts matter

Contracts drive **native-side codegen**: they are the input for
`generate_kotlin_host`, `generate_swift_client`, and so on. Rust-only
apps do not need them — the trait itself, once expanded by the macro,
carries all the information Rust needs.

You care about contracts if:

- Your app has an `android/` or `ios/` directory next to `Cargo.toml`
  (then `istmo_build::emit()` walks upstream contracts and regenerates
  Kotlin/Swift dispatchers).
- You publish a plugin crate (then your `build.rs` emits your contract
  via `DEP_<links>_ISTMO_CONTRACT`).

## How they get emitted

Every plugin crate runs `istmo_build::emit()` in `build.rs`. Under the
hood, `emit()`:

1. Reads `istmo.toml` and finds `client_type = "::mycrate::FooClient"`.
2. Derives the trait name (`Foo`) by stripping the `Client` suffix off
   the last path segment.
3. Parses `src/lib.rs` with `syn`, finds `trait Foo`, and builds a
   `Contract` from its methods and any sibling `#[istmo::message]`
   items.
4. Emits the contract as a hex-encoded, `bincode`-serialised payload
   via `cargo:CONTRACT=<hex>`.

Cargo forwards that to every downstream crate as
`DEP_<links>_ISTMO_CONTRACT`. Your app's `build.rs` walks all those env
vars via `collect_dep_contracts()` and feeds each contract to the
generators.

Zero duplication. The `Contract` is not something you author; it is a
by-product of the trait itself.

## What a contract carries

```rust
pub struct Contract {
    pub plugin_id: String,          // matches istmo.toml `[plugin] id`
    pub type_name: String,          // trait name, e.g. "Battery"
    pub methods: Vec<Method>,       // each entry: name, args, return, stream?
    pub init: Option<TypeRef>,      // stateful plugins declare their config here
    pub types: Vec<TypeDef>,        // named structs / enums referenced by args or returns
}
```

Method signatures preserve the arg names (converted to `camelCase` on
the Kotlin/Swift side to match idiomatic naming), whether the return is
`unary` or `stream`, and any typed error.

## Sharing types across contracts

Sometimes two contracts in the same package need to share a named type
(e.g. an `Event` enum that both `Timer` and `Notifier` return). Istmo
resolves this at the code-gen layer:

- One contract emits the `<T>Types.kt` / `<T>Types.swift` file.
- The sibling contract sets `types = vec![]` in its `Contract`, so no
  duplicate declarations are written.
- The sibling still emits its own dispatcher and codecs; it just
  imports the shared types from the primary contract.

The [`emit_app`](/istmo/build-scripts/emit-overview/) helper handles this
automatically when contracts come from the standard `DEP_*` handover.
For hand-built testbed contracts, you set `Contract::types = vec![]`
manually — see the `android-demo` example in the repo for a reference.

## Inspecting a contract

You can print any contract during `build.rs` for debugging:

```rust
let contract = istmo_build::extract_contract("src/lib.rs", "Foo")
    .expect("extract Foo contract");
println!("cargo::warning=contract for Foo: {contract:#?}");
```

Cargo prints the contract to your terminal on the next build.

## Next

- See `emit()` end-to-end: [Build scripts overview](/istmo/build-scripts/emit-overview/).
- Learn where the trait lives: [Native vs. Rust-hosted](/istmo/concepts/native-vs-rust-hosted/).
- See the wire format contracts are packed into: [Frame protocol](/istmo/concepts/frame-protocol/).
