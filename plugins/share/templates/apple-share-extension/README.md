# Share Extension template (iOS / macOS)

Receiving shares on Apple platforms requires a **Share Extension**: a
separate target the system launches when the user picks your app in the
share sheet. This template hands everything it receives to the app
through an App Group container; the app publishes it to Rust as an
`IncomingShare` on the `istmo.share.incoming` queue (`ShareInbox` on the
Rust side).

Sending needs none of this — only receiving.

## 1. Pick identifiers

| Build setting | Example | Used by |
|---|---|---|
| `ISTMO_SHARE_APP_GROUP` | `group.com.example.myapp` | app + extension |
| `ISTMO_SHARE_URL_SCHEME` | `myapp` | app + extension (iOS only) |

Register the App Group for both bundle ids in the Apple developer
portal.

## 2. Add the extension target

With xcodegen (`project.yml`):

```yaml
settings:
  base:
    ISTMO_SHARE_APP_GROUP: group.com.example.myapp
    ISTMO_SHARE_URL_SCHEME: myapp

targets:
  MyAppShare:
    type: app-extension
    platform: iOS            # or macOS
    sources:
      - path: ShareExtension # copy of this directory
        excludes: ["*.entitlements", "README.md"]
      - path: <path-to-istmo-share>/native/ios/ShareHandoff.swift
    info:
      path: ShareExtension/Info.plist
    entitlements:
      path: ShareExtension/ShareExtension-iOS.entitlements   # -macOS on macOS
    settings:
      PRODUCT_BUNDLE_IDENTIFIER: com.example.myapp.share

  MyApp:
    dependencies:
      - target: MyAppShare
```

In Xcode by hand: *File → New → Target → Share Extension*, replace the
generated sources with `ShareViewController.swift`, add
`ShareHandoff.swift`, and use this `Info.plist` / entitlements.

## 3. Configure the app

App entitlements (`<App>.entitlements`):

```xml
<key>com.apple.security.application-groups</key>
<array><string>$(ISTMO_SHARE_APP_GROUP)</string></array>
```

App `Info.plist`:

```xml
<key>IstmoShareAppGroup</key>
<string>$(ISTMO_SHARE_APP_GROUP)</string>
<!-- iOS: lets the extension wake the app immediately -->
<key>IstmoShareURLScheme</key>
<string>$(ISTMO_SHARE_URL_SCHEME)</string>
<key>CFBundleURLTypes</key>
<array>
  <dict>
    <key>CFBundleURLSchemes</key>
    <array><string>$(ISTMO_SHARE_URL_SCHEME)</string></array>
  </dict>
</array>
```

## 4. Drain the inbox

**iOS** (Swift, `native/ios/ShareInboxDrain.swift` is already in the app
target through the istmo xcodegen fragment):

```swift
// At launch: drain right away whenever the extension commits a share
// while the app process is alive.
IstmoShareInbox.observeHandoffs()

func sceneDidBecomeActive(_ scene: UIScene) {
    IstmoShareInbox.drain()
}

func scene(_ scene: UIScene, openURLContexts contexts: Set<UIOpenURLContext>) {
    if contexts.contains(where: { IstmoShareInbox.isWakeURL($0.url) }) {
        IstmoShareInbox.drain()
    }
}
```

**macOS** (Rust — the app is Rust-hosted):

```rust
let inbox = istmo_share::ShareInbox::acquire()?;
let group = "group.com.example.myapp";
istmo_share::desktop::drain_app_group_inbox(group, &inbox)?;
istmo_share::desktop::observe_app_group_handoffs(group, &inbox)?;
```

## How the extension wakes the app

After writing a share, the extension:

1. posts the Darwin notification `<group>.istmo-share.handoff` — a
   running app (`observeHandoffs` / `observe_app_group_handoffs`)
   drains immediately;
2. iOS: opens `<ISTMO_SHARE_URL_SCHEME>://istmo-share` through the
   responder chain, which brings a suspended or terminated app to the
   foreground; macOS: launches the app in the background if it is not
   running.

If both fail the share is not lost: it stays in the App Group until the
app's next activation drain.

## 5. Optional: share-sheet suggestions (direct share)

`ShareClient::set_share_targets` donates `INSendMessageIntent`
interactions so iOS suggests your conversations in the share sheet. It
needs, in the app's `Info.plist`:

```xml
<key>NSUserActivityTypes</key>
<array><string>INSendMessageIntent</string></array>
```

The template's `Info.plist` already lists `INSendMessageIntent` under
`IntentsSupported`, and the extension forwards the picked
conversation as `IncomingShare::target_id`.
