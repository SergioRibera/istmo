//! In-memory engine for testing the desktop core: one UI thread, views
//! that "navigate" by running the core's hooks.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use istmo_core::NativeHandleId;
use istmo_core::sync::lock;

use super::core::{Done, Engine, FrameSlot, SchemeResponder, UiTask, UiView, ViewCore};
use crate::present::FrameSink;
use crate::{
    ClearDataRequest, CookieFilter, FindQuery, FindResult, HistoryNav, LoadRequest, ReplyResult,
    SchemeBody, SchemeRequestInfo, SchemeResponseHead, ScriptCall, SnapshotOptions, ViewFrame,
    WebCookie, WebHistory, WebSnapshot, WebViewCapabilities, WebViewError,
};

thread_local! {
    static VIEWS: RefCell<HashMap<NativeHandleId, MockView>> = RefCell::new(HashMap::new());
}

/// What the mock UI thread saw.
#[derive(Debug, Default)]
pub(crate) struct Log {
    pub(crate) loads: Vec<String>,
    pub(crate) delivered: Vec<(String, Vec<u8>)>,
    pub(crate) scheme: Vec<String>,
    pub(crate) replies: Vec<ReplyResult>,
}

pub(crate) struct MockEngine {
    tx: flume::Sender<UiTask>,
    pub(crate) log: Arc<Mutex<Log>>,
    frames: Mutex<HashMap<NativeHandleId, Arc<FrameSlot>>>,
}

impl MockEngine {
    pub(crate) fn start() -> Arc<Self> {
        let (tx, rx) = flume::unbounded::<UiTask>();
        std::thread::Builder::new()
            .name("mock-ui".into())
            .spawn(move || {
                while let Ok(task) = rx.recv() {
                    task();
                }
            })
            .unwrap();
        Arc::new(Self {
            tx,
            log: Arc::default(),
            frames: Mutex::default(),
        })
    }
}

struct MockView {
    core: Arc<ViewCore>,
    log: Arc<Mutex<Log>>,
}

struct MockResponder {
    log: Arc<Mutex<Log>>,
}

impl SchemeResponder for MockResponder {
    fn head(&mut self, head: &SchemeResponseHead) {
        lock(&self.log).scheme.push(format!("head {}", head.status));
    }
    fn body(&mut self, body: SchemeBody) {
        lock(&self.log).scheme.push(match body {
            SchemeBody::Data(d) => format!("data {}", d.len()),
            SchemeBody::End => "end".into(),
            SchemeBody::Abort(r) => format!("abort {r}"),
        });
    }
}

impl Engine for MockEngine {
    fn describe(&self) -> (String, String) {
        ("mock".into(), String::new())
    }
    fn capabilities(&self, caps: &mut WebViewCapabilities) {
        caps.embedded = true;
    }
    fn post(&self, task: UiTask) {
        drop(self.tx.send(task));
    }
    fn create(&self, core: Arc<ViewCore>, done: Done<()>) {
        let log = Arc::clone(&self.log);
        self.post(Box::new(move || {
            VIEWS.with(|v| v.borrow_mut().insert(core.id, MockView { core, log }));
            done(Ok(()));
        }));
    }
    fn with_view(&self, id: NativeHandleId, f: super::core::ViewTask) {
        self.post(Box::new(move || {
            VIEWS.with(|v| {
                let mut views = v.borrow_mut();
                f(views.get_mut(&id).map(|v| v as &mut dyn UiView));
            });
        }));
    }
    fn frame_sink(&self, id: NativeHandleId) -> Arc<dyn FrameSink> {
        let slot = Arc::clone(lock(&self.frames).entry(id).or_default());
        Arc::new(SlotSink(slot))
    }
}

struct SlotSink(Arc<FrameSlot>);

impl FrameSink for SlotSink {
    fn set_frame(&self, frame: ViewFrame) {
        self.0.store(frame);
    }
}

