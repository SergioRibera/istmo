# istmo-opener

Open URLs with the system from Rust, for the
[istmo](https://github.com/SergioRibera/istmo) framework: the browser
for `https:`, the mail app for `mailto:`, the dialer for `tel:`, maps
for `geo:`.

| Platform | Backend |
| --- | --- |
| Android | `ACTION_VIEW` intent in `CATEGORY_BROWSABLE` (`native/android/OpenerBackendImpl.kt`) |
| iOS | `UIApplication.shared.open` (`native/ios/OpenerBackendImpl.swift`) |
| Desktop | `DesktopOpener`: `xdg-open`, `open` or `rundll32 url.dll,FileProtocolHandler`, no shell |

## Usage

```rust,ignore
use istmo_opener::{OpenerClient, SchemePolicy};

let opener = OpenerClient::from_runtime(&runtime)?;
opener
    .open_checked("https://example.com".into(), &SchemePolicy::default())
    .await?;
```

## Scheme allow-list

Every backend only opens `http`, `https`, `mailto`, `tel`, `sms` and
`geo` by default and fails anything else with
`OpenError::SchemeNotAllowed`. Widen or narrow it per backend:

* Rust callers: `SchemePolicy::default().allow("file")` with
  `OpenerClient::open_checked`.
* Desktop: `DesktopOpener::with_policy(policy)`.
* Android: `OpenerBackendImpl(context, allowedSchemes = setOf(...))`.
* iOS: `OpenerBackendImpl(allowedSchemes: [...])`.

The mobile backends are registered by the generated
`IstmoPluginRegistry`; desktop apps register
`OpenerHost::new(DesktopOpener::default())` themselves.
