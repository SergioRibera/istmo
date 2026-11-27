//! `IndexedDB`, the few calls the host needs: the scheduled records and
//! the queue of clicks and closes the service worker leaves.
//!
//! Database [`DB_NAME`] version [`DB_VERSION`], stores
//! [`SCHEDULE_STORE`] (one record per scheduled notification, key = its
//! id, value = a one-entry core store) and [`QUEUE_STORE`]
//! (auto-increment keys, one object per click or close). The worker
//! script creates the same stores; keep both in step. Writes touch only
//! the records they change, so tabs never overwrite each other's
//! entries.

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
use crate::NotificationId;
use crate::web_assets::{DB_NAME, DB_VERSION, QUEUE_STORE, SCHEDULE_STORE};

/// The store of the previous layout (the whole schedule under one key),
/// dropped on upgrade: nothing shipped with it.
const OLD_SCHEDULE: &str = "schedule";

/// How long an open may wait (another tab's older connection blocks an
/// upgrade until it closes) before the host goes on in memory.
const OPEN_WAIT_MS: i32 = 5_000;

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

/// Creates the stores of this layout (an `upgradeneeded` handler).
fn upgrade(db: &JsValue) {
    let names = js::get(db, "objectStoreNames");
    let exists = |name: &str| {
        names
            .as_ref()
            .and_then(|names| js::call(names, "contains", &[JsValue::from_str(name)]).ok())
            .is_some_and(|v| v.is_truthy())
    };
    if exists(OLD_SCHEDULE) {
        let _deleted = js::call(db, "deleteObjectStore", &[JsValue::from_str(OLD_SCHEDULE)]);
    }
    if !exists(SCHEDULE_STORE) {
        let _created = js::call(
            db,
            "createObjectStore",
            &[JsValue::from_str(SCHEDULE_STORE)],
        );
    }
    if !exists(QUEUE_STORE) {
        let options = js::object();
        js::set(&options, "autoIncrement", &JsValue::TRUE);
        let _created = js::call(
            db,
            "createObjectStore",
            &[JsValue::from_str(QUEUE_STORE), options],
        );
    }
}

/// Closes `db` when a newer version wants to upgrade (another tab
/// loaded a newer app), so this tab never blocks it.
fn close_on_versionchange(db: &JsValue) {
    let target = db.clone();
    let close = Closure::<dyn FnMut()>::new(move || {
        let _closed = js::call(&target, "close", &[]);
    });
    js::set(db, "onversionchange", close.as_ref());
    // Lives as long as the connection: one per page.
    close.forget();
}

/// Opens the database, creating the stores on first use; `None` where
/// `IndexedDB` is missing, refused (some private modes) or blocked for
/// longer than [`OPEN_WAIT_MS`] by another tab's older connection.
/// `blocked` is not a failure: the open goes on once that connection
/// closes, and a connection that arrives after the wait is closed.
pub(super) async fn open() -> Option<JsValue> {
    let factory = js::global_get("indexedDB")?;
    let request = js::call(
        &factory,
        "open",
        &[JsValue::from_str(DB_NAME), JsValue::from(DB_VERSION)],
    )
    .ok()?;
    let on_upgrade = {
        let request = request.clone();
        Closure::<dyn FnMut()>::new(move || {
            if let Some(db) = js::get(&request, "result") {
                upgrade(&db);
            }
        })
    };
    js::set(&request, "onupgradeneeded", on_upgrade.as_ref());
    let on_blocked = Closure::<dyn FnMut()>::new(|| {
        tracing::info!(
            "notifications: IndexedDB upgrade waits for another tab of the app to close its connection"
        );
    });
    js::set(&request, "onblocked", on_blocked.as_ref());
    let settled = {
        let request = request.clone();
        js_sys::Promise::new(&mut |resolve, reject| {
            js::set(&request, "onsuccess", &resolve);
            js::set(&request, "onerror", &reject);
        })
    };
    let raced = js_sys::Promise::race(&Array::of2(&settled, &super::host::sleep(OPEN_WAIT_MS)));
    let outcome = js::settle(raced.into()).await;
    let db = js::get(&request, "result").filter(|db| db.is_object() && outcome.is_ok());
    let opened = match (&outcome, db) {
        (Ok(event), Some(db)) if !event.is_undefined() => Some(db),
        (Ok(_), _) => {
            tracing::warn!(
                "notifications: IndexedDB did not open in time; schedule kept in memory"
            );
            // A late success must not leave an open connection behind.
            let late = request.clone();
            let close = Closure::once_into_js(move || {
                if let Some(db) = js::get(&late, "result") {
                    let _closed = js::call(&db, "close", &[]);
                }
            });
            js::set(&request, "onsuccess", &close);
            None
        }
        (Err(err), _) => {
            tracing::warn!(err = %js::message(&err_of(&request, err)), "notifications: IndexedDB unavailable; schedule kept in memory");
            None
        }
    };
    js::set(&request, "onupgradeneeded", &JsValue::NULL);
    js::set(&request, "onblocked", &JsValue::NULL);
    drop((on_upgrade, on_blocked));
    if let Some(db) = &opened {
        close_on_versionchange(db);
    }
    opened
}

/// The request's error, else the rejection value.
fn err_of(request: &JsValue, rejection: &JsValue) -> JsValue {
    js::get(request, "error").unwrap_or_else(|| rejection.clone())
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

/// Every stored record.
pub(super) async fn load_records(db: &JsValue) -> Result<Vec<Vec<u8>>, JsValue> {
    let (_tx, store) = store(db, SCHEDULE_STORE, "readonly")?;
    let request = js::call(&store, "getAll", &[])?;
    let values = Waiter::new(&request, "onsuccess", &["onerror"])
        .done(Some("result"))
        .await?;
    Ok(Array::from(&values)
        .iter()
        .filter(JsValue::is_object)
        .map(|value| Uint8Array::new(&value).to_vec())
        .collect())
}

/// Writes (`Some`) or deletes (`None`) the records of these ids in one
/// transaction. The transaction is created before the first await, so
/// writes of one tab land in call order.
pub(super) async fn write_records(
    db: &JsValue,
    writes: &[(NotificationId, Option<Vec<u8>>)],
) -> Result<(), JsValue> {
    let (tx, store) = store(db, SCHEDULE_STORE, "readwrite")?;
    for (id, record) in writes {
        let key = JsValue::from_str(id.as_str());
        match record {
            Some(bytes) => js::call(
                &store,
                "put",
                &[Uint8Array::from(bytes.as_slice()).into(), key],
            )?,
            None => js::call(&store, "delete", &[key])?,
        };
    }
    Waiter::new(&tx, "oncomplete", &["onerror", "onabort"])
        .done(None)
        .await
        .map(drop)
}

/// Takes every queued click and close, in queue order, in one
/// transaction (with several tabs open each entry reaches one).
pub(super) async fn drain(db: &JsValue) -> Result<Vec<JsValue>, JsValue> {
    let (tx, store) = store(db, QUEUE_STORE, "readwrite")?;
    let all = js::call(&store, "getAll", &[])?;
    js::call(&store, "clear", &[])?;
    let got = Waiter::new(&all, "onsuccess", &["onerror"]);
    let committed = Waiter::new(&tx, "oncomplete", &["onerror", "onabort"]);
    let entries = got.done(Some("result")).await?;
    committed.done(None).await?;
    Ok(Array::from(&entries).iter().collect())
}
