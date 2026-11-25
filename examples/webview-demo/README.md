# webview-demo

A small desktop browser built on `istmo-webview`, plus the two things
most apps want a web view for:

- **A local page with a bridge.** `app://localhost/` is served from
  [`assets/web/`](assets/web) by the engine itself (no network, no round
  trip to Rust per file). The page calls `istmo.invoke("demo", "time")`
  and Rust answers; "Send to page" pushes a message the other way. The
  bridge only accepts the `app://localhost` origin.
- **Sign in with Microsoft.** `istmo-web-auth` opens the system browser
  with PKCE, waits on a loopback redirect and checks `state` before
  handing back the authorization code. Exchanging the code for tokens is
  left to the app.

## Run

```sh
cargo run -p webview-demo
```

The view sits inside the window when the engine can embed it (X11,
macOS, Windows). On Wayland it opens its own window and the panel says
so.

Linux needs WebKitGTK 4.1 or 6.0 at run time (`libwebkit2gtk-4.1.so.0`
/ `libwebkitgtk-6.0.so.4`); nothing is linked at build time. To also try
Qt WebEngine, build with `--features qt` and Qt 6 WebEngine development
files on `PKG_CONFIG_PATH`.

### Microsoft sign-in

1. In the Microsoft Entra admin center, register an application and add
   a **Mobile and desktop applications** platform with the redirect URI
   `http://localhost`. Microsoft accepts any port on a loopback redirect,
   which is what `istmo-web-auth` listens on.
2. Run with the application (client) id:

   ```sh
   ISTMO_DEMO_MS_CLIENT_ID=00000000-0000-0000-0000-000000000000 cargo run -p webview-demo
   ```

### Self-test

`ISTMO_DEMO_SELFTEST=1` loads the local page, reads its heading through
`eval`, prints one `SELFTEST …` line and quits.
