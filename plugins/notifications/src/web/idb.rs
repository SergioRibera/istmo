//! `IndexedDB`, the few calls the host needs: the schedule bytes and the
//! queue of clicks and closes the service worker leaves.
//!
//! Database [`DB_NAME`] version [`DB_VERSION`], stores `schedule` (the
//! core store format under the key `entries`) and `responses`
//! (auto-increment keys, one object per click or close). The worker
//! script creates the same stores; keep both in step.

#![allow(
    clippy::future_not_send,
    reason = "wasm32-unknown-unknown is single-threaded: these futures run on the JS event loop, behind `js::local` where a `Send` one is needed"
)]

use std::cell::RefCell;
use std::rc::Rc;

use js_sys::{Array, Uint8Array};
use wasm_bindgen::JsValue;
use wasm_bindgen::closure::Closure;

use super::js;
use crate::web_assets::{DB_NAME, DB_VERSION};

const SCHEDULE: &str = "schedule";
const QUEUE: &str = "responses";
const ENTRIES: &str = "entries";

/// The success and failure callbacks of a [`Waiter`].
type Handlers = (Closure<dyn FnMut()>, Closure<dyn FnMut()>);

/// Handlers on an `IndexedDB` request or transaction, installed at once
/// (before any await, so no event is missed) and removed when done.
struct Waiter {
    target: JsValue,
    ok: &'static str,
    fail: &'static [&'static str],
    rx: oneshot::Receiver<bool>,
    handlers: Handlers,
}

impl Waiter {
    fn new(target: &JsValue, ok: &'static str, fail: &'static [&'static str]) -> Self {
        let (tx, rx) = oneshot::channel::<bool>();
        let tx = Rc::new(RefCell::new(Some(tx)));
        let succeed = {
            let tx = Rc::clone(&tx);
            Closure::<dyn FnMut()>::new(move || {
                if let Some(tx) = tx.borrow_mut().take() {
                    let _sent = tx.send(true);
                }
            })
        };
        let failed = Closure::<dyn FnMut()>::new(move || {
            if let Some(tx) = tx.borrow_mut().take() {
                let _sent = tx.send(false);
            }
        });
        js::set(target, ok, succeed.as_ref());
        for event in fail {
            js::set(target, event, failed.as_ref());
        }
        Self {
            target: target.clone(),
            ok,
            fail,
            rx,
            handlers: (succeed, failed),
        }
    }

    /// `target[result]` on success, `target.error` otherwise.
    async fn done(self, result: Option<&str>) -> Result<JsValue, JsValue> {
        let Self {
            target,
            ok,
            fail,
            rx,
            handlers,
        } = self;
        let done = rx.await.unwrap_or(false);
        js::set(&target, ok, &JsValue::NULL);
        for event in fail {
            js::set(&target, event, &JsValue::NULL);
        }
        drop(handlers);
        if done {
            Ok(result
                .and_then(|key| js::get(&target, key))
                .unwrap_or(JsValue::UNDEFINED))
        } else {
            Err(js::get(&target, "error")
                .unwrap_or_else(|| JsValue::from_str("IndexedDB request failed")))
        }
    }
}

/// Opens the database, creating the stores on first use; `None` where
/// `IndexedDB` is missing or refused (some private modes).
pub(super) async fn open() -> Option<JsValue> {
    let factory = js::global_get("indexedDB")?;
    let request = js::call(
        &factory,
        "open",
        &[JsValue::from_str(DB_NAME), JsValue::from(DB_VERSION)],
    )
    .ok()?;
    let upgrade = {
        let request = request.clone();
        Closure::<dyn FnMut()>::new(move || {
            let Some(db) = js::get(&request, "result") else {
                return;
            };
            let names = js::get(&db, "objectStoreNames");
            let exists = |name: &str| {
                names
                    .as_ref()
                    .and_then(|names| js::call(names, "contains", &[JsValue::from_str(name)]).ok())
                    .is_some_and(|v| v.is_truthy())
            };
            if !exists(SCHEDULE) {
                let _created = js::call(&db, "createObjectStore", &[JsValue::from_str(SCHEDULE)]);
            }
            if !exists(QUEUE) {
                let options = js::object();
                js::set(&options, "autoIncrement", &JsValue::TRUE);
                let _created = js::call(
                    &db,
                    "createObjectStore",
                    &[JsValue::from_str(QUEUE), options],
                );
            }
        })
    };
    js::set(&request, "onupgradeneeded", upgrade.as_ref());
    let waiter = Waiter::new(&request, "onsuccess", &["onerror", "onblocked"]);
    let opened = waiter.done(Some("result")).await;
    js::set(&request, "onupgradeneeded", &JsValue::NULL);
    drop(upgrade);
    match opened {
        Ok(db) if db.is_object() => Some(db),
        Ok(_) => None,
        Err(err) => {
            tracing::warn!(err = %js::message(&err), "notifications: IndexedDB unavailable; schedule kept in memory");
            None
        }
    }
}

fn store(db: &JsValue, name: &str, mode: &str) -> Result<(JsValue, JsValue), JsValue> {
    let tx = js::call(
        db,
        "transaction",
        &[JsValue::from_str(name), JsValue::from_str(mode)],
    )?;
    let store = js::call(&tx, "objectStore", &[JsValue::from_str(name)])?;
    Ok((tx, store))
}

/// The stored schedule bytes.
pub(super) async fn load_schedule(db: &JsValue) -> Result<Option<Vec<u8>>, JsValue> {
    let (_tx, store) = store(db, SCHEDULE, "readonly")?;
    let request = js::call(&store, "get", &[JsValue::from_str(ENTRIES)])?;
    let value = Waiter::new(&request, "onsuccess", &["onerror"])
        .done(Some("result"))
        .await?;
    Ok(value.is_object().then(|| Uint8Array::new(&value).to_vec()))
}

/// Replaces the schedule bytes (`None` deletes them). The transaction is
/// created before the first await, so writes land in call order.
pub(super) async fn save_schedule(db: &JsValue, bytes: Option<&[u8]>) -> Result<(), JsValue> {
    let (tx, store) = store(db, SCHEDULE, "readwrite")?;
    let key = JsValue::from_str(ENTRIES);
    match bytes {
        Some(bytes) => js::call(&store, "put", &[Uint8Array::from(bytes).into(), key])?,
        None => js::call(&store, "delete", &[key])?,
    };
    Waiter::new(&tx, "oncomplete", &["onerror", "onabort"])
        .done(None)
        .await
        .map(drop)
}

/// Takes every queued click and close, in queue order, in one
/// transaction (with several tabs open each entry reaches one).
pub(super) async fn drain(db: &JsValue) -> Result<Vec<JsValue>, JsValue> {
    let (tx, store) = store(db, QUEUE, "readwrite")?;
    let all = js::call(&store, "getAll", &[])?;
    js::call(&store, "clear", &[])?;
    let got = Waiter::new(&all, "onsuccess", &["onerror"]);
    let committed = Waiter::new(&tx, "oncomplete", &["onerror", "onabort"]);
    let entries = got.done(Some("result")).await?;
    committed.done(None).await?;
    Ok(Array::from(&entries).iter().collect())
}
