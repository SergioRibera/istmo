//! End-to-end through the runtime with the scripted host: tokens,
//! intercept rules, decisions answered by drop, presenter frames and the
//! release path.

use std::sync::Arc;
use std::time::Duration;

use istmo_core::{NativeHandleId, Runtime};
use istmo_webview::testing::{RecordedCall, ScriptedWebViewFactory, WebScript};
use istmo_webview::{
    ControlEvent, Decision, DecisionKind, DecisionResponse, LoadRequest, NavigationRules,
    RuleAction, UrlPattern, ViewEvent, ViewFrame, ViewRef, WebViewClient, WebViewClientExt,
    WebViewError, WebViewHost, WebViewInitConfig, WebViewSpec, WebViewSpecExt,
};

fn setup() -> (Arc<Runtime>, WebScript, Arc<WebViewClient>) {
    let rt = Runtime::mock().runtime;
    let script = WebScript::default();
    rt.register_host(WebViewHost::new(ScriptedWebViewFactory::new(
        &rt,
        script.clone(),
    )));
    let client = pollster::block_on(WebViewClient::from_runtime_with(
        &rt,
        WebViewInitConfig::default(),
    ))
    .expect("create client");
    (rt, script, Arc::new(client))
}

fn wait_for(script: &WebScript, pred: impl Fn(&[RecordedCall]) -> bool) {
    for _ in 0..500 {
        if pred(&script.calls()) {
            return;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    panic!("condition not met; calls: {:#?}", script.calls());
}

#[test]
fn sign_in_view_intercepts_its_redirect() {
    let (_rt, _script, client) = setup();
    pollster::block_on(async {
        let spec = WebViewSpec::window("Sign in", 480, 640).sign_in(
            UrlPattern::host("com.example.app", "callback"),
            [UrlPattern::host("https", "login.example.com")],
        );
        let view = client.open_view(spec).await.unwrap();
        let events = view.control().expect("control lane");
        assert!(view.control().is_none(), "one subscriber");

        view.load_url("https://login.example.com/authorize")
            .await
            .unwrap();
        view.load_url("https://evil.example/phish").await.unwrap();
        view.load_url("com.example.app://callback/?code=abc&state=s")
            .await
            .unwrap();

        let mut finished = 0;
        let mut failed = 0;
        let mut intercepted = None;
        for _ in 0..16 {
            match events.next().await.expect("event") {
                ViewEvent::Other(ControlEvent::NavigationFinished(_)) => finished += 1,
                ViewEvent::Other(ControlEvent::NavigationFailed(_)) => failed += 1,
                ViewEvent::Other(ControlEvent::Intercepted(nav)) => {
                    intercepted = Some(nav.url);
                    break;
                }
                _ => {}
            }
        }
        let intercepted = intercepted.expect("redirect intercepted");
        assert_eq!((finished, failed), (1, 1));
        assert!(intercepted.contains("code=abc"));
        view.close().await.unwrap();
    });
}

#[test]
fn dropped_decision_denies_and_allowed_one_navigates() {
    let (_rt, script, client) = setup();
    pollster::block_on(async {
        let spec = WebViewSpec::embedded(1).with_rules(
            NavigationRules::allow_only([]).with(UrlPattern::host("https", "*"), RuleAction::Ask),
        );
        let view = client.open_view(spec).await.unwrap();
        let events = view.control().unwrap();

        view.load_url("https://a.example/").await.unwrap();
        let Some(ViewEvent::Decision(first)) = events.next().await else {
            panic!("expected a decision");
        };
        assert!(matches!(first.kind(), DecisionKind::Navigation(_)));
        drop(first);
        wait_for(&script, |calls| {
            calls.iter().any(|c| {
                matches!(
                    c,
                    RecordedCall::Decide(Decision {
                        response: DecisionResponse::Deny,
                        ..
                    })
                )
            })
        });

        view.load_url("https://b.example/").await.unwrap();
        let Some(ViewEvent::Decision(second)) = events.next().await else {
            panic!("expected a decision");
        };
        second.allow().await.unwrap();
        loop {
            if let Some(ViewEvent::Other(ControlEvent::NavigationFinished(nav))) =
                events.next().await
            {
                assert_eq!(nav.url, "https://b.example/");
                break;
            }
        }
    });
}

#[test]
fn wrong_token_is_refused() {
    let (_rt, _script, client) = setup();
    pollster::block_on(async {
        let view = client.open_view(WebViewSpec::embedded(1)).await.unwrap();
        let forged = ViewRef {
            token_lo: view.view_ref().token_lo.wrapping_add(1),
            ..view.view_ref()
        };
        let err = client
            .load(forged, LoadRequest::get("https://example.com/"))
            .await
            .map_err(WebViewError::from)
            .unwrap_err();
        assert_eq!(err, WebViewError::InvalidToken);
        let unknown = ViewRef {
            view: NativeHandleId(u64::MAX),
            ..forged
        };
        let err = client
            .focus(unknown)
            .await
            .map_err(WebViewError::from)
            .unwrap_err();
        assert_eq!(err, WebViewError::ViewClosed);
    });
}

#[test]
fn presenter_frames_reach_the_host_and_drop_releases_the_view() {
    let (_rt, script, client) = setup();
    let view = pollster::block_on(client.open_view(WebViewSpec::embedded(1))).unwrap();
    let id = view.id();
    let frame = ViewFrame {
        x: 10,
        y: 20,
        width: 300,
        height: 200,
        visible: true,
    };
    for _ in 0..5 {
        view.presenter().sync(frame);
    }
    let frames = script
        .calls()
        .iter()
        .filter(|c| matches!(c, RecordedCall::Frame(..)))
        .count();
    assert_eq!(frames, 1, "in-process sink, unchanged frames not resent");

    drop(view);
    wait_for(&script, |calls| calls.contains(&RecordedCall::Released(id)));
    assert_eq!(script.open_views(), []);
}
