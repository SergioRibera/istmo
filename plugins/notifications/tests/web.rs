//! The web host against stubbed browser APIs, in Node.
//!
//! Run with `wasm-bindgen-test-runner` as the target runner:
//! `cargo test --target wasm32-unknown-unknown -p istmo-notifications --test web`.
//! Node has no `Notification`, service worker or `IndexedDB`: each test
//! installs the stubs it needs on `globalThis` (the host looks them up at
//! call time). `IndexedDB` stays missing, so the store and the worker's
//! queue are not exercised here.
#![cfg(all(target_arch = "wasm32", target_os = "unknown"))]
#![allow(
    clippy::future_not_send,
    reason = "tests run on the single-threaded JS event loop"
)]

use std::sync::Arc;
use std::time::Duration;

use istmo_core::{Runtime, StreamItem, TypedStream};
use istmo_notifications::{
    Action, Feature, Notification, NotificationEvent, NotificationResponses, NotificationsClient,
    NotificationsHost, PermissionState, Progress, ResponseAction, ServiceWorker, Support, Trigger,
    WebNotifications,
};
use js_sys::{Array, Function, Reflect};
use wasm_bindgen::JsValue;
use wasm_bindgen_test::wasm_bindgen_test;

/// The stubbed `Notification` class: instances land in
/// `Notification.made`; `maxActions` and the option properties exist.
const PAGE_STUB: &str = r#"
    class Notification {
        constructor(title, options) {
            this.title = title;
            this.options = options || {};
            Notification.made.push(this);
        }
        close() { this.closed = true; if (this.onclose) this.onclose(); }
        get requireInteraction() { return false; }
        get silent() { return false; }
        get renotify() { return false; }
        get timestamp() { return 0; }
        get actions() { return []; }
        get image() { return ""; }
    }
    Notification.made = [];
    Notification.maxActions = 2;
    Notification.permission = "granted";
    Notification.requestPermission = () => Promise.resolve("granted");
    globalThis.Notification = Notification;
    Object.defineProperty(globalThis, "navigator", { value: {}, configurable: true, writable: true });
"#;

/// A service worker registration that records `showNotification` and
/// answers `getNotifications`.
const WORKER_STUB: &str = r#"
    const shown = [];
    const registration = {
        active: { scriptURL: "https://app.test/istmo-notifications-sw.js" },
        showNotification(title, options) {
            const tag = options.tag;
            for (let i = shown.length - 1; i >= 0; i--) {
                if (shown[i].tag === tag) shown.splice(i, 1);
            }
            const note = Object.assign({ title }, options);
            note.close = () => {
                const at = shown.indexOf(note);
                if (at >= 0) shown.splice(at, 1);
            };
            shown.push(note);
            return Promise.resolve();
        },
        getNotifications(filter) {
            const tag = filter && filter.tag;
            return Promise.resolve(shown.filter((n) => tag === undefined || n.tag === tag));
        },
    };
    globalThis.workerShown = shown;
    globalThis.navigator.serviceWorker = {
        ready: Promise.resolve(registration),
        getRegistration() { return Promise.resolve(registration); },
        register() { return Promise.resolve(registration); },
        addEventListener() {},
        startMessages() {},
    };
"#;

fn run(code: &str) -> JsValue {
    Function::new_no_args(code).call0(&JsValue::NULL).unwrap()
}

fn get(target: &JsValue, path: &str) -> JsValue {
    path.split('.').fold(target.clone(), |at, key| {
        Reflect::get(&at, &JsValue::from_str(key)).unwrap()
    })
}

fn made() -> Array {
    Array::from(&run("return Notification.made;"))
}

async fn sleep(ms: i32) {
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        let set: Function = get(&js_sys::global().into(), "setTimeout").into();
        set.call2(&JsValue::NULL, &resolve, &JsValue::from(ms))
            .unwrap();
    });
    wasm_bindgen_futures::JsFuture::from(promise).await.unwrap();
}

struct Setup {
    client: NotificationsClient,
    rt: Arc<Runtime>,
}

fn setup(service_worker: ServiceWorker) -> Setup {
    let host = WebNotifications::builder()
        .service_worker(service_worker)
        .without_store()
        .build();
    let rt = Runtime::mock()
        .host(NotificationsHost::new(host.clone()))
        .runtime;
    host.attach(&rt);
    Setup {
        client: NotificationsClient::from_runtime(&rt).unwrap(),
        rt,
    }
}

/// The events received so far.
fn taken(events: &TypedStream<NotificationEvent>) -> Vec<NotificationEvent> {
    let mut out = Vec::new();
    while let Some(Ok(StreamItem::Event(event))) = events.try_recv() {
        out.push(event);
    }
    out
}

fn n(id: &str) -> Notification {
    Notification::new(id, "Title").unwrap()
}

#[wasm_bindgen_test]
async fn page_notifications_show_click_and_close() {
    run(PAGE_STUB);
    let s = setup(ServiceWorker::None);
    let caps = s.client.capabilities().await.unwrap();
    assert_eq!(caps.backend, "web:page");
    assert_eq!(caps.max_actions, 0, "buttons need the service worker");
    assert_eq!(caps.support(Feature::TapWhenClosed), Support::Unsupported);
    assert_eq!(
        s.client.permission().await.unwrap().state,
        PermissionState::Granted
    );
    let events = s.client.events().unwrap();
    s.client
        .show(n("a").with_body("Body").with_user_data("ud"))
        .await
        .unwrap();
    let note = made().get(0);
    assert_eq!(get(&note, "title"), "Title");
    assert_eq!(get(&note, "options.body"), "Body");
    assert_eq!(get(&note, "options.tag"), "a");
    assert_eq!(get(&note, "options.data.istmo"), 1);
    assert!(get(&note, "options.actions").is_undefined());

    let responses = NotificationResponses::from_runtime(&s.rt).unwrap().stream();
    let click: Function = get(&note, "onclick").into();
    click.call0(&note).unwrap();
    let response = responses.recv_async().await.unwrap();
    assert_eq!(response.action, ResponseAction::Open);
    assert_eq!(response.user_data.as_deref(), Some("ud"));

    s.client.show(n("b")).await.unwrap();
    let second = made().get(1);
    let close: Function = get(&second, "onclose").into();
    close.call0(&second).unwrap();
    sleep(10).await;
    assert!(
        taken(&events)
            .iter()
            .any(|e| matches!(e, NotificationEvent::Dismissed(d) if d.id.as_str() == "b"))
    );
    assert_eq!(s.client.delivered().await.unwrap(), Vec::new());
}

