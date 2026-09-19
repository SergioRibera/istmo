---
title: Data Store
description: Storage key/value cross-plataforma respaldado por SharedPreferences en Android y UserDefaults en iOS.
sidebar:
  order: 2
---

`istmo-data-store` es un store key/value native-hosted. La API Rust es
idéntica en cada plataforma; el backend usa `SharedPreferences` en
Android y `UserDefaults` en iOS. Los dos backends viajan en el crate
del plugin bajo `native/{android,ios}/` con cero deps externas.

## Instala

```toml
[dependencies]
istmo-data-store = "0.1"
```

## Úsalo

```rust
use istmo_data_store::DataStoreClient;

let store = DataStoreClient::from_runtime(&runtime)?;

store.put_string("user.email", "iris@example.com").await?;
store.put_bool("onboarding.completed", true).await?;
store.put_i64("last_sync_ms", now_ms()).await?;

let email = store.get_string("user.email").await?;
let done  = store.get_bool("onboarding.completed").await?;
```

Cada getter retorna `Option<T>` — keys faltantes son `None`, no un
error.

### Eliminar y listar

```rust
store.remove("user.email").await?;
store.clear().await?;                    // borra todo el store
let all_keys = store.keys().await?;      // Vec<String>
```

## Storage subyacente

| Plataforma | Backend                | Ubicación                            |
| ---------- | ---------------------- | ------------------------------------ |
| Android    | `SharedPreferences`    | XML app-privado en el data dir       |
| iOS        | `UserDefaults`         | El suite default de la app           |

Ambos backends son **solo key/value** — sin queries, sin encriptación.
No lo uses para secretos (usa `EncryptedSharedPreferences` /
`Keychain`); úsalo para preferencias de usuario y cache low-security.

## Cablealo

Cada app istmo llama `IstmoPluginRegistry.registerAll(...)` una sola
vez — esa llamada cablea todos los dispatchers de plugins elegibles,
incluyendo este. Si ya lo tenés en su lugar, no hace falta que hagas
nada extra.

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

Los archivos `DataStoreBackendImpl` / `DataStoreFactoryImpl` de
referencia viven en `plugins/data-store/native/{android,ios}/`.
Copiálos a tu app una vez — compilan tal cual y raramente necesitan
customización.

Ver [Auto-registración](/es/build-scripts/auto-register/) para
detalles de cómo se genera el registry y cómo opt-out.

## Sample: emulador desktop

El crate [`examples/data-store-demo`](https://github.com/sergioribera/istmo/tree/main/examples/data-store-demo)
envía un **emulador nativo in-process** — un pequeño thread que
responde frames del `DataStoreDispatcher` usando un `HashMap`
in-memory. Genial para iterar sobre UI sin bootear un emulador.

```rust
// examples/data-store-demo/src/emulator.rs (excerpt)
pub fn spawn(runtime: &Arc<Runtime>) {
    let store = Arc::new(Mutex::new(HashMap::<String, Vec<u8>>::new()));
    runtime.register_handler("istmo.data_store", move |envelope| {
        // decodea call, responde desde `store`
    });
}
```

## Threading

Todo es `suspend` en Kotlin y `async` en Swift; los backends de
referencia usan el dispatcher default / cualquier thread. Si
reemplazas el backend por algo con threading más estricto, gatéalo
apropiadamente.

## No enviado (por ahora)

- Encriptación. Usa `EncryptedSharedPreferences` / Keychain envolviendo
  el plugin detrás de tu propio trait, o espera un follow-up
  `istmo-secure-store`.
- Suscripción a cambios. Streams sobre el store entero están en el
  roadmap.
- Aislamiento multi-suite en iOS. Actualmente fijado a
  `UserDefaults.standard`.
