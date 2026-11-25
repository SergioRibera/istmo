# istmo-webview

Cross-platform web view plugin for [istmo](https://github.com/SergioRibera/istmo):
Android `WebView`, `WKWebView` (iOS, macOS), WebView2 (Windows),
WebKitGTK 4.1 / 6.0 or Qt WebEngine (Linux) and iframes (browser) behind
one typed Rust surface.

- Embedded views the app places every frame (coalesced, no wire round
  trip), or plugin-owned windows.
- Navigation rules evaluated natively (allow, deny, intercept, open
  externally, ask), with one WHATWG normalisation every backend is held
  to (`tests/rule_vectors.tsv`, `tests/origin_vectors.tsv`).
- A JS bridge (`istmo.postMessage`, `await istmo.invoke`) bound to
  origins reported by the engine, with replies bound to their document.
- Custom schemes served from bundled assets or answered from Rust in
  streamed chunks.
- Ephemeral, persistent and shared profiles; cookies; data clearing.
- Decisions (permissions, downloads, dialogs, certificate errors, file
  choosers) as objects that answer safely when dropped.
- Lossless control events and opt-in lossy telemetry.
- `testing` feature: `ScriptedWebView`, an engine-less host for testing
  app flows.

| Feature | What it adds |
|---|---|
| `desktop` | Rust hosts for macOS, Windows and Linux (WebKitGTK 4.1) |
| `linux-webkitgtk-6-0` | WebKitGTK 6.0 (GTK 4) on Linux |
| `linux-qt` | Qt 6 WebEngine on Linux through `libistmo_webview_qt.so` |
| `web` | the browser host (`wasm32-unknown-unknown`, iframes) |
| `testing` | `ScriptedWebView` |

Linux engines are opened with `dlopen` at run time: the binary never
links GTK or Qt, and a missing engine is an error, not a crash.

For sign-in flows prefer the system browser through `istmo-web-auth`.

```rust
use std::sync::Arc;
use istmo_webview::{WebViewClient, WebViewClientExt, WebViewInitConfig, WebViewSpec};

let client = Arc::new(WebViewClient::acquire_with(WebViewInitConfig::default()).await?);
let view = client.open_view(WebViewSpec::embedded(window_id)).await?;
view.load_url("https://example.com").await?;
// every frame:
view.presenter().sync(frame);
```

Full guide: <https://sergioribera.rs/istmo/plugins/webview/>. Demo:
[`examples/webview-demo`](../../examples/webview-demo).
