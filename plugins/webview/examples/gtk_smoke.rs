//! Smoke test of the desktop engine: a window with local HTML that
//! calls Rust through the bridge, a blocked navigation and an `eval`.
//!
//! `cargo run -p istmo-webview --features desktop --example gtk_smoke`

use std::sync::Arc;
use std::time::Duration;

use istmo_core::Runtime;
use istmo_webview::desktop::{DesktopOptions, DesktopWebViewFactory};
use istmo_webview::{
    BridgeConfig, ControlEvent, NavigationRules, RuleAction, ScriptCall, ScriptWorld, UrlPattern,
    ViewEvent, WebViewClient, WebViewClientExt, WebViewHost, WebViewInitConfig, WebViewSpec,
};

const PAGE: &str = r#"<!doctype html><title>smoke</title><h1>istmo</h1><script>
window.addEventListener("load", async () => {
  const answer = await istmo.invoke("rpc", "ping");
  document.title = new TextDecoder().decode(answer);
  location.href = "https://blocked.example/";
});
</script>"#;

fn main() {
    let rt = Runtime::mock().runtime;
    rt.register_host(WebViewHost::new(DesktopWebViewFactory::new(
        &rt,
        DesktopOptions::default(),
    )));
    let client = Arc::new(
        pollster::block_on(WebViewClient::from_runtime_with(
            &rt,
            WebViewInitConfig {
                schemes: vec![istmo_webview::CustomScheme {
                    name: "app".into(),
                    source: istmo_webview::SchemeSource::Dynamic,
                }],
                ..WebViewInitConfig::default()
            },
        ))
        .expect("client"),
    );
    pollster::block_on(async {
        let caps = client.capabilities().await.expect("caps");
        println!(
            "backend {} {} embedded={}",
            caps.backend, caps.engine_version, caps.embedded
        );
        let spec = WebViewSpec::window("istmo smoke", 640, 480)
            .with_rules(
                NavigationRules::allow_only([UrlPattern::host("https", "app.example")])
                    .with(
                        UrlPattern::host("https", "blocked.example"),
                        RuleAction::Deny,
                    )
                    .with(UrlPattern::host("app", "localhost"), RuleAction::Allow),
            )
            .with_bridge(BridgeConfig::new(["rpc"]).allow_origin("https://app.example"));
        let view = client.open_view(spec).await.expect("open");
        let events = view.control().expect("events");
        view.load_html(PAGE.into(), Some("https://app.example/".into()))
            .await
            .expect("html");
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        let mut got_invoke = false;
        let mut got_block = false;
        while std::time::Instant::now() < deadline && !(got_invoke && got_block) {
            match events.next().await {
                Some(ViewEvent::Invoke(call)) => {
                    println!(
                        "invoke from {}: {:?}",
                        call.message().origin,
                        String::from_utf8_lossy(&call.message().data)
                    );
                    call.reply(Ok(b"pong".to_vec())).await.expect("reply");
                    got_invoke = true;
                }
                Some(ViewEvent::Other(ControlEvent::NavigationFailed(f))) => {
                    println!("blocked: {} ({:?})", f.url, f.kind);
                    got_block = true;
                }
                Some(ViewEvent::Other(e)) => println!("event: {e:?}"),
                Some(_) => {}
                None => break,
            }
        }
        let title = view
            .eval(ScriptCall {
                source: "document.title".into(),
                world: ScriptWorld::Page,
                expected_origin: "https://app.example".into(),
            })
            .await;
        println!("eval document.title = {title:?}");
        view.load_url("app://localhost/hello")
            .await
            .expect("load app");
        let mut streamed = false;
        while std::time::Instant::now() < deadline && !streamed {
            match events.next().await {
                Some(ViewEvent::Scheme(req)) => {
                    println!("scheme request {}", req.info().url);
                    let body = req.stream(200, "text/html", None).await.expect("head");
                    body.write(b"<p>stream".to_vec()).await.expect("chunk");
                    body.write(b"ed</p>".to_vec()).await.expect("chunk");
                    body.finish().await.expect("end");
                }
                Some(ViewEvent::Other(ControlEvent::NavigationFinished(_))) => streamed = true,
                Some(_) => {}
                None => break,
            }
        }
        let text = view
            .eval(ScriptCall {
                source: "document.body.textContent".into(),
                world: ScriptWorld::Page,
                expected_origin: "app://localhost".into(),
            })
            .await;
        println!("app page text = {text:?}");
        println!("RESULT invoke={got_invoke} blocked={got_block} streamed={streamed}");
        view.close().await.expect("close");
    });
}
