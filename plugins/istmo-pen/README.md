# istmo-pen

Stylus and pencil input from Rust, for the
[istmo](https://github.com/SergioRibera/istmo) framework. Each
`PenClient` is bound to one window (`PenConfig::window_id`) and exposes
two streams: `events()` for contact samples (down, move with coalesced
and predicted samples, up, cancel, barrel buttons, double tap /
squeeze) and `hover()` for proximity samples. Every `PenSample` carries
position, pressure, tilt, azimuth, altitude, twist, barrel pressure,
tool id and button state; `capabilities()` says which of those the
device fills in.

| Platform | Backend |
| --- | --- |
| Android | `PenCaptureView` over stylus `MotionEvent`s (`native/android`) |
| iOS | `PenCaptureView` over `UITouch` / `UIPencilInteraction` (`native/ios`) |
| Windows | `PenPublisher`: `WM_POINTER*` window subclass |
| macOS | `PenPublisher`: `NSEvent` tablet events |
| Linux | `PenPublisher`: Wayland tablet-v2 or `XInput2`; optional seat-wide `libinput` (`libinput_fallback` feature) |

The plugin declares no `[min_versions]`.

## Install

```toml
[dependencies]
istmo-pen = "0.1.1-alpha.5"
```

## Usage

```rust,ignore
use istmo_core::StreamItem;
use istmo_pen::{PenClient, PenConfig, PenEvent};

let pen = PenClient::from_runtime_with(&runtime, PenConfig::new(1)).await?;
let events = pen.events()?;
while let Ok(StreamItem::Event(event)) = events.recv_async().await {
    if let PenEvent::Move(m) = event {
        draw(m.sample.x, m.sample.y, m.sample.pressure);
    }
}
```

## App setup

The plugin sets `auto_register = false`: the backend needs the view
or window that receives stylus input, so the app registers it.

* **Android**: add a `PenCaptureView` over the drawing surface and
  register it from the activity:

  ```kotlin
  IstmoRuntime.registerHandler(
      PenDispatcher.PLUGIN_ID,
      PenDispatcher(PenFactoryImpl(penView), PenCodecsImpl()),
  )
  ```

  The generated Kotlin lives in `dev.istmo.plugins.pen`.
* **iOS**: add a `PenCaptureView` and register it:

  ```swift
  IstmoRuntime.shared.registerHandler(
      PenDispatcher.PLUGIN_ID,
      PenDispatcher(factory: PenFactoryImpl(view: penView), codecs: PenCodecsImpl())
  )
  ```
* **Desktop**: install a publisher, register each window under the id
  its client uses, and host the Rust backend:

  ```rust,ignore
  use std::sync::Arc;
  use istmo_pen::{PenHost, backend::PenPublisherFactory, publisher::PenPublisher};

  let publisher = PenPublisher::install(&runtime);
  publisher.register_window(1, &window)?; // on the window's UI thread
  runtime.register_host(PenHost::new(PenPublisherFactory::new(Arc::clone(&publisher))));
  ```

  `PenPublisher::follow_registry` attaches every window registered in
  a shared `istmo_window::WindowRegistry` instead. On Windows the
  plugin's `[[windows_manifest_fragment]]` makes the app per-monitor
  DPI aware, which pen pointer messages need; `istmo-build` embeds it
  in the executable.

Full guide: <https://sergioribera.rs/istmo/plugins/pen/>
