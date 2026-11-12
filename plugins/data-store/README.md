# istmo-data-store

Persistent, namespaced key-value storage from Rust, for the
[istmo](https://github.com/SergioRibera/istmo) framework.

| Platform | Backend |
| --- | --- |
| Android | `SharedPreferences`, one file per namespace (`native/android`) |
| iOS | `UserDefaults` suite `<bundle id>.istmo.<namespace>` (`native/ios`) |
| Desktop | `DesktopDataStore`: one file per namespace under the app's preference directory |

## Usage

```rust,ignore
use istmo_data_store::{DataStoreClient, DataStoreConfig};

let store = DataStoreClient::acquire_with(DataStoreConfig::new("settings")).await?;
store.set_string("theme".into(), "dark".into()).await?;
let theme = store.get_string("theme".into()).await?;
```

A namespace is a short name for one group of values inside the app
(`"settings"`, `"session"`); platforms already keep apps apart, so it
does not need the app id. On Android it is used as a file name and must
not contain `/`.

Reading a key as a different type than it was written with fails with
`DataStoreError::Corrupted` on every platform (Android cannot tell an
`f64` read as `i64`, or bytes read as a string, apart from the real
type, because `SharedPreferences` stores them as `Long` / `String`).

Desktop apps register `DataStoreHost::new(DesktopDataStoreFactory)`. A
namespace file that no longer decodes is moved aside to
`<file>.corrupt` and the namespace starts empty.
