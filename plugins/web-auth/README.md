# istmo-web-auth

Browser-based sign-in (OAuth 2.0 / OpenID Connect, RFC 8252) for
[istmo](https://github.com/SergioRibera/istmo).

| Platform | Browser | Callback |
|---|---|---|
| iOS | `ASWebAuthenticationSession` | custom scheme, or `https` on iOS 17.4+ |
| Android | Auth Tab, else Custom Tabs | custom scheme or verified App Link |
| Desktop (`desktop` feature) | default browser | loopback `http://127.0.0.1:<port>` or app scheme |
| Desktop (`embedded` feature) | `istmo-webview` window | intercepted redirect |

The plugin carries the authorization URL out and the callback back,
checks `state` (and `iss`), and mints PKCE / `state` / `nonce`. The token
exchange stays with the app.

App-scheme callbacks need the scheme declared in the app's `istmo.toml`:

```toml
[[app.url_scheme]]
scheme = "com.example.app"
```

```rust
use istmo_web_auth::{CallbackSpec, SignInOptions, WebAuthClient, WebAuthClientExt};

let auth = WebAuthClient::acquire()?;
let flow = auth.begin(CallbackSpec::loopback("/callback")).await?;
let url = format!(
    "https://login.example.com/authorize?client_id=…&response_type=code\
     &redirect_uri={}&state={}&code_challenge={}&code_challenge_method=S256",
    flow.redirect_uri_encoded(), flow.state(), flow.pkce().challenge(),
);
let callback = flow.complete(url, SignInOptions::default()).await?;
let code = callback.code()?;
// POST code + flow.pkce().verifier() to the token endpoint.
```

Provider notes: Google refuses embedded views and custom schemes for new
Android clients (use `istmo-google-sign-in` on mobile, loopback on
desktop); Microsoft Entra accepts any loopback port and no `[::1]`; Sign
in with Apple works through `ASWebAuthenticationSession`.
