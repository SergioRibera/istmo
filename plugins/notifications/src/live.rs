//! [`LiveNotification`]: progress without awaiting each step.
//!
//! Every live notification of one runtime shares one sender: a named
//! thread on native targets (a local task on the web) that keeps one call
//! in flight. [`LiveNotification::progress`] only stores the value in its
//! notification's queue (consecutive progress values collapse to the
//! latest) and wakes the sender; nothing blocks the caller. The sender
//! exits once every live notification is gone and its queue is empty.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use istmo_core::Runtime;

use crate::{
    Notification, NotificationError, NotificationId, NotificationPatch, NotificationsClient,
    Progress,
};

type Reply = oneshot::Sender<Result<(), NotificationError>>;

/// One queued call.
enum Op {
    Progress(Option<Progress>),
    Update(Box<NotificationPatch>, Reply),
    Show(Box<Notification>, Reply),
    Cancel(Option<Reply>),
}

#[derive(Default)]
struct Queue {
    /// Ids with queued calls, in turn order.
    turns: VecDeque<NotificationId>,
    ops: HashMap<NotificationId, VecDeque<Op>>,
}

impl Queue {
    fn push(&mut self, id: &NotificationId, op: Op) {
        let ops = self.ops.entry(id.clone()).or_default();
        if ops.is_empty() {
            self.turns.push_back(id.clone());
        }
        match op {
            Op::Progress(value) => {
                if let Some(Op::Progress(last)) = ops.back_mut() {
                    *last = value;
                } else {
                    ops.push_back(Op::Progress(value));
                }
            }
            terminal @ (Op::Show(..) | Op::Cancel(_)) => {
                // Progress queued before the end is moot.
                ops.retain(|queued| !matches!(queued, Op::Progress(_)));
                ops.push_back(terminal);
            }
            update @ Op::Update(..) => ops.push_back(update),
        }
    }

    /// The next call, round robin across ids.
    fn pop(&mut self) -> Option<(NotificationId, Op)> {
        while let Some(id) = self.turns.pop_front() {
            let Some(ops) = self.ops.get_mut(&id) else {
                continue;
            };
            let Some(op) = ops.pop_front() else {
                self.ops.remove(&id);
                continue;
            };
            if ops.is_empty() {
                self.ops.remove(&id);
            } else {
                self.turns.push_back(id.clone());
            }
            return Some((id, op));
        }
        None
    }
}

struct Shared {
    client: NotificationsClient,
    queue: Mutex<Queue>,
}

/// The sender of one runtime; dropped with its last live notification.
struct Hub {
    shared: Arc<Shared>,
    wake: flume::Sender<()>,
}

impl Hub {
    fn push(&self, id: &NotificationId, op: Op) {
        istmo_core::sync::lock(&self.shared.queue).push(id, op);
        // A full channel already holds a wake-up.
        let _woken = self.wake.try_send(());
    }
}

/// Hubs by runtime, so live notifications of one runtime share one sender.
type Registry = Mutex<Vec<(Weak<Runtime>, Weak<Hub>)>>;

fn hubs() -> &'static Registry {
    static HUBS: OnceLock<Registry> = OnceLock::new();
    HUBS.get_or_init(Mutex::default)
}

#[expect(
    clippy::significant_drop_tightening,
    reason = "the registry stays locked until the new hub is in it"
)]
fn hub_for(client: &NotificationsClient) -> Result<Arc<Hub>, NotificationError> {
    let runtime = client.runtime();
    let mut hubs = istmo_core::sync::lock(hubs());
    hubs.retain(|(rt, hub)| rt.strong_count() > 0 && hub.strong_count() > 0);
    if let Some(hub) = hubs
        .iter()
        .find(|(rt, _)| std::ptr::eq(rt.as_ptr(), Arc::as_ptr(runtime)))
        .and_then(|(_, hub)| hub.upgrade())
    {
        return Ok(hub);
    }
    let shared = Arc::new(Shared {
        client: NotificationsClient::from_runtime(runtime)?,
        queue: Mutex::default(),
    });
    let (wake, woken) = flume::bounded(1);
    start(Arc::clone(&shared), woken);
    let hub = Arc::new(Hub { shared, wake });
    hubs.push((Arc::downgrade(runtime), Arc::downgrade(&hub)));
    Ok(hub)
}

#[cfg(not(target_arch = "wasm32"))]
fn start(shared: Arc<Shared>, woken: flume::Receiver<()>) {
    let spawned = std::thread::Builder::new()
        .name("istmo-notifications-live".to_owned())
        .spawn(move || pollster::block_on(run(&shared, &woken)));
    if let Err(err) = spawned {
        tracing::warn!(%err, "notifications: the live-update sender could not start");
    }
}

#[cfg(target_arch = "wasm32")]
fn start(shared: Arc<Shared>, woken: flume::Receiver<()>) {
    wasm_bindgen_futures::spawn_local(async move { run(&shared, &woken).await });
}

/// Sends queued calls one at a time until the hub is gone and the queue
/// is empty.
async fn run(shared: &Shared, woken: &flume::Receiver<()>) {
    loop {
        let open = woken.recv_async().await.is_ok();
        loop {
            let next = istmo_core::sync::lock(&shared.queue).pop();
            let Some((id, op)) = next else {
                break;
            };
            send(&shared.client, id, op).await;
        }
        if !open {
            return;
        }
    }
}

