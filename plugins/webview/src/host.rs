//! Building blocks shared by the Rust-side hosts (desktop backends and
//! the scripted test host): capability tokens, the view table, the two
//! event lanes and single-use request ids.
//!
//! Producers on engine threads only ever call non-blocking methods here,
//! so a slow or absent subscriber can never stall a UI thread.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use flume::{Receiver, Sender, TrySendError};
use istmo_core::NativeHandleId;
use istmo_core::sync::lock;

use crate::{ControlEvent, TelemetryEvent, TelemetryKind, ViewRef, WebViewError, WebViewInstance};

/// Control events buffered before the first subscriber; past this, new
/// events are dropped (decisions among them time out to their safe
/// default).
pub const CONTROL_PREBUFFER: usize = 1024;

/// Per-subscriber telemetry queue depth.
pub const TELEMETRY_DEPTH: usize = 64;

/// Random `u64` for tokens and request ids.
///
/// # Panics
/// When the OS random source fails, which leaves no safe way to mint
/// capability tokens.
#[must_use]
pub fn random_u64() -> u64 {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let mut bytes = [0_u8; 8];
        getrandom::fill(&mut bytes).expect("invariant: the OS random source is available");
        u64::from_ne_bytes(bytes)
    }
    #[cfg(target_arch = "wasm32")]
    {
        // No Rust host runs on wasm (the web backend lives in the page),
        // so ids only need to be distinct here.
        use std::hash::{BuildHasher, RandomState};
        RandomState::new().hash_one(std::ptr::from_ref(&()) as usize)
    }
}

/// 128-bit capability token of a view.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ViewToken {
    hi: u64,
    lo: u64,
}

impl std::fmt::Debug for ViewToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ViewToken(<redacted>)")
    }
}

impl ViewToken {
    /// Fresh random token.
    #[must_use]
    pub fn generate() -> Self {
        Self {
            hi: random_u64(),
            lo: random_u64(),
        }
    }

    /// Wire instance for `view` carrying this token.
    #[must_use]
    pub const fn instance(self, view: NativeHandleId) -> WebViewInstance {
        WebViewInstance {
            view,
            token_hi: self.hi,
            token_lo: self.lo,
        }
    }

    /// Constant-time comparison with the token of `view`.
    #[must_use]
    pub const fn matches(self, view: &ViewRef) -> bool {
        ((self.hi ^ view.token_hi) | (self.lo ^ view.token_lo)) == 0
    }
}

/// Views of one host instance, keyed by handle id.
#[derive(Debug)]
pub struct ViewTable<V> {
    views: Mutex<HashMap<NativeHandleId, (ViewToken, Arc<V>)>>,
}

impl<V> Default for ViewTable<V> {
    fn default() -> Self {
        Self {
            views: Mutex::default(),
        }
    }
}

impl<V> ViewTable<V> {
    /// Store `view` under `id` with a fresh token.
    pub fn insert(&self, id: NativeHandleId, view: Arc<V>) -> WebViewInstance {
        let token = ViewToken::generate();
        lock(&self.views).insert(id, (token, view));
        token.instance(id)
    }

    /// The view `r` names, when its token matches.
    ///
    /// # Errors
    /// [`WebViewError::ViewClosed`] for an unknown id,
    /// [`WebViewError::InvalidToken`] for a wrong token.
    pub fn get(&self, r: &ViewRef) -> Result<Arc<V>, WebViewError> {
        let (token, view) = lock(&self.views)
            .get(&r.view)
            .map(|(token, view)| (*token, Arc::clone(view)))
            .ok_or(WebViewError::ViewClosed)?;
        if token.matches(r) {
            Ok(view)
        } else {
            Err(WebViewError::InvalidToken)
        }
    }

    /// Remove the view `r` names, checking its token.
    ///
    /// # Errors
    /// As [`Self::get`].
    pub fn remove(&self, r: &ViewRef) -> Result<Arc<V>, WebViewError> {
        let mut views = lock(&self.views);
        match views.get(&r.view) {
            None => Err(WebViewError::ViewClosed),
            Some((token, _)) if !token.matches(r) => Err(WebViewError::InvalidToken),
            Some(_) => Ok(views
                .remove(&r.view)
                .map(|(_, v)| v)
                .ok_or(WebViewError::ViewClosed)?),
        }
    }

    /// Remove by id alone: the native handle release path, which only
    /// the owner of the handle can trigger.
    pub fn release(&self, id: NativeHandleId) -> Option<Arc<V>> {
        lock(&self.views).remove(&id).map(|(_, v)| v)
    }

    /// Ids of the live views.
    pub fn ids(&self) -> Vec<NativeHandleId> {
        lock(&self.views).keys().copied().collect()
    }

    /// The view `id` without a token check, for code that already owns
    /// the view (engine callbacks, test scripts).
    pub fn by_id(&self, id: NativeHandleId) -> Option<Arc<V>> {
        lock(&self.views).get(&id).map(|(_, v)| Arc::clone(v))
    }

