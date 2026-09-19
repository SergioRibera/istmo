---
title: Google Sign-In
description: OAuth sign-in en Android (Credential Manager) e iOS (SDK GoogleSignIn).
sidebar:
  order: 1
---

`istmo-google-sign-in` envuelve los flows Google Sign-In idiomáticos de
cada plataforma detrás de un solo trait Rust.

- **Android** usa `androidx.credentials` (Credential Manager) más el
  provider `googleid`.
- **iOS** usa el paquete SwiftPM oficial `GoogleSignIn` (7.0+).

Ambos flows retornan un `NativeHandle<Credential>` que puedes
refresquear y liberar determinísticamente.

## Instala

```toml
[dependencies]
istmo-google-sign-in = "0.1"
```

Reinicia el build de tu app; `istmo_build::emit()` recoge las entradas
Gradle + SwiftPM del plugin automáticamente. Sin config extra de
plataforma más allá de los OAuth client IDs.

## Configura los OAuth clients

Sigue el [walkthrough de Google Cloud Console](https://developers.google.com/identity)
para crear:

- Un OAuth client **Web** — usado como `serverClientId` en Android.
- Un OAuth client **iOS** — usado como `clientID` en iOS.

Añádelos a tu módulo de config y pásalos en runtime:

```rust
use istmo_google_sign_in::{SignInClient, SignInConfig};

let sign_in = SignInClient::from_runtime(&runtime)?;
sign_in.configure(SignInConfig {
    server_client_id: "1234-web.apps.googleusercontent.com".into(),
    ios_client_id: Some("1234-ios.apps.googleusercontent.com".into()),
    scopes: vec!["email".into(), "profile".into()],
}).await?;
```

## Sign in

```rust
let account = sign_in.sign_in_owned(SignInRequest {
    prompt: SignInPrompt::SelectAccount,
}).await?;

tracing::info!("signed in as {} ({})", account.display_name, account.email);
// account.credential: NativeHandle<Credential>
```

Usa `sign_in_owned` (variante owned) a menos que específicamente
necesites re-shippear el `NativeHandleId` crudo — la variante owned
libera el credential automáticamente al drop.

## Refresh silencioso

Refresh silencioso retorna un handle de credential nuevo. Dropea el
viejo primero si quieres release determinístico:

```rust
let refreshed = sign_in.refresh_owned(&account.credential).await?;
```

## Sign out

```rust
sign_in.sign_out().await?;
```

`sign_out` invalida el credential del lado nativo y emite
`Frame::ReleaseNativeHandle` para cualquier credential outstanding que
tu código Rust todavía retenga.

## Detalles de Android

El plugin declara sus deps Gradle via `istmo.toml`:

```toml
[[gradle]]
group    = "androidx.credentials"
artifact = "credentials"
version  = "1.3.0"

[[gradle]]
group    = "androidx.credentials"
artifact = "credentials-play-services-auth"
version  = "1.3.0"

[[gradle]]
scope    = "api"
group    = "com.google.android.libraries.identity.googleid"
artifact = "googleid"
version  = "1.1.1"
```

Nada que añadir a tu `build.gradle.kts` app-side — las entradas fluyen
por el handover de deps nativas de `istmo-build`.

El flow Credential Manager necesita contexto `Activity`. El backend de
referencia bajo `plugins/google-sign-in/native/android/` lo toma de
`IstmoRuntime.instance.currentActivity`.

## Detalles de iOS

El plugin declara el paquete Swift:

```toml
[[swift_package]]
url          = "https://github.com/google/GoogleSignIn-iOS.git"
product      = "GoogleSignIn"
from_version = "7.0.0"
```

Tu `Info.plist` debe incluir el reversed client id bajo
`CFBundleURLSchemes`, y tu `App.swift` debe llamar
`GIDSignIn.sharedInstance.handle(url:)` dentro de `.onOpenURL`. El
plugin envía un helper `SignInAppDelegate` que hace ambos si lo
registras en `IstmoRuntime.shared`.

## Cablealo

`SignIn` es stateful (carga una configuración OAuth por instancia),
así que `istmo-build` lo genera dentro de
`IstmoPluginRegistry.registerAll(...)` usando su `SignInFactoryImpl`.
Mientras copies el `SignInFactoryImpl.kt` / `SignInFactoryImpl.swift`
de referencia a tu tree de app y llames al registry una vez, el
plugin queda cableado.

```kotlin
override fun onCreate(savedInstanceState: Bundle?) {
    IstmoRuntime.instance.start(this)
    IstmoPluginRegistry.registerAll(applicationContext)
    super.onCreate(savedInstanceState)
}
```

```swift
@main
struct MyApp: App {
    init() {
        IstmoRuntime.shared.start()
        IstmoPluginRegistry.registerAll()
    }
}
```

Ver [Auto-registración](/es/build-scripts/auto-register/) para el
mecanismo completo y flags de opt-out.

## Backends de referencia

El repo del plugin envía implementaciones de referencia bajo
`plugins/google-sign-in/native/`:

- `android/SignInBackendImpl.kt`
- `ios/SignInBackendImpl.swift`

Copia estos a tu tree de app la primera vez que habilites el plugin,
luego customiza según necesites. Como la forma del trait es fija, la
mayoría de apps nunca necesita tocarlos.

## Sample end-to-end completo

Ver [`examples/rust-mobile-demo`](https://github.com/sergioribera/istmo/tree/main/examples/rust-mobile-demo)
en el repo — una app basada en egui que combina sign-in, permisos,
notifications, y AdMob en un solo binario Rust.
