# istmo-data-store

Persistent, namespaced key-value storage from Rust, for the
[istmo](https://github.com/SergioRibera/istmo) framework.

| Platform | Backend |
| --- | --- |
| Android | `SharedPreferences` file `<package name>.istmo.<namespace>` (`native/android`) |
| iOS | `UserDefaults` suite `<bundle id>.istmo.<namespace>` (`native/ios`) |
| Desktop | `DesktopDataStore`: one file per namespace under the app's preference directory |

The plugin declares no `[min_versions]`: it runs on every OS version
the istmo runtime supports.

## Install

```toml
[dependencies]
istmo-data-store = "0.1.1-alpha.5"
```

## Usage

```rust,ignore
use istmo_data_store::{DataStoreClient, DataStoreConfig};

let store = DataStoreClient::acquire_with(DataStoreConfig::new("settings")).await?;
store.set_string("theme".into(), "dark".into()).await?;
let theme = store.get_string("theme".into()).await?;
```

Values are strings, `i64`, `f64`, `bool` or bytes (`get_*` / `set_*`
per type), plus `remove`, `contains`, `keys` and `clear`.

A namespace is a short name for one group of values inside the app
(`"settings"`, `"session"`); platforms already keep apps apart, so it
does not need the app id. On Android it is part of a file name and must
not contain `/`. Because the file is named
`<package name>.istmo.<namespace>`, a namespace never shares a file
with the app's own preferences.

Reading a key as a different type than it was written with fails with
`DataStoreError::Corrupted` on every platform, with one Android
exception: `SharedPreferences` stores `f64` as a `Long` and bytes as a
Base64 `String`, so mixing `i64` / `f64`, or string / bytes, on one key
reinterprets the stored value instead of failing (a string that is not
valid Base64 still fails `get_bytes` with `Corrupted`).

## App setup

* **Android / iOS**: nothing to wire — the generated
  `IstmoPluginRegistry` registers `DataStoreFactoryImpl`, and the
  backends need no extra Gradle or Swift package dependencies.
* **Desktop**: register
  `runtime.register_host(DataStoreHost::new(DesktopDataStoreFactory))`.
  A namespace file that no longer decodes is moved aside to
  `<namespace>.bin.corrupt` and the namespace starts empty.

Full guide: <https://sergioribera.rs/istmo/plugins/data-store/>
