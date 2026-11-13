---
title: Plugin native-hosted
description: Envuelve un SDK nativo detrás de un trait Rust para que tu código Rust lo llame como si fuera local.
sidebar:
  order: 2
---

Un plugin native-hosted declara su forma en Rust y delega el trabajo
real a Kotlin (Android) y Swift (iOS). Esta es la forma que quieres
cuando la plataforma posee la API — permisos, ejecución en background,
Sign in with Google, HealthKit, Live Activities.

## El loop completo

1. Escribes el trait Rust con `#[istmo::plugin]`.
2. `istmo_build::emit()` publica el contract.
3. El `build.rs` app-side genera dispatchers Kotlin/Swift dentro de
   `android/` e `ios/`.
4. Implementas cada interface de backend generada en Kotlin/Swift con
   el código real de plataforma.
5. Registras el backend en el runtime al startup.
6. Tu código Rust llama `<T>Client::method(...)` como cualquier otra
   función async.

## Skeleton — un share sheet del sistema

```rust
// plugins/share-sheet/src/lib.rs
use istmo::plugin;

#[plugin(name = "acme.share_sheet")]
pub trait ShareSheet {
    async fn share_text(&self, text: String, subject: Option<String>)
        -> Result<(), ShareError>;
}

#[istmo::message]
pub struct ShareError {
    pub reason: String,
}

impl core::fmt::Display for ShareError { /* ... */ }
impl std::error::Error for ShareError {}
```

`istmo.toml`:

```toml
[plugin]
id          = "acme.share_sheet"
client_type = "::share_sheet::ShareSheetClient"

# El plugin no necesita nada nativo en compile-time; las APIs del
# sistema Android/iOS están disponibles out-of-the-box. Algunos
# plugins añaden entradas Gradle o SwiftPM aquí (ver el plugin
# google-sign-in para un ejemplo).
```

`build.rs`:

```rust
fn main() { istmo_build::emit(); }
```

## Qué se genera en la app

Luego de que tu app reconstruye, ves dos archivos nuevos por
plataforma dentro de tu tree de app:

```
android/app/src/main/java/com/myapp/gen/
├── ShareSheetTypes.kt
├── ShareSheetCodecsImpl.kt
└── ShareSheetDispatcher.kt           # expone interface ShareSheetBackend
```

```
ios/MyApp/Plugins/ShareSheet/Generated/
├── ShareSheetTypes.swift
├── ShareSheetCodecsImpl.swift
└── ShareSheetDispatcher.swift        # expone protocol ShareSheetBackend
```

Los archivos `Dispatcher` se regeneran en cada build — no los edites.
El único archivo que escribes a mano es el `BackendImpl`.

## Implementa el backend

### Android

```kotlin
package com.myapp.gen

import android.content.Context
import android.content.Intent

class ShareSheetBackendImpl(
    private val ctx: Context,
) : ShareSheetBackend {
    override suspend fun shareText(text: String, subject: String?): Unit {
        val intent = Intent(Intent.ACTION_SEND).apply {
            type = "text/plain"
            putExtra(Intent.EXTRA_TEXT, text)
            subject?.let { putExtra(Intent.EXTRA_SUBJECT, it) }
        }
        ctx.startActivity(Intent.createChooser(intent, null))
    }
}
```

### iOS

```swift
import UIKit

final class ShareSheetBackendImpl: ShareSheetBackend {
    func shareText(text: String, subject: String?) async throws {
        await MainActor.run {
            let vc = UIActivityViewController(
                activityItems: [text],
                applicationActivities: nil
            )
            if let subject { vc.setValue(subject, forKey: "subject") }
            UIApplication.shared.topViewController?.present(vc, animated: true)
        }
    }
}
```

## Distribuye el backend con el plugin

El backend de arriba vive en la app. Para distribuirlo con el crate del
plugin, pon las fuentes en `native/android/` y `native/ios/` junto a
`istmo.toml`: el plugin de Gradle agrega `native/android/` al source set
de la app, e `istmo-plugins.yml` agrega `native/ios/` al target de Xcode.

