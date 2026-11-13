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
- You publish a plugin crate (then your `build.rs` emits your contracts
  via `DEP_<links>_ISTMO_CONTRACTS`).

## How they get emitted

Every plugin crate runs `istmo_build::emit()` in `build.rs`. Under the
hood, `emit()`:

1. Reads `istmo.toml` and finds `client_type = "::mycrate::FooClient"`.
2. Derives the trait name (`Foo`) by stripping the `Client` suffix off
   the last path segment.
3. Parses `src/lib.rs` (and the modules it declares) with `syn`, finds
   `trait Foo`, and builds a `Contract` from its methods and the
   `#[istmo::message]` types they use.
4. Repeats this for every `[[plugin]]` entry with a `client_type` and
   emits all the contracts as one hex-encoded, wincode-serialised list
   via `cargo:ISTMO_CONTRACTS=<hex>` (Cargo keeps one value per key, so
   a crate with several plugins hands them over together).

Cargo forwards that to every downstream crate as
`DEP_<links>_ISTMO_CONTRACTS` — which is why every plugin crate sets
`links = "…"` in its `[package]`. Your app's `build.rs` walks those env
vars via `collect_dep_contracts()` and feeds each contract to the
generators. A plugin id handed over by two crates keeps the contract of
the crate first by `links` name, with a `cargo::warning`; a payload that
fails to decode is skipped with a warning.

Zero duplication. The `Contract` is not something you author; it is a
by-product of the trait itself.

## What a contract carries

```rust
pub struct Contract {
    pub plugin_id: String,     // the trait's `#[istmo::plugin(name = "…")]`
    pub type_name: String,     // trait name, e.g. "Battery"
    pub methods: Vec<Method>,  // name, kind (Unary | Stream), args, returns, error
    pub init: Option<TypeRef>, // config type of a stateful plugin (`init = Config`)
    pub types: Vec<TypeDef>,   // named structs / enums the methods reference
}
```

The extractor folds argument and field names to `camelCase` for the
Kotlin/Swift side (the wire is order-based, so the rename is cosmetic),
records whether each method is unary or a `#[istmo::stream]`, and keeps
the `E` of a `Result<T, E>` return as the typed error. `CancelToken`
arguments are dropped — they never travel on the wire — and `Vec<u8>`
becomes `Bytes` (`ByteArray` in Kotlin, `Data` in Swift).

`extract_contract` validates the result, and the build fails naming the
offending method or type when:

- a named type is neither a supported primitive nor an
  `#[istmo::message]` type of the crate;
- a domain error `E` is not an `#[istmo::message]` type;
- a type nests `Option<Option<_>>` (Kotlin and Swift cannot tell the two
  `None`s apart) or puts `()` inside a `Vec` / `Option`;
- two message types, or two fields of one struct, share a name.

## Sharing types across contracts

Each contract carries its own named types, and `emit()` writes one
`<T>Types` and one `<T>CodecsImpl` file per contract. When two contracts
in the same package must share a named type (an `Event` enum that both
`Timer` and `Notifier` return), declare the type in one contract only
and call the generators yourself: generate the second contract with
`types = vec![]` and let it use the first contract's codecs. `emit()`
does not do this for you; the `android-demo` example's `build.rs` is the
reference for the hand-rolled version.

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