#[wasm_bindgen_test]
async fn page_refusal_reports_show_unsupported() {
    run(PAGE_STUB);
    run(
        "globalThis.Notification = class extends Notification { constructor() { throw new TypeError('Illegal constructor'); } };",
    );
    let s = setup(ServiceWorker::None);
    let err = s.client.show(n("x")).await.unwrap_err();
    assert_eq!(
        istmo_notifications::NotificationError::from(err),
        istmo_notifications::NotificationError::Unsupported(Feature::Show)
    );
    assert_eq!(s.client.capabilities().await.unwrap().backend, "web:none");
}

#[wasm_bindgen_test]
async fn scheduled_notifications_fire_on_a_timeout() {
    run(PAGE_STUB);
    let s = setup(ServiceWorker::None);
    let events = s.client.events().unwrap();
    let info = s
        .client
        .schedule(n("later"), Trigger::after(Duration::from_millis(30)))
        .await
        .unwrap();
    assert!(info.next_fire_ms.is_some());
    assert_eq!(s.client.pending().await.unwrap().len(), 1);
    sleep(150).await;
    assert_eq!(made().length(), 1);
    assert_eq!(s.client.pending().await.unwrap(), Vec::new());
    assert!(matches!(
        taken(&events).as_slice(),
        [NotificationEvent::Delivered(id)] if id.as_str() == "later"
    ));
}

#[wasm_bindgen_test]
async fn the_service_worker_gets_buttons_and_lists_what_it_shows() {
    run(PAGE_STUB);
    run(WORKER_STUB);
    let s = setup(ServiceWorker::Existing);
    let caps = s.client.capabilities().await.unwrap();
    assert_eq!(caps.backend, "web:sw");
    assert_eq!(caps.max_actions, 2);
    assert_eq!(caps.support(Feature::ListDelivered), Support::Native);
    s.client
        .show(
            n("dl")
                .with_progress(Progress::fraction(0.5))
                .with_action(Action::button("pause", "Pause").unwrap())
                .with_action(Action::button("stop", "Stop").unwrap().background())
                .with_action(Action::button("more", "Details").unwrap()),
        )
        .await
        .unwrap();
    let shown = Array::from(&run("return globalThis.workerShown;"));
    assert_eq!(shown.length(), 1);
    let note = shown.get(0);
    assert_eq!(get(&note, "body"), "50%", "progress as text");
    let actions = Array::from(&get(&note, "actions"));
    let keys: Vec<String> = actions
        .iter()
        .map(|a| get(&a, "action").as_string().unwrap())
        .collect();
    assert_eq!(keys, ["pause", "istmo.more"], "third action overflows");
    let delivered = s.client.delivered().await.unwrap();
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].id.as_str(), "dl");

    // A silent progress update replaces by tag without renotify.
    s.client
        .set_progress("dl".try_into().unwrap(), Some(Progress::fraction(0.75)))
        .await
        .unwrap();
    let shown = Array::from(&run("return globalThis.workerShown;"));
    assert_eq!(shown.length(), 1);
    assert_eq!(get(&shown.get(0), "body"), "75%");
    assert!(get(&shown.get(0), "renotify").is_undefined());

    s.client.cancel("dl".try_into().unwrap()).await.unwrap();
    sleep(10).await;
    assert_eq!(
        Array::from(&run("return globalThis.workerShown;")).length(),
        0
    );
}

#[wasm_bindgen_test]
async fn a_failed_registration_falls_back_to_the_page_at_once() {
    run(PAGE_STUB);
    // The script is missing (404): `register` rejects, `ready` never settles.
    run(r#"
        globalThis.navigator.serviceWorker = {
            ready: new Promise(() => {}),
            getRegistration() { return Promise.resolve(undefined); },
            register() { return Promise.reject(new TypeError("bad HTTP response code (404)")); },
            addEventListener() {},
            startMessages() {},
        };
    "#);
    let s = setup(ServiceWorker::default());
    let started = js_sys::Date::now();
    let caps = s.client.capabilities().await.unwrap();
    assert_eq!(caps.backend, "web:page");
    assert!(
        js_sys::Date::now() - started < 2_000.0,
        "no wait for a worker that cannot come"
    );
    s.client.show(n("p")).await.unwrap();
    assert_eq!(made().length(), 1);
}

#[wasm_bindgen_test]
async fn callback_only_permission_requests_settle() {
    run(PAGE_STUB);
    // Old Safari: `requestPermission(callback)` returns nothing.
    run(r#"
        Notification.permission = "default";
        Notification.requestPermission = (done) => {
            setTimeout(() => { Notification.permission = "granted"; done("granted"); }, 5);
            return undefined;
        };
    "#);
    let s = setup(ServiceWorker::None);
    let status = s
        .client
        .request_permission(istmo_notifications::PermissionRequest::default())
        .await
        .unwrap();
    assert_eq!(status.state, PermissionState::Granted);
}