En Android, declara además el package que usan esas fuentes:

```toml
[plugin]
id              = "acme.share_sheet"
client_type     = "::share_sheet::ShareSheetClient"
android_package = "com.acme.sharesheet"
```

La app genera entonces `ShareSheetDispatcher.kt`, `ShareSheetTypes.kt`
y `ShareSheetCodecsImpl.kt` en `com.acme.sharesheet` en vez de en su
propio package, así que `native/android/ShareSheetBackendImpl.kt`
declara `package com.acme.sharesheet`, nombra los tipos generados sin
imports y compila en cualquier app, sea cual sea su
`[app] android_package`. Los tipos del runtime (`BackendException`,
`NativeHandleId`, `IstmoRuntime`, `IstmoActivityBound`, …) siguen
viniendo de `dev.istmo.runtime`. El registry construye
`com.acme.sharesheet.ShareSheetBackendImpl` salvo que `android_backend`
nombre otra clase.

Swift no tiene packages: el Swift generado y `native/ios/` compilan en el
mismo módulo de la app, así que mantén nombres de tipo únicos entre
plugins (con el nombre del plugin como prefijo, como hacen los archivos
generados `<T>Dispatcher`, `<T>Types` y `<T>CodecsImpl`). `seed_backends`
nunca siembra un `<T>BackendImpl` para un plugin que trae fuentes
nativas.

## Registra el backend

Por default `istmo-build` genera un `IstmoPluginRegistry` que cablea
cada dispatcher de plugin elegible por vos, y los entry points del
runtime lo llaman antes de que corra el entry point de Rust:

```kotlin
// IstmoGameActivity, la launcher del template, ya hace esto.
// Una app con su propia activity llama a IstmoHost antes de super.onCreate:
override fun onCreate(savedInstanceState: Bundle?) {
    IstmoHost.onCreate(this) // arranca el runtime + IstmoPluginRegistry
    super.onCreate(savedInstanceState)
}
```

```swift
// ios/<Name>App/main.swift
import IstmoRuntime

IstmoApp.run() // arranca el runtime + IstmoPluginRegistry, luego corre Rust
```

El registry construye el backend Android con un `Context` (o la
activity host, con `android_backend_arg`) y el de iOS sin argumentos. Plugins que necesitan
un constructor distinto setean `auto_register = false` en su
`istmo.toml` y esperan que la app registre manualmente. Ver
[Auto-registración](/istmo/es/build-scripts/auto-register/).

## Llama desde Rust

```rust
use share_sheet::ShareSheetClient;

let sheet = ShareSheetClient::from_runtime(&runtime)?;
sheet.share_text("Hello world".into(), None).await?;
```

## Sembra los archivos de backend

¿Iterando sobre un plugin grande? Habilita generación one-time de
skeleton con `AppOpts::seed_backends`:

```rust
// build.rs (en la raíz del crate app)
fn main() {
    istmo_build::emit_with(istmo_build::AppOpts {
        seed_backends: true,
        ..Default::default()
    });
}
```

El primer build crea `ShareSheetBackendImpl.kt` y
`ShareSheetBackendImpl.swift` con cuerpos `TODO()` / `fatalError`. El
seed usa `write_if_absent`, así que tus ediciones posteriores nunca se
sobrescriben.

## Threading

Tanto `suspend fun` de Kotlin como protocolos `async` de Swift son
honorados por el dispatcher generado. Usa el patrón de coroutine o
task que sea idiomático; Istmo no impone un executor. Si tu trabajo
debe aterrizar en el main thread (APIs de UIKit, trabajo de ciclo de
vida de vista), gatéalo con `MainActor.run` / `withContext(Dispatchers.Main)`
como siempre.

## Siguiente

- Streaming de eventos: [Streams](/istmo/es/writing-plugins/streams/).
- Pasar recursos nativos: [Native handles](/istmo/es/writing-plugins/native-handles/).
- Trabajo de larga duración: [Services y workers](/istmo/es/writing-plugins/services-workers/).