    /// Every live view, for shutdown.
    pub fn drain(&self) -> Vec<Arc<V>> {
        lock(&self.views).drain().map(|(_, (_, v))| v).collect()
    }
}

type ControlItem = Result<ControlEvent, WebViewError>;

/// Lossless per-view control lane with a single subscriber.
#[derive(Debug, Default)]
pub struct ControlLane {
    state: Mutex<ControlState>,
}

#[derive(Debug, Default)]
struct ControlState {
    /// Events published before anyone subscribed.
    pending: VecDeque<ControlEvent>,
    subscriber: Option<Sender<ControlItem>>,
    closed: bool,
}

impl ControlLane {
    /// Deliver `event`. Never blocks.
    pub fn publish(&self, event: ControlEvent) {
        let mut state = lock(&self.state);
        if state.closed {
            return;
        }
        if let Some(tx) = &state.subscriber {
            match tx.try_send(Ok(event)) {
                // The subscriber went away: buffer for the next one.
                Err(TrySendError::Disconnected(Ok(event)) | TrySendError::Full(Ok(event))) => {
                    state.subscriber = None;
                    state.push_pending(event);
                    return;
                }
                Ok(()) | Err(TrySendError::Disconnected(Err(_)) | TrySendError::Full(Err(_))) => {
                    return;
                }
            }
        }
        state.push_pending(event);
    }

    /// Become the subscriber. Buffered events come first.
    ///
    /// # Errors
    /// [`WebViewError::InvalidArgument`] while another live subscriber
    /// holds the lane; [`WebViewError::ViewClosed`] after [`Self::close`].
    pub fn subscribe(&self) -> Result<Receiver<ControlItem>, WebViewError> {
        let mut state = lock(&self.state);
        if state.closed {
            return Err(WebViewError::ViewClosed);
        }
        if state
            .subscriber
            .as_ref()
            .is_some_and(|tx| !tx.is_disconnected())
        {
            return Err(WebViewError::InvalidArgument(
                "control events already have a subscriber".into(),
            ));
        }
        // Unbounded: the lane is lossless and producers must not block.
        let (tx, rx) = flume::unbounded();
        for event in state.pending.drain(..) {
            drop(tx.send(Ok(event)));
        }
        state.subscriber = Some(tx);
        drop(state);
        Ok(rx)
    }

    /// End the lane; the subscriber's stream completes.
    pub fn close(&self) {
        let mut state = lock(&self.state);
        state.closed = true;
        state.subscriber = None;
        state.pending.clear();
    }
}

impl ControlState {
    fn push_pending(&mut self, event: ControlEvent) {
        if self.pending.len() >= CONTROL_PREBUFFER {
            tracing::warn!("web view control events overflowed before anyone subscribed");
            return;
        }
        self.pending.push_back(event);
    }
}

type TelemetryItem = Result<TelemetryEvent, WebViewError>;

/// Lossy per-view telemetry lane; any number of subscribers, each with
/// its own mask and bounded queue.
#[derive(Debug, Default)]
pub struct TelemetryLane {
    subscribers: Mutex<Vec<TelemetrySub>>,
}

#[derive(Debug)]
struct TelemetrySub {
    mask: Vec<TelemetryKind>,
    tx: Sender<TelemetryItem>,
    dropped: u32,
}

impl TelemetryLane {
    /// Deliver `event` to every subscriber that asked for its kind.
    /// Never blocks; a full queue drops the event and reports the loss
    /// with the next delivery.
    pub fn publish(&self, event: &TelemetryEvent) {
        let kind = telemetry_kind(event);
        let mut subs = lock(&self.subscribers);
        subs.retain_mut(|sub| {
            if kind.is_some_and(|k| !sub.mask.contains(&k)) {
                return !sub.tx.is_disconnected();
            }
            if sub.dropped > 0 {
                match sub.tx.try_send(Ok(TelemetryEvent::Dropped(sub.dropped))) {
                    Ok(()) => sub.dropped = 0,
                    Err(TrySendError::Full(_)) => {
                        sub.dropped = sub.dropped.saturating_add(1);
                        return true;
                    }
                    Err(TrySendError::Disconnected(_)) => return false,
                }
            }
            match sub.tx.try_send(Ok(event.clone())) {
                Ok(()) => true,
                Err(TrySendError::Full(_)) => {
                    sub.dropped = sub.dropped.saturating_add(1);
                    true
                }
                Err(TrySendError::Disconnected(_)) => false,
            }
        });
    }

    /// New subscriber for the kinds in `mask`.
    pub fn subscribe(&self, mask: Vec<TelemetryKind>) -> Receiver<TelemetryItem> {
        let (tx, rx) = flume::bounded(TELEMETRY_DEPTH);
        lock(&self.subscribers).push(TelemetrySub {
            mask,
            tx,
            dropped: 0,
        });
        rx
    }

    /// Whether anyone listens for `kind`; producers skip building
    /// events nobody reads.
    #[must_use]
    pub fn wants(&self, kind: TelemetryKind) -> bool {
        lock(&self.subscribers)
            .iter()
            .any(|sub| sub.mask.contains(&kind) && !sub.tx.is_disconnected())
    }

