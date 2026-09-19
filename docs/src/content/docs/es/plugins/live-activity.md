---
title: Live Activity
description: Publica Live Activities de iOS y notifications persistentes de Android desde un solo trait Rust.
sidebar:
  order: 3
---

`istmo-live-activity` mapea ActivityKit de iOS 16+ y notifications
persistentes de Android detrás de un trait Rust. Describes los
atributos y estado de tu activity en Rust; ambas plataformas renderizan
idiomáticamente.

## Instala

```toml
[dependencies]
istmo-live-activity = "0.1"
```

## Modela tu activity

Los atributos son inmutables por activity; el estado se actualiza
sobre su lifetime.

```rust
use istmo_live_activity::LiveActivityClient;

#[istmo::message]
pub struct TimerAttributes {
    pub title:          String,
    pub target_seconds: u32,
}

#[istmo::message]
pub struct TimerState {
    pub elapsed_seconds: u32,
    pub label:           String,
}
```

Registra el codec una vez (Live Activities necesita saber cómo
serializar tus tipos nombrados), y arranca:

```rust
let la = LiveActivityClient::from_runtime(&runtime)?;
let activity = la.start(TimerAttributes {
    title: "Focus block".into(),
    target_seconds: 25 * 60,
}).await?;
```

## Update

```rust
activity.update(TimerState {
    elapsed_seconds: 60,
    label: "24:00 remaining".into(),
}).await?;
```

`activity` es un `NativeHandle<LiveActivity>`. Dropearlo *no* termina
la activity — llama `end()` explícitamente. Si quieres que la activity
sobreviva muerte del proceso, guarda el id en tu data store y
adóptalo de vuelta con `LiveActivityClient::adopt(id)`.

## End

```rust
activity.end(TimerState {
    elapsed_seconds: 25 * 60,
    label: "Done".into(),
}).await?;
```

El estado final renderiza brevemente antes de que la activity sea
removida.

## Detalles de iOS

- Requiere iOS 16.1+ (`ActivityKit`).
- Tu `Info.plist` necesita `NSSupportsLiveActivities = YES`.
- **Debes** implementar un struct `ActivityAttributes` Swift
  matcheado con la misma forma que tus `Attributes`/`State` en Rust.
  El `TimerLiveActivityHandler.swift` de referencia en el repo del
  plugin muestra el patrón.
- La extensión Widget (SwiftUI) renderiza los layouts compacto +
  expandido. El plugin no genera el código SwiftUI — lo escribes una
  vez y se queda quieto.

## Detalles de Android

- Renderiza como una notification persistente con custom content
  view.
- Usa `NotificationChannel` con `IMPORTANCE_LOW` por default; ajusta
  en tu copia de `NotificationLiveActivityHandler.kt`.
- Las notifications sobreviven muerte de proceso; adopta de vuelta
  con el id persistido.

## Cablealo

Llamá `IstmoPluginRegistry.registerAll(...)` una vez al startup. El
registry construye `LiveActivityDispatcher` con tu
`LiveActivityBackendImpl` copiado y el handler apropiado.

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
mecanismo completo.

## Handlers de referencia

El repo del plugin envía dos estrategias de renderizado bajo
`plugins/live-activity/native/`:

- **iOS `ActivityKitLiveActivityHandler`** — invoca ActivityKit.
- **Android `NotificationLiveActivityHandler`** — renderiza como
  notification persistente con una RemoteView inflada.

Ambos están pensados para ser **copiados y customizados** — renderizan
layouts específicos por app. El resto del plugin (protocolo, dispatch,
codec) nunca cambia.

## Sample end-to-end

Ver [`examples/live-activity-demo`](https://github.com/sergioribera/istmo/tree/main/examples/live-activity-demo)
en el repo. Handlers estilo timer + notification renderizados
in-process via un pequeño thread "emulador nativo".
