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
2. `istmo_build::emit()` en el `build.rs` del plugin pasa el contrato y
   el `istmo.toml` a cada app que depende del crate.
3. El `build.rs` de la app genera dispatchers Kotlin/Swift dentro de
   `android/` e `ios/`.
4. Implementas cada interface de backend generada en Kotlin/Swift con
   el código real de plataforma.
5. El `IstmoPluginRegistry` generado registra el backend al arrancar la
   app.
6. Tu código Rust llama `<T>Client::method(...)` como cualquier otra
   función async.

## Skeleton — un share sheet del sistema

```rust
// share-sheet/src/lib.rs
use istmo::plugin;

#[plugin(name = "acme.share_sheet")]
pub trait ShareSheet {
    async fn share_text(&self, text: String, subject: Option<String>)
        -> Result<(), ShareError>;
}

#[istmo::message]
#[derive(Debug)]
pub struct ShareError {
    pub reason: String,
}

impl core::fmt::Display for ShareError { /* ... */ }
impl std::error::Error for ShareError {}
```

(Un crate de plugin independiente usa `istmo-core` + `istmo-macros` y
`crate = "::istmo_core"`, como en
[Tu primer plugin](/istmo/es/getting-started/first-plugin/).)

`istmo.toml`:

```toml
[plugin]
id          = "acme.share_sheet"
client_type = "::share_sheet::ShareSheetClient"

# El plugin no necesita nada nativo en compile time; las APIs de
# sistema de Android/iOS están disponibles de fábrica. Algunos plugins
# agregan entradas de Gradle o SwiftPM aquí (ver el plugin
# google-sign-in como ejemplo).
```

`build.rs`:

```rust
fn main() { istmo_build::emit(); }
```

## Qué se genera en la app

Después de que tu app recompila, aparecen tres archivos por plataforma
en el árbol de la app:

```
android/app/src/main/java/com/myapp/gen/
├── ShareSheetTypes.kt
├── ShareSheetCodecsImpl.kt
└── ShareSheetDispatcher.kt           # interface ShareSheetBackend + dispatcher
```

```
ios/MyApp/Plugins/ShareSheet/Generated/
├── ShareSheetTypes.swift
├── ShareSheetCodecsImpl.swift
└── ShareSheetDispatcher.swift        # protocolo ShareSheetBackend + dispatcher
```

`com.myapp.gen` es el paquete Kotlin por defecto de la app (su
`namespace` de Gradle + `.gen`, o `[app] android_package`). Estos
archivos se regeneran en cada build — no los edites. El único archivo
que escribes a mano es el `BackendImpl`.

Los métodos del backend conservan el nombre de Rust, así que
`share_text` sigue siendo `share_text`. Los tipos Rust se mapean a
Kotlin / Swift como `String` → `String`, `Option<T>` → `T?`, `Vec<u8>`
→ `ByteArray` / `Data`, `u32` → `UInt` / `UInt32`, etc.; un retorno
`Result<T, E>` se vuelve un método que lanza y retorna `T`.

## Implementa el backend

### Android

```kotlin
package com.myapp.gen

import android.content.Context
import android.content.Intent

class ShareSheetBackendImpl(
    private val ctx: Context,
) : ShareSheetBackend {
    override suspend fun share_text(text: String, subject: String?) {
        val intent = Intent(Intent.ACTION_SEND).apply {
            type = "text/plain"
            putExtra(Intent.EXTRA_TEXT, text)
            subject?.let { putExtra(Intent.EXTRA_SUBJECT, it) }
        }
        val chooser = Intent.createChooser(intent, null)
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        ctx.startActivity(chooser)
    }
}
```

Lanza `BackendException(ShareError(...))` (de `dev.istmo.runtime`) para
retornar el error de dominio.

### iOS

```swift
import UIKit

final class ShareSheetBackendImpl: ShareSheetBackend {
    func share_text(text: String, subject: String?) async throws {
        await MainActor.run {
            let vc = UIActivityViewController(
                activityItems: [text],
                applicationActivities: nil
            )
            if let subject { vc.setValue(subject, forKey: "subject") }
            // presenta `vc` desde el root view controller de la key window
        }
    }
}
```

Lanza un `ShareError` (el struct generado conforma `Error`) para
retornar el error de dominio.

## Distribuye el backend con el plugin

El backend de arriba vive en la app. Para distribuirlo con el crate del
plugin, pon las fuentes en `native/android/` y `native/ios/` junto a
`istmo.toml`: el plugin de Gradle agrega `native/android/` al source set
de la app, y el `istmo-plugins.yml` generado agrega `native/ios/` al
target de Xcode.

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

