//! Small dynamic helpers over `js_sys`: the host reaches the browser
//! through `globalThis` lookups at call time, so it runs (and is
//! tested) wherever the globals exist, Node with stubs included.

#![allow(
    clippy::future_not_send,
    reason = "wasm32-unknown-unknown is single-threaded: these futures run on the JS event loop, behind `js::local` where a `Send` one is needed"
)]

use std::future::Future;

use js_sys::{Array, Function, Object, Reflect};
use wasm_bindgen::{JsCast, JsValue};

/// `globalThis`.
pub(super) fn global() -> JsValue {
    js_sys::global().into()
}

/// `target[key]`, `None` when missing, `null` or `undefined`.
pub(super) fn get(target: &JsValue, key: &str) -> Option<JsValue> {
    if target.is_null() || target.is_undefined() {
        return None;
    }
    Reflect::get(target, &JsValue::from_str(key))
        .ok()
        .filter(|value| !value.is_null() && !value.is_undefined())
}

/// `globalThis[key]`.
pub(super) fn global_get(key: &str) -> Option<JsValue> {
    get(&global(), key)
}

/// Whether `key in target`.
pub(super) fn has(target: &JsValue, key: &str) -> bool {
    target.is_object() && Reflect::has(target, &JsValue::from_str(key)).unwrap_or(false)
}

/// `target[key] = value`.
pub(super) fn set(target: &JsValue, key: &str, value: &JsValue) {
    let _set = Reflect::set(target, &JsValue::from_str(key), value);
}

/// `target[method](...args)`.
pub(super) fn call(target: &JsValue, method: &str, args: &[JsValue]) -> Result<JsValue, JsValue> {
    let function: Function = get(target, method)
        .and_then(|f| f.dyn_into().ok())
        .ok_or_else(|| JsValue::from_str(&format!("{method} is not a function")))?;
    let args: Array = args.iter().collect();
    function.apply(target, &args)
}

/// A fresh `{}`.
pub(super) fn object() -> JsValue {
    Object::new().into()
}

/// Awaits `value` as a promise (a plain value resolves at once).
pub(super) async fn settle(value: JsValue) -> Result<JsValue, JsValue> {
    wasm_bindgen_futures::JsFuture::from(js_sys::Promise::resolve(&value)).await
}

/// A readable message of a thrown value.
pub(super) fn message(err: &JsValue) -> String {
    if let Some(text) = err.as_string() {
        return text;
    }
    let name = get(err, "name").and_then(|v| v.as_string());
    let text = get(err, "message").and_then(|v| v.as_string());
    match (name, text) {
        (Some(name), Some(text)) => format!("{name}: {text}"),
        (None, Some(text)) => text,
        (Some(name), None) => name,
        (None, None) => format!("{err:?}"),
    }
}

/// Whether `err` is a `TypeError`.
pub(super) fn is_type_error(err: &JsValue) -> bool {
    err.is_instance_of::<js_sys::TypeError>()
        || get(err, "name").and_then(|v| v.as_string()).as_deref() == Some("TypeError")
}

/// Runs the `!Send` future `make()` builds on the event loop and waits
/// for it from a `Send` future: the trait's futures must be `Send`, JS
/// values are not. `None` when the task was dropped unfinished.
///
/// Sound because `wasm32-unknown-unknown` is single-threaded: the task
/// and its waiter run on the same thread.
pub(super) fn local<T, F, Fut>(make: F) -> impl Future<Output = Option<T>> + Send
where
    T: Send + 'static,
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = T> + 'static,
{
    let (tx, rx) = oneshot::channel();
    wasm_bindgen_futures::spawn_local(async move {
        let _sent = tx.send(make().await);
    });
    async move { rx.await.ok() }
}
