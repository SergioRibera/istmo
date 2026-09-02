//! End-to-end verification of `#[istmo::worker]` and the `workers:` section
//! of `istmo::runtime!`.

use std::sync::{Arc, Mutex};

use istmo::plugins::{TaskOutcome, WorkerContext};
use istmo::{Envelope, Frame, Runtime, codec};

#[istmo::worker(name = "myapp.backup")]
pub trait BackupTask {
    async fn run(&self, ctx: WorkerContext) -> Result<TaskOutcome, BackupError>;
}

#[istmo::message]
#[derive(Debug, PartialEq, Eq)]
pub struct BackupError {
    pub reason: String,
}

#[derive(Debug, Clone)]
struct BackupImpl {
    inputs: Arc<Mutex<Vec<Vec<u8>>>>,
    outcome: TaskOutcome,
}

impl BackupTask for BackupImpl {
    async fn run(&self, ctx: WorkerContext) -> Result<TaskOutcome, BackupError> {
        self.inputs.lock().unwrap().push(ctx.input().to_vec());
        Ok(self.outcome)
    }
}

#[test]
fn worker_adapter_runs_and_encodes_outcome() {
    let backup = BackupImpl {
        inputs: Arc::new(Mutex::new(Vec::new())),
        outcome: TaskOutcome::Retry,
    };
    let adapter = BackupTaskWorkerAdapter::new(backup.clone());
    let init = Runtime::mock().host(adapter).finish();
    let rt = init.runtime;
    let outbound = init.outbound;

    let call_id = istmo::CallId(1);
    let payload = codec::encode(&(
        "myapp.backup".to_owned(),
        "nightly".to_owned(),
        b"hello".to_vec(),
    ))
    .unwrap();
    rt.dispatch_inbound(Envelope::new(Frame::Call {
        call_id,
        plugin_id: "myapp.backup".to_owned(),
        instance_id: None,
        method: "run".to_owned(),
        payload,
    }))
    .unwrap();

    // Adapter dispatches via `std::thread::spawn` — wait for outbound respond.
    let envelope = outbound.recv().expect("respond");
    let Frame::Respond {
        call_id: rid,
        result,
    } = envelope.frame
    else {
        panic!("expected respond, got {:?}", envelope.frame);
    };
    assert_eq!(rid, call_id);
    let bytes = result.expect("ok");
    let (outcome, _) = codec::decode::<TaskOutcome>(&bytes).unwrap();
    assert_eq!(outcome, TaskOutcome::Retry);
    assert_eq!(
        backup.inputs.lock().unwrap().as_slice(),
        [b"hello".to_vec()]
    );
}