impl UiView for MockView {
    fn load(&mut self, request: &LoadRequest) {
        lock(&self.log).loads.push(request.url.clone());
        let url = request.url.clone();
        let core = Arc::clone(&self.core);
        // A page under `app:` asks its scheme; others navigate.
        if url.starts_with("app:") {
            core.scheme_request(
                SchemeRequestInfo {
                    request_id: 0,
                    scheme: "app".into(),
                    method: "GET".into(),
                    url,
                    headers: Vec::new(),
                    has_body: false,
                    initiator_origin: None,
                },
                Box::new(MockResponder {
                    log: Arc::clone(&self.log),
                }),
            );
            return;
        }
        let navigating = Arc::clone(&core);
        let target = url.clone();
        core.navigation(
            &target,
            false,
            Box::new(move |go| {
                if go {
                    navigating.started(&url);
                    navigating.committed(&url);
                    navigating.finished(&url, false, false);
                }
            }),
        );
    }
    fn load_html(&mut self, _html: &str, base_url: Option<&str>) {
        let url = base_url.unwrap_or("about:blank");
        self.core.committed(url);
    }
    fn navigate(&mut self, _nav: HistoryNav) {}
    fn history(&mut self) -> WebHistory {
        WebHistory::default()
    }
    fn focus(&mut self) {}
    fn eval(&mut self, call: &ScriptCall, done: Done<String>) {
        // The "page" posts a bridge message back, then answers.
        let log = Arc::clone(&self.log);
        self.core.bridge_message(
            "rpc".into(),
            call.expected_origin.clone(),
            true,
            self.core.document_id(),
            call.source.clone().into_bytes(),
            Some(Box::new(move |result| lock(&log).replies.push(result))),
        );
        done(Ok("1".into()));
    }
    fn deliver(&mut self, channel: &str, data: &[u8]) {
        lock(&self.log)
            .delivered
            .push((channel.to_owned(), data.to_vec()));
    }
    fn cookies(&mut self, _filter: &CookieFilter, done: Done<Vec<WebCookie>>) {
        done(Ok(Vec::new()));
    }
    fn set_cookie(&mut self, _cookie: &WebCookie, done: Done<()>) {
        done(Ok(()));
    }
    fn clear_data(&mut self, _request: &ClearDataRequest, done: Done<()>) {
        done(Ok(()));
    }
    fn set_zoom(&mut self, _factor: f32) {}
    fn find(&mut self, _query: &FindQuery, done: Done<FindResult>) {
        done(Ok(FindResult::default()));
    }
    fn snapshot(&mut self, _options: SnapshotOptions, done: Done<WebSnapshot>) {
        done(Err(WebViewError::Unsupported("snapshot".into())));
    }
    fn print(&mut self) -> Result<(), WebViewError> {
        Ok(())
    }
    fn set_inspector(&mut self, _open: bool) -> Result<(), WebViewError> {
        Ok(())
    }
    fn close(&mut self) {}
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use istmo_core::Runtime;

    use super::*;
    use crate::desktop::DesktopWebViewFactory;
    use crate::{
        BridgeConfig, ControlEvent, LoadRequest, NavigationRules, OutgoingMessage, RuleAction,
        SchemeBody, ScriptCall, ScriptWorld, UrlPattern, ViewEvent, WebViewClient,
        WebViewClientExt, WebViewError, WebViewHost, WebViewInitConfig, WebViewSpec,
    };

    fn setup() -> (Arc<MockEngine>, Arc<WebViewClient>) {
        let rt = Runtime::mock().runtime;
        let engine = MockEngine::start();
        rt.register_host(WebViewHost::new(DesktopWebViewFactory::with_engine(
            &rt,
            Arc::clone(&engine) as Arc<dyn Engine>,
        )));
        let client = pollster::block_on(WebViewClient::from_runtime_with(
            &rt,
            WebViewInitConfig::default(),
        ))
        .unwrap();
        (engine, Arc::new(client))
    }