    /// End every subscriber's stream.
    pub fn close(&self) {
        lock(&self.subscribers).clear();
    }
}

const fn telemetry_kind(event: &TelemetryEvent) -> Option<TelemetryKind> {
    Some(match event {
        TelemetryEvent::Progress(_) => TelemetryKind::Progress,
        TelemetryEvent::Title(_) => TelemetryKind::Title,
        TelemetryEvent::Url(_) => TelemetryKind::Url,
        TelemetryEvent::Favicon(_) => TelemetryKind::Favicon,
        TelemetryEvent::History(_) => TelemetryKind::History,
        TelemetryEvent::Console(_) => TelemetryKind::Console,
        TelemetryEvent::DownloadUpdate(_) | TelemetryEvent::DownloadFinished(_) => {
            TelemetryKind::Downloads
        }
        TelemetryEvent::Dropped(_) => return None,
    })
}

/// Outstanding requests (decisions, scheme requests, bridge replies)
/// keyed by random single-use ids.
#[derive(Debug)]
pub struct PendingTable<T> {
    entries: Mutex<HashMap<u64, T>>,
}

impl<T> Default for PendingTable<T> {
    fn default() -> Self {
        Self {
            entries: Mutex::default(),
        }
    }
}

impl<T> PendingTable<T> {
    /// Store `value` under a fresh id.
    pub fn insert(&self, value: T) -> u64 {
        let mut entries = lock(&self.entries);
        loop {
            let id = random_u64();
            if id != 0 && !entries.contains_key(&id) {
                entries.insert(id, value);
                drop(entries);
                return id;
            }
        }
    }

    /// Take the entry; a second take of the same id returns `None`.
    pub fn take(&self, id: u64) -> Option<T> {
        lock(&self.entries).remove(&id)
    }

    /// Take every entry (navigation away, crash, close).
    pub fn drain(&self) -> Vec<T> {
        lock(&self.entries).drain().map(|(_, v)| v).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TerminationInfo;

    fn event(n: u64) -> ControlEvent {
        ControlEvent::Intercepted(crate::InterceptedNav {
            navigation_id: n,
            url: String::new(),
        })
    }

    #[test]
    fn control_lane_buffers_until_first_subscriber() {
        let lane = ControlLane::default();
        lane.publish(event(1));
        lane.publish(event(2));
        let rx = lane.subscribe().unwrap();
        lane.publish(event(3));
        let got: Vec<_> = rx.try_iter().map(Result::unwrap).collect();
        assert_eq!(got, vec![event(1), event(2), event(3)]);
    }

    #[test]
    fn control_lane_has_one_live_subscriber() {
        let lane = ControlLane::default();
        let rx = lane.subscribe().unwrap();
        assert!(lane.subscribe().is_err());
        drop(rx);
        lane.publish(event(7));
        let rx = lane.subscribe().unwrap();
        assert_eq!(rx.try_recv().unwrap().unwrap(), event(7));
        lane.close();
        lane.publish(ControlEvent::ProcessTerminated(TerminationInfo {
            reason: crate::TerminationReason::Crashed,
        }));
        assert!(rx.recv().is_err(), "closed lane ends the stream");
    }

    #[test]
    fn telemetry_drops_and_reports() {
        let lane = TelemetryLane::default();
        let rx = lane.subscribe(vec![TelemetryKind::Console]);
        assert!(!lane.wants(TelemetryKind::Progress));
        lane.publish(&TelemetryEvent::Progress(0.5));
        assert!(rx.is_empty(), "unsubscribed kind is not delivered");
        let console_line = TelemetryEvent::Console(crate::ConsoleLine {
            level: crate::ConsoleLevel::Log,
            message: "x".into(),
            source: String::new(),
            line: 0,
        });
        for _ in 0..TELEMETRY_DEPTH + 5 {
            lane.publish(&console_line);
        }
        assert_eq!(rx.drain().count(), TELEMETRY_DEPTH);
        lane.publish(&console_line);
        assert_eq!(rx.try_recv().unwrap().unwrap(), TelemetryEvent::Dropped(5));
    }

    #[test]
    fn view_table_checks_tokens() {
        let table = ViewTable::default();
        let id = NativeHandleId(9);
        let instance = table.insert(id, Arc::new(()));
        let good = ViewRef {
            view: id,
            token_hi: instance.token_hi,
            token_lo: instance.token_lo,
        };
        let bad = ViewRef {
            token_lo: instance.token_lo ^ 1,
            ..good
        };
        assert!(table.get(&good).is_ok());
        assert_eq!(table.get(&bad).unwrap_err(), WebViewError::InvalidToken);
        assert_eq!(table.remove(&bad).unwrap_err(), WebViewError::InvalidToken);
        assert!(table.remove(&good).is_ok());
        assert_eq!(table.get(&good).unwrap_err(), WebViewError::ViewClosed);
    }

    #[test]
    fn pending_ids_are_single_use() {
        let table = PendingTable::default();
        let id = table.insert("a");
        assert_eq!(table.take(id), Some("a"));
        assert_eq!(table.take(id), None);
    }
}