async fn send(client: &NotificationsClient, id: NotificationId, op: Op) {
    match op {
        Op::Progress(progress) => {
            if let Err(err) = client.set_progress(id, progress).await {
                tracing::debug!(%err, "notifications: live progress update failed");
            }
        }
        Op::Update(patch, reply) => {
            let result = client
                .update(id, *patch)
                .await
                .map_err(NotificationError::from);
            let _sent = reply.send(result);
        }
        Op::Show(notification, reply) => {
            let result = client
                .show(*notification)
                .await
                .map_err(NotificationError::from);
            let _sent = reply.send(result);
        }
        Op::Cancel(reply) => {
            let result = client.cancel(id).await.map_err(NotificationError::from);
            if let Some(reply) = reply {
                let _sent = reply.send(result);
            } else if let Err(err) = result {
                tracing::debug!(%err, "notifications: cancelling a dropped live notification failed");
            }
        }
    }
}

/// A shown notification updated in place, cheaply and in order.
///
/// Calls on one live notification reach the host in the order they were
/// made; progress values between two other calls collapse to the latest.
/// Dropped without [`finish`](Self::finish) or [`detach`](Self::detach)
/// it is cancelled.
pub struct LiveNotification {
    id: NotificationId,
    label: Option<String>,
    hub: Arc<Hub>,
    armed: bool,
}

impl std::fmt::Debug for LiveNotification {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveNotification")
            .field("id", &self.id)
            .field("armed", &self.armed)
            .finish_non_exhaustive()
    }
}

impl LiveNotification {
    /// Wraps a notification already shown through `client`.
    pub(crate) fn new(
        client: &NotificationsClient,
        shown: &Notification,
    ) -> Result<Self, NotificationError> {
        Ok(Self {
            id: shown.id.clone(),
            label: shown.progress.as_ref().and_then(|p| p.label.clone()),
            hub: hub_for(client)?,
            armed: true,
        })
    }

    /// Its id.
    #[must_use]
    pub const fn id(&self) -> &NotificationId {
        &self.id
    }

    /// Sets the progress fraction (keeps the label it was shown with).
    /// Never waits; queued values collapse to the latest.
    pub fn progress(&self, value: f32) {
        let mut progress = Progress::fraction(value);
        progress.label.clone_from(&self.label);
        self.set_progress(Some(progress));
    }

    /// Sets (or with `None` removes) the whole progress bar. Never waits.
    pub fn set_progress(&self, progress: Option<Progress>) {
        self.hub.push(&self.id, Op::Progress(progress));
    }

    /// Applies `patch`, after every call made before it.
    pub async fn update(&self, patch: NotificationPatch) -> Result<(), NotificationError> {
        patch.validate()?;
        let (reply, answer) = oneshot::channel();
        self.hub.push(&self.id, Op::Update(Box::new(patch), reply));
        answered(answer).await
    }

    /// Replaces it with its final content (shown, alerting as a new
    /// notification). Queued progress is dropped. A `last` with another
    /// id cancels this one first.
    pub async fn finish(mut self, last: Notification) -> Result<(), NotificationError> {
        last.validate()?;
        self.armed = false;
        if last.id != self.id {
            self.hub.push(&self.id, Op::Cancel(None));
        }
        let (reply, answer) = oneshot::channel();
        self.hub.push(&self.id, Op::Show(Box::new(last), reply));
        answered(answer).await
    }

    /// Removes it now.
    pub async fn cancel(mut self) -> Result<(), NotificationError> {
        self.armed = false;
        let (reply, answer) = oneshot::channel();
        self.hub.push(&self.id, Op::Cancel(Some(reply)));
        answered(answer).await
    }

    /// Lets it outlive this handle: dropping no longer cancels it.
    /// Queued calls are still sent.
    pub fn detach(mut self) {
        self.armed = false;
    }
}

async fn answered(
    answer: oneshot::Receiver<Result<(), NotificationError>>,
) -> Result<(), NotificationError> {
    answer.await.unwrap_or_else(|_| {
        Err(NotificationError::Platform(crate::PlatformError::message(
            "the live-update sender stopped",
        )))
    })
}

impl Drop for LiveNotification {
    fn drop(&mut self) {
        if self.armed {
            self.hub.push(&self.id, Op::Cancel(None));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(value: &str) -> NotificationId {
        NotificationId::try_from(value).unwrap()
    }

    fn label(op: &Op) -> String {
        match op {
            Op::Progress(p) => format!("p{:?}", p.as_ref().and_then(|p| p.value)),
            Op::Update(..) => "update".to_owned(),
            Op::Show(..) => "show".to_owned(),
            Op::Cancel(_) => "cancel".to_owned(),
        }
    }

    #[test]
    fn progress_collapses_and_ends_drop_it() {
        let mut q = Queue::default();
        let (a, b) = (id("a"), id("b"));
        q.push(&a, Op::Progress(Some(Progress::fraction(0.1))));
        q.push(&a, Op::Progress(Some(Progress::fraction(0.2))));
        q.push(&b, Op::Progress(None));
        let (reply, _answer) = oneshot::channel();
        q.push(&a, Op::Update(Box::default(), reply));
        q.push(&a, Op::Progress(Some(Progress::fraction(0.3))));
        q.push(&a, Op::Progress(Some(Progress::fraction(0.4))));
        let mut got = Vec::new();
        while let Some((id, op)) = q.pop() {
            got.push(format!("{id}:{}", label(&op)));
        }
        assert_eq!(got, ["a:pSome(0.2)", "b:pNone", "a:update", "a:pSome(0.4)"]);
        q.push(&a, Op::Progress(Some(Progress::fraction(0.5))));
        q.push(&a, Op::Cancel(None));
        let (id, op) = q.pop().unwrap();
        assert_eq!(format!("{id}:{}", label(&op)), "a:cancel");
        assert!(q.pop().is_none());
    }
}