Por defecto `istmo-build` genera un `IstmoPluginRegistry` que cablea
cada dispatcher de plugin elegible por ti, y los entry points del
runtime lo llaman antes de que corra el entry point de Rust. Un plugin
se registra en una plataforma sólo cuando istmo conoce una clase de
backend ahí: el crate trae fuentes `native/<plataforma>/` (como arriba)
o su `istmo.toml` declara `android_backend` / `ios_backend`. Cuando el
backend vive en la app, como en
[Implementa el backend](#implementa-el-backend), la app lo pide para
ese plugin:

```toml
# el istmo.toml de la app
[[app.plugin]]
id            = "acme.share_sheet"
auto_register = true
```

Luego los entry points del runtime lo registran junto con el resto:

```kotlin
// IstmoGameActivity, la launcher del template, ya hace esto.
// Una app con su propia activity llama a IstmoHost ella misma:
override fun onCreate(savedInstanceState: Bundle?) {
    IstmoHost.onCreate(this) // arranca el runtime + IstmoPluginRegistry
    super.onCreate(savedInstanceState)
}

override fun onDestroy() {
    super.onDestroy()
    IstmoHost.onDestroy(this)
}
```

```swift
// ios/<Name>App/main.swift
import IstmoRuntime

IstmoApp.run() // arranca el runtime + IstmoPluginRegistry, luego corre Rust
```

El registry construye el backend Android con el `Context` de la
aplicación por defecto; `android_backend_arg = "activity"` (o una
subclase de `Activity` con nombre completo) le pasa la activity host.
El backend iOS se construye sin argumentos. Los plugins que necesitan
otro constructor ponen `auto_register = false` en su `istmo.toml` y
esperan que la app registre a mano con
`IstmoRuntime.registerHandler(ShareSheetDispatcher.PLUGIN_ID, ShareSheetDispatcher(backend, ShareSheetCodecsImpl()))`.
Ver [Auto-registración](/istmo/es/build-scripts/auto-register/).

## Llama desde Rust

```rust
use share_sheet::ShareSheetClient;

let sheet = ShareSheetClient::from_runtime(&runtime)?;
sheet.share_text("Hello world".into(), None).await?;
```

## Siembra los archivos de backend

¿Iterando sobre un plugin grande? Habilita la generación única de
esqueletos con `AppOpts::seed_backends`:

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
seed solo se escribe cuando el archivo no existe, así que tus
ediciones posteriores nunca se sobrescriben.

## Threading

Tanto `suspend fun` de Kotlin como protocolos `async` de Swift son
honrados por el dispatcher generado. Usa el patrón de coroutine o task
que sea idiomático; Istmo no impone un executor. Si tu trabajo debe
aterrizar en el main thread (APIs de UIKit, trabajo de ciclo de vida
de vista), protégelo con `MainActor.run` /
`withContext(Dispatchers.Main)` como siempre.

En Android una llamada unaria que hace Rust arranca tu backend **en el
hilo que la hizo** (el hilo del llamador en Rust, enganchado a la JVM en
su primera llamada), con `CoroutineStart.UNDISPATCHED`: un método que
nunca se suspende responde antes de que ese hilo regrese, sin ningún
salto de hilo; uno que se suspende continúa donde se reanude
(`Dispatchers.Default`, o el dispatcher al que cambió). Ese hilo puede
ser cualquiera: un worker de Rust, o el principal cuando una llamada de
Kotlin en main llegó a un host de Rust que vuelve a llamar. Así que, como
en cualquier `suspend fun`, no bloquees el hilo: mueve el trabajo
bloqueante (escrituras a disco como `SharedPreferences.Editor.commit()`,
aperturas de content providers, llamadas binder que pueden trabarse) a
`withContext(Dispatchers.IO)`. `withContext(Dispatchers.Default)` nunca lo
mueve: la corrutina ya corre bajo `Default` (arrancada en el sitio, pero
con el dispatcher de `Default`), y `withContext` solo salta cuando cambia
el dispatcher, sea cual sea el hilo. Para sacar trabajo pesado de CPU
del hilo de Rust usa `withContext(Dispatchers.IO)` o un dispatcher
propio (o `yield()`, que reanuda en un worker de `Default`). Un backend
que no pueda cumplirlo se registra con
`IstmoRuntime.registerHandler(id, handler, startInPlace = false)` (o,
en un plugin empaquetado, pone `android_start_in_place = false` en su
`istmo.toml`) y cada llamada arranca en `Dispatchers.Default`.

Todo lo que entrega el hilo de entrega del runtime (llamadas que pasaron
por el canal saliente, streams, notificaciones, creación de instancias)
arranca en `Dispatchers.Default`, nunca en ese hilo.

Un backend que lanza una `CancellationException` que no es la
cancelación de su propia llamada (un `withTimeout` sin capturar, una
tarea esperada que se canceló) responde a Rust con un error, y un stream
que alimenta termina con error; solo cuando Rust cancela la llamada queda
sin respuesta. Del lado que llama,
`IstmoRuntime.call` lanza `CancellationException` para una corrutina
cancelada aunque el host de Rust hubiera respondido en el sitio.

Las llamadas desde Rust llegan directo a Kotlin solo mientras no quede
en camino nada encolado antes (una notificación, un cancel, un ítem de
stream): una llamada nunca adelanta a un frame que el mismo hilo envió
antes. Las llamadas de hilos distintos nunca estuvieron ordenadas entre
sí.

En iOS las llamadas siguen llegando por el hilo de entrega del runtime
y corren en un `Task`. Una llamada de Swift a un host de Rust ejecuta el
host en la tarea del llamador, fuera de la cola serial del runtime.

## Siguiente

- Streaming de eventos: [Streams](/istmo/es/writing-plugins/streams/).
- Pasar recursos nativos de ida y vuelta: [Native handles](/istmo/es/writing-plugins/native-handles/).
- Trabajo de larga duración: [Servicios y workers](/istmo/es/writing-plugins/services-workers/).
- Frames, audio y otros buffers de alta frecuencia: [Frame sinks](/istmo/es/writing-plugins/frame-sinks/).
