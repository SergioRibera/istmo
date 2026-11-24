# istmo-webview

Cross-platform web view plugin for [istmo](https://github.com/SergioRibera/istmo):
Android `WebView`, `WKWebView` (iOS, macOS), WebView2 (Windows) and
WebKitGTK / Qt WebEngine (Linux) behind one typed Rust surface.

- Embedded views placed by the app every frame, or plugin-owned windows.
- Navigation rules evaluated natively (allow, deny, intercept, open
  externally, ask), with one normalisation shared by every backend
  (`tests/rule_vectors.tsv`).
- A JS bridge (`istmo.postMessage`, `await istmo.invoke`) bound to
  origins reported by the engine.
- Custom schemes served from bundled assets or answered from Rust in
  streamed chunks.
- Ephemeral, persistent and shared profiles; cookies; data clearing.
- Decisions (permissions, downloads, dialogs, certificate errors, file
  choosers) as objects that answer safely when dropped.
- Lossless control events and opt-in lossy telemetry.
- `testing` feature: `ScriptedWebView`, an engine-less host for testing
  app flows.

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
