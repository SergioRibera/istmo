//! Slice A verification: `ServiceContext` talks to a hosted `ServiceControl`
//! implementation via the frame protocol, and `StopNotifier` drives the
//! `stopped()` future.

use std::sync::{Arc, Mutex};

use istmo_core::{CancelToken, Runtime};
use istmo_plugins::{
    NotificationSpec, ServiceContext, ServiceControl, ServiceControlError, ServiceControlHost,
    WakelockToken, stop_channel,
};

#[derive(Debug, Default, Clone)]
struct Recorded {
    foreground: Vec<(String, NotificationSpec)>,
    updates: Vec<(String, NotificationSpec)>,
    stop_foregrounds: Vec<(String, bool)>,
    wakelocks: Vec<(String, String)>,
    released: Vec<(String, WakelockToken)>,
    stops: Vec<String>,
    next_token: u64,
}

#[derive(Debug, Default, Clone)]
struct MockControl {
    inner: Arc<Mutex<Recorded>>,
}

impl MockControl {
    fn snapshot(&self) -> Recorded {
        self.inner.lock().unwrap().clone()
    }
}

impl ServiceControl for MockControl {
    async fn start_foreground(
        &self,
        service_id: String,
        spec: NotificationSpec,
    ) -> Result<(), ServiceControlError> {
        self.inner
            .lock()
            .unwrap()
            .foreground
            .push((service_id, spec));
        Ok(())
    }

    async fn update_notification(
        &self,
        service_id: String,
        spec: NotificationSpec,
    ) -> Result<(), ServiceControlError> {
        self.inner.lock().unwrap().updates.push((service_id, spec));
        Ok(())
    }

    async fn stop_foreground(
        &self,
        service_id: String,
        remove_notification: bool,
    ) -> Result<(), ServiceControlError> {
        self.inner
            .lock()
            .unwrap()
            .stop_foregrounds
            .push((service_id, remove_notification));
        Ok(())
    }

    async fn acquire_wakelock(
        &self,
        service_id: String,
        tag: String,
    ) -> Result<WakelockToken, ServiceControlError> {
        let token = {
            let mut g = self.inner.lock().unwrap();
            g.next_token += 1;
            g.wakelocks.push((service_id, tag));
            WakelockToken(g.next_token)
        };
        Ok(token)
    }

    async fn release_wakelock(
        &self,
        service_id: String,
        token: WakelockToken,
    ) -> Result<(), ServiceControlError> {
        self.inner
            .lock()
            .unwrap()
            .released
            .push((service_id, token));
        Ok(())
    }

    async fn stop_self(&self, service_id: String) -> Result<(), ServiceControlError> {
        self.inner.lock().unwrap().stops.push(service_id);
        Ok(())
    }
}

fn spec() -> NotificationSpec {
    NotificationSpec {
        channel_id: "sync".to_owned(),
        notification_id: 42,
        title: "Syncing".to_owned(),
        body: "Uploading data".to_owned(),
        small_icon: None,
        ongoing: true,
        foreground_service_type: Some(1),
    }
}

fn runtime_with_mock() -> (Arc<Runtime>, MockControl) {
    let mock = MockControl::default();
    let host = ServiceControlHost::new(mock.clone());
    let init = Runtime::mock().host(host);
    (init.runtime, mock)
}

#[test]
fn set_foreground_reaches_host() {
    let (rt, mock) = runtime_with_mock();
    let (_notifier, stop_rx) = stop_channel();
    let ctx = ServiceContext::new(rt, "myapp.sync".to_owned(), stop_rx, CancelToken::new());

    pollster::block_on(ctx.set_foreground(spec())).unwrap();

    let snap = mock.snapshot();
    assert_eq!(snap.foreground.len(), 1);
    assert_eq!(snap.foreground[0].0, "myapp.sync");
    assert_eq!(snap.foreground[0].1.title, "Syncing");
}

#[test]
fn update_and_stop_foreground_reach_host() {
    let (rt, mock) = runtime_with_mock();
    let (_notifier, stop_rx) = stop_channel();
    let ctx = ServiceContext::new(rt, "myapp.sync".to_owned(), stop_rx, CancelToken::new());

    pollster::block_on(async {
        ctx.update_notification(spec()).await.unwrap();
        ctx.stop_foreground(true).await.unwrap();
    });

    let snap = mock.snapshot();
    assert_eq!(snap.updates.len(), 1);
    assert_eq!(snap.stop_foregrounds, vec![("myapp.sync".to_owned(), true)]);
}

#[test]
fn wakelock_acquire_returns_token_and_release_forwards_it() {
    let (rt, mock) = runtime_with_mock();
    let (_notifier, stop_rx) = stop_channel();
    let ctx = ServiceContext::new(rt, "myapp.sync".to_owned(), stop_rx, CancelToken::new());

    pollster::block_on(async {
        let lock = ctx.acquire_wakelock("network").await.unwrap();
        assert_eq!(lock.token(), WakelockToken(1));
        lock.release().await.unwrap();
    });

    let snap = mock.snapshot();
    assert_eq!(
        snap.wakelocks,
        vec![("myapp.sync".to_owned(), "network".to_owned())]
    );
    assert_eq!(
        snap.released,
        vec![("myapp.sync".to_owned(), WakelockToken(1))]
    );
}

#[test]
fn stop_signal_wakes_stopped_future() {
    let (rt, _mock) = runtime_with_mock();
    let (notifier, stop_rx) = stop_channel();
    let ctx = ServiceContext::new(rt, "myapp.sync".to_owned(), stop_rx, CancelToken::new());

    assert!(!ctx.is_stopped());
    notifier.signal();
    pollster::block_on(ctx.stopped());
    assert!(ctx.is_stopped());
}

#[test]
fn cancel_token_trips_is_stopped_and_wakes_stopped_future() {
    let (rt, _mock) = runtime_with_mock();
    let (_notifier, stop_rx) = stop_channel();
    let cancel = CancelToken::new();
    let ctx = ServiceContext::new(rt, "myapp.sync".to_owned(), stop_rx, cancel.clone());

    assert!(!ctx.is_stopped());
    cancel.cancel();
    assert!(ctx.is_stopped(), "cancel token trip should surface as stopped");
    pollster::block_on(ctx.stopped());
}

#[test]
fn stop_self_reaches_host() {
    let (rt, mock) = runtime_with_mock();
    let (_notifier, stop_rx) = stop_channel();
    let ctx = ServiceContext::new(rt, "myapp.sync".to_owned(), stop_rx, CancelToken::new());

    pollster::block_on(ctx.stop_self()).unwrap();
    let snap = mock.snapshot();
    assert_eq!(snap.stops, vec!["myapp.sync".to_owned()]);
}
