use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use istmo::plugins::ServiceContext;
use istmo::{Dispatch, Envelope, Frame, Outcome, Runtime, codec};

#[istmo::service(name = "myapp.sync")]
pub trait SyncService {
    async fn on_start(&self, ctx: ServiceContext) -> Result<(), SyncError>;
    async fn on_stop(&self);
}

#[istmo::message]
#[derive(Debug, PartialEq, Eq)]
pub struct SyncError {
    pub reason: String,
}

#[derive(Debug, Default, Clone)]
struct SyncImpl {
    ticks: Arc<AtomicUsize>,
    stops: Arc<AtomicUsize>,
    started_ids: Arc<Mutex<Vec<String>>>,
}

impl SyncService for SyncImpl {
    async fn on_start(&self, ctx: ServiceContext) -> Result<(), SyncError> {
        self.started_ids
            .lock()
            .unwrap()
            .push(ctx.service_id().to_owned());
        while !ctx.is_stopped() {
            self.ticks.fetch_add(1, Ordering::SeqCst);
            thread::sleep(Duration::from_millis(1));
        }
        Ok(())
    }

    async fn on_stop(&self) {
        self.stops.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn service_adapter_starts_and_stops_worker_thread() {
    let sync = SyncImpl::default();
    let adapter = SyncServiceAdapter::new(sync.clone());
    let init = Runtime::mock().host(adapter).finish();
    let rt = init.runtime;

    let call_id = istmo::CallId(1);
    let payload = codec::encode(&("service-a".to_owned(),)).unwrap();
    rt.dispatch_inbound(Envelope::new(Frame::Call {
        call_id,
        plugin_id: "myapp.sync".to_owned(),
        instance_id: None,
        method: "on_start".to_owned(),
        payload,
    }))
    .unwrap();

    let start = std::time::Instant::now();
    while sync.ticks.load(Ordering::SeqCst) == 0 {
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "service body never ran",
        );
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(sync.started_ids.lock().unwrap().as_slice(), ["service-a"]);

    let stop_payload = codec::encode(&()).unwrap();
    rt.dispatch_inbound(Envelope::new(Frame::Call {
        call_id: istmo::CallId(2),
        plugin_id: "myapp.sync".to_owned(),
        instance_id: None,
        method: "on_stop".to_owned(),
        payload: stop_payload,
    }))
    .unwrap();

    let start = std::time::Instant::now();
    while sync.stops.load(Ordering::SeqCst) == 0 {
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "on_stop never called",
        );
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(sync.stops.load(Ordering::SeqCst), 1);
}

#[test]
fn adapter_reports_stable_plugin_id() {
    let sync = SyncImpl::default();
    let adapter = SyncServiceAdapter::new(sync);
    assert_eq!(
        <SyncServiceAdapter<SyncImpl> as istmo::Plugin>::PLUGIN_ID,
        "myapp.sync"
    );
    assert_eq!(adapter.plugin_id(), "myapp.sync");
    let _ = Outcome::Ok(Vec::new());
}
