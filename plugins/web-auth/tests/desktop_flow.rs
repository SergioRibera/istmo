//! The desktop sign-in flow through the runtime: `begin` reserves a
//! loopback port, a fake browser follows the authorization URL to the
//! callback, `complete` verifies `state`.

#![cfg(all(
    any(target_os = "linux", target_os = "macos", target_os = "windows"),
    feature = "desktop"
))]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;

use istmo_core::Runtime;
use istmo_web_auth::desktop::DesktopWebAuth;
use istmo_web_auth::{
    CallbackSpec, SignInOptions, WebAuthClient, WebAuthClientExt, WebAuthError, WebAuthHost,
    parse_params,
};

/// Plays the provider: reads `redirect_uri` and `state` from the
/// authorization URL and hits the callback with `state_override` or the
/// real state.
fn fake_browser(state_override: Option<&'static str>) -> istmo_web_auth::desktop::LaunchFn {
    Arc::new(move |url: &str| {
        let params = parse_params(url)?;
        let get = |k: &str| params.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        let redirect = get("redirect_uri").expect("redirect_uri");
        let state = state_override.map_or_else(|| get("state").expect("state"), str::to_owned);
        std::thread::spawn(move || {
            let rest = redirect.strip_prefix("http://").expect("loopback");
            let (authority, path) = rest.split_at(rest.find('/').expect("path"));
            let mut stream = TcpStream::connect(authority).expect("connect");
            write!(
                stream,
                "GET {path}?code=the-code&state={state} HTTP/1.1\r\nHost: {authority}\r\n\r\n"
            )
            .unwrap();
            let mut response = String::new();
            drop(stream.read_to_string(&mut response));
        });
        Ok(())
    })
}

fn client(launcher: istmo_web_auth::desktop::LaunchFn) -> WebAuthClient {
    let rt = Runtime::mock().runtime;
    rt.register_host(WebAuthHost::new(
        DesktopWebAuth::new(&rt).with_launcher(launcher),
    ));
    WebAuthClient::from_runtime(&rt).expect("client")
}

fn authorize_url(flow: &istmo_web_auth::SignInFlow<'_>) -> String {
    format!(
        "https://id.example/authorize?response_type=code&redirect_uri={}&state={}&code_challenge={}&code_challenge_method=S256",
        flow.redirect_uri_encoded(),
        flow.state(),
        flow.pkce().challenge()
    )
}

#[test]
fn loopback_sign_in_returns_the_verified_code() {
    let auth = client(fake_browser(None));
    pollster::block_on(async {
        let flow = auth
            .begin(CallbackSpec::loopback("/callback"))
            .await
            .unwrap();
        assert!(flow.redirect_uri().starts_with("http://127.0.0.1:"));
        let callback = flow
            .complete(authorize_url(&flow), SignInOptions::default())
            .await
            .unwrap();
        assert_eq!(callback.code().unwrap(), "the-code");
    });
}

#[test]
fn forged_state_never_completes() {
    let auth = client(fake_browser(Some("forged")));
    pollster::block_on(async {
        let flow = auth
            .begin(CallbackSpec::loopback("/callback"))
            .await
            .unwrap();
        let options = SignInOptions {
            timeout: Some(std::time::Duration::from_millis(400)),
            ..SignInOptions::default()
        };
        let err = flow
            .complete(authorize_url(&flow), options)
            .await
            .unwrap_err();
        assert_eq!(err, WebAuthError::Timeout);
    });
}

#[test]
fn a_prepared_redirect_is_single_use_and_https_only() {
    let auth = client(fake_browser(None));
    pollster::block_on(async {
        let flow = auth.begin(CallbackSpec::loopback("/cb")).await.unwrap();
        let err = flow
            .complete("http://id.example/authorize", SignInOptions::default())
            .await
            .unwrap_err();
        assert!(matches!(err, WebAuthError::InvalidArgument(_)));
        flow.complete(authorize_url(&flow), SignInOptions::default())
            .await
            .unwrap();
        let again = flow
            .complete(authorize_url(&flow), SignInOptions::default())
            .await
            .unwrap_err();
        assert_eq!(again, WebAuthError::UnknownSession);
    });
}