    fn wait(pred: impl Fn() -> bool) {
        for _ in 0..500 {
            if pred() {
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("condition not met");
    }

    #[test]
    fn rules_bridge_and_origin_checks_through_the_core() {
        let (engine, client) = setup();
        pollster::block_on(async {
            let spec = WebViewSpec::embedded(1)
                .with_rules(
                    NavigationRules::allow_only([UrlPattern::host("https", "app.example")]).with(
                        UrlPattern::host("https", "cb.example"),
                        RuleAction::Intercept,
                    ),
                )
                .with_bridge(BridgeConfig::new(["rpc"]).allow_origin("https://app.example"));
            let view = client.open_view(spec).await.unwrap();
            let events = view.control().unwrap();
            view.load_url("https://evil.example/").await.unwrap();
            view.load_url("https://app.example/").await.unwrap();
            let mut saw_commit = false;
            for _ in 0..8 {
                match events.next().await.unwrap() {
                    ViewEvent::Other(ControlEvent::NavigationCommitted(_)) => saw_commit = true,
                    ViewEvent::Other(ControlEvent::NavigationFinished(_)) => break,
                    _ => {}
                }
            }
            assert!(saw_commit);
            assert_eq!(
                lock(&engine.log).loads,
                vec!["https://app.example/"],
                "denied load never reached the engine"
            );

            // eval/post with the wrong origin never reach the page.
            let wrong = ScriptCall {
                source: "x".into(),
                world: ScriptWorld::Page,
                expected_origin: "https://evil.example".into(),
            };
            assert_eq!(
                view.eval(wrong).await.unwrap_err(),
                WebViewError::OriginMismatch
            );
            let right = ScriptCall {
                source: "ping".into(),
                world: ScriptWorld::Page,
                expected_origin: "https://app.example".into(),
            };
            assert_eq!(view.eval(right).await.unwrap(), "1");
            let Some(ViewEvent::Invoke(call)) = events.next().await else {
                panic!("expected invoke")
            };
            assert_eq!(call.message().data, b"ping");
            call.reply(Ok(b"pong".to_vec())).await.unwrap();
            wait(|| !lock(&engine.log).replies.is_empty());
            view.post_message(OutgoingMessage {
                channel: "rpc".into(),
                data: vec![1],
                expected_origin: "https://app.example".into(),
            })
            .await
            .unwrap();
            wait(|| lock(&engine.log).delivered.len() == 1);
        });
    }

    #[test]
    fn scheme_requests_stream_and_time_out() {
        let (engine, client) = setup();
        pollster::block_on(async {
            let spec = WebViewSpec::embedded(1).with_timeouts(crate::DecisionTimeouts {
                navigation_ms: 1000,
                scheme_head_ms: 100,
            });
            let view = client.open_view(spec).await.unwrap();
            let events = view.control().unwrap();
            view.load(LoadRequest::get("app://localhost/a"))
                .await
                .unwrap();
            let Some(ViewEvent::Scheme(req)) = events.next().await else {
                panic!("expected scheme request")
            };
            req.respond(200, "text/plain", b"hello".to_vec())
                .await
                .unwrap();
            wait(|| lock(&engine.log).scheme.len() == 3);
            assert_eq!(lock(&engine.log).scheme, vec!["head 200", "data 5", "end"]);

            view.load(LoadRequest::get("app://localhost/b"))
                .await
                .unwrap();
            let Some(ViewEvent::Scheme(slow)) = events.next().await else {
                panic!("expected scheme request")
            };
            std::thread::sleep(Duration::from_millis(250));
            wait(|| lock(&engine.log).scheme.len() == 5);
            assert_eq!(lock(&engine.log).scheme[3], "head 504");
            let late = slow.respond(200, "text/plain", Vec::new()).await;
            assert_eq!(late.unwrap_err(), WebViewError::UnknownRequest);
            let _ = SchemeBody::End;
        });
    }
}
