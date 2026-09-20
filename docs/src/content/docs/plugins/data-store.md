---
title: Data Store
description: Cross-platform key/value storage backed by SharedPreferences on Android and UserDefaults on iOS.
sidebar:
  order: 2
---

`istmo-data-store` is a native-hosted key/value store. The Rust API is
identical on every platform; the backend uses `SharedPreferences` on
Android and `UserDefaults` on iOS. The two backends ship in the plugin
crate under `native/{android,ios}/` with zero external dependencies.

## Install

```toml
[dependencies]
istmo-data-store = "0.1"
```

## Use it

`DataStore` is stateful (`init = DataStoreConfig`) — the config carries
the namespace the backend segregates values by:

```rust
use istmo_data_store::{DataStoreClient, DataStoreConfig};

let cfg = DataStoreConfig::new("com.example.app");
let store = DataStoreClient::from_runtime_with(&runtime, cfg).await?;

store.set_string("user.email".into(), "iris@example.com".into()).await?;
store.set_bool("onboarding.completed".into(), true).await?;
store.set_i64("last_sync_ms".into(), now_ms()).await?;

let email = store.get_string("user.email".into()).await?;
let done  = store.get_bool("onboarding.completed".into()).await?;
```

Every getter returns `Option<T>` — missing keys are `None`, not an
error. `set_bytes` / `get_bytes` cover `Vec<u8>` for opaque blobs.

### Removing and listing

```rust
store.remove("user.email".into()).await?;   // returns bool: was present?
store.clear().await?;                       // wipe the namespace
let all_keys = store.keys().await?;         // Vec<String>
let has_key  = store.contains("theme".into()).await?;
```

## Backing storage

| Platform | Backend                | Location                             |
| -------- | ---------------------- | ------------------------------------ |
| Android  | `SharedPreferences`    | App-private XML in the app data dir  |
| iOS      | `UserDefaults`         | The app's default suite              |

Both backends are **key/value only** — no queries, no encryption. Do
not use it for secrets (use `EncryptedSharedPreferences` /
`Keychain`); use it for user preferences and low-security cache.

## Wire it up

Every istmo app calls `IstmoPluginRegistry.registerAll(...)` once —
that single call wires every eligible plugin's dispatcher, including
this one. If you already have it in place, you don't need to do
anything extra.

### Android

```kotlin
override fun onCreate(savedInstanceState: Bundle?) {
    IstmoRuntime.instance.start(this)
    IstmoPluginRegistry.registerAll(applicationContext)
    super.onCreate(savedInstanceState)
}
```

### iOS

```swift
@main
struct MyApp: App {
    init() {
        IstmoRuntime.shared.start()
        IstmoPluginRegistry.registerAll()
    }
}
```

The reference `DataStoreBackendImpl` / `DataStoreFactoryImpl` files
live in `plugins/data-store/native/{android,ios}/`. Copy them into
your app once — they compile as-is and rarely need customisation.

See [Auto-registration](/build-scripts/auto-register/) for details of
how the registry is generated and how to opt out.

## Sample: desktop emulator

The [`examples/data-store-demo`](https://github.com/sergioribera/istmo/tree/main/examples/data-store-demo)
crate ships an **in-process native emulator** — a small thread that
answers `DataStoreDispatcher` frames using an in-memory `HashMap`.
Great for iterating on UI without booting an emulator.

```rust
// examples/data-store-demo/src/emulator.rs (excerpt)
pub fn spawn(runtime: &Arc<Runtime>) {
    let store = Arc::new(Mutex::new(HashMap::<String, Vec<u8>>::new()));
    runtime.register_handler("istmo.data_store", move |envelope| {
        // decode call, respond from `store`
    });
}
```

## Threading

Everything is `suspend` on Kotlin and `async` on Swift; the reference
backends use the default dispatcher / any thread. If you swap the
backend for something with stricter threading, gate accordingly.

## Not shipped (yet)

- Encryption. Use `EncryptedSharedPreferences` / Keychain by wrapping
  the plugin behind your own trait, or wait for a follow-up
  `istmo-secure-store`.
- Change subscription. Streams over the entire store are on the
  roadmap.
- Multi-suite isolation on iOS. Currently pinned to `UserDefaults.standard`.
