//! `TaskScheduler` client + host wiring: enqueue / cancel / cancel-by-tag /
//! cancel-by-unique-name land in the same order on the mock backend.

use std::sync::{Arc, Mutex};

use istmo_core::Runtime;
use istmo_plugins::{
    Constraints, ExistingWorkPolicy, NetworkKind, TaskHandle, TaskRequest, TaskScheduler,
    TaskSchedulerClient, TaskSchedulerError, TaskSchedulerHost,
};

#[derive(Debug, Default, Clone)]
struct Log {
    enqueued: Vec<TaskRequest>,
    cancelled: Vec<TaskHandle>,
    cancelled_tag: Vec<String>,
    cancelled_unique: Vec<String>,
    next_id: u64,
}

#[derive(Debug, Default, Clone)]
struct MockScheduler {
    inner: Arc<Mutex<Log>>,
}

impl MockScheduler {
    fn snapshot(&self) -> Log {
        self.inner.lock().unwrap().clone()
    }
}

impl TaskScheduler for MockScheduler {
    async fn enqueue(&self, request: TaskRequest) -> Result<TaskHandle, TaskSchedulerError> {
        if request.task_id.is_empty() {
            return Err(TaskSchedulerError::UnknownTask(request.task_id));
        }
        let id = {
            let mut g = self.inner.lock().unwrap();
            g.next_id += 1;
            let id = format!("mock-{}", g.next_id);
            g.enqueued.push(request);
            id
        };
        Ok(TaskHandle { id })
    }

    async fn cancel(&self, handle: TaskHandle) -> Result<(), TaskSchedulerError> {
        self.inner.lock().unwrap().cancelled.push(handle);
        Ok(())
    }

    async fn cancel_by_tag(&self, tag: String) -> Result<(), TaskSchedulerError> {
        self.inner.lock().unwrap().cancelled_tag.push(tag);
        Ok(())
    }

    async fn cancel_by_unique_name(&self, name: String) -> Result<(), TaskSchedulerError> {
        self.inner.lock().unwrap().cancelled_unique.push(name);
        Ok(())
    }
}

fn runtime_with_mock() -> (Arc<Runtime>, MockScheduler) {
    let mock = MockScheduler::default();
    let init = Runtime::mock().host(TaskSchedulerHost::new(mock.clone()));
    (init.runtime, mock)
}

fn sample_request(task_id: &str, unique_name: &str) -> TaskRequest {
    TaskRequest {
        task_id: task_id.to_owned(),
        unique_name: unique_name.to_owned(),
        input: vec![1, 2, 3, 4],
        constraints: Constraints {
            required_network: NetworkKind::Unmetered,
            requires_charging: true,
            requires_device_idle: false,
            requires_battery_not_low: true,
            requires_storage_not_low: false,
        },
        initial_delay_seconds: Some(30),
        tags: vec!["sync".to_owned(), "premium".to_owned()],
        existing_work_policy: ExistingWorkPolicy::Replace,
    }
}

#[test]
fn enqueue_ships_typed_request_and_returns_platform_handle() {
    let (rt, mock) = runtime_with_mock();
    let client = TaskSchedulerClient::from_runtime(&rt).expect("declared");

    let handle = pollster::block_on(client.enqueue(sample_request("myapp.backup", "nightly")))
        .expect("enqueue Ok");
    assert_eq!(handle.id, "mock-1");

    let snap = mock.snapshot();
    assert_eq!(snap.enqueued.len(), 1);
    let request = &snap.enqueued[0];
    assert_eq!(request.task_id, "myapp.backup");
    assert_eq!(request.unique_name, "nightly");
    assert_eq!(request.input, vec![1, 2, 3, 4]);
    assert_eq!(request.constraints.required_network, NetworkKind::Unmetered);
    assert!(request.constraints.requires_charging);
    assert_eq!(request.initial_delay_seconds, Some(30));
    assert_eq!(request.tags, vec!["sync".to_owned(), "premium".to_owned()]);
    assert_eq!(request.existing_work_policy, ExistingWorkPolicy::Replace);
}

#[test]
fn enqueue_domain_error_round_trips_as_typed_variant() {
    let (rt, _mock) = runtime_with_mock();
    let client = TaskSchedulerClient::from_runtime(&rt).expect("declared");

    // Empty task_id triggers the mock's UnknownTask branch.
    let err = pollster::block_on(client.enqueue(sample_request("", "nightly")))
        .expect_err("empty task_id should fail");
    let bytes = match err {
        istmo_core::IstmoError::PluginError { bytes } => bytes,
        other => panic!("expected PluginError, got {other:?}"),
    };
    let (decoded, _) = istmo_core::codec::decode::<TaskSchedulerError>(&bytes).unwrap();
    assert_eq!(decoded, TaskSchedulerError::UnknownTask(String::new()));
}

#[test]
fn cancel_by_handle_forwards_the_platform_id() {
    let (rt, mock) = runtime_with_mock();
    let client = TaskSchedulerClient::from_runtime(&rt).expect("declared");

    pollster::block_on(async {
        client
            .cancel(TaskHandle {
                id: "work-1".to_owned(),
            })
            .await
            .unwrap();
    });

    let snap = mock.snapshot();
    assert_eq!(
        snap.cancelled,
        vec![TaskHandle {
            id: "work-1".to_owned()
        }],
    );
}

#[test]
fn cancel_by_tag_and_unique_name_reach_the_host_in_order() {
    let (rt, mock) = runtime_with_mock();
    let client = TaskSchedulerClient::from_runtime(&rt).expect("declared");

    pollster::block_on(async {
        client.cancel_by_tag("sync".to_owned()).await.unwrap();
        client
            .cancel_by_unique_name("nightly".to_owned())
            .await
            .unwrap();
        client.cancel_by_tag("premium".to_owned()).await.unwrap();
    });

    let snap = mock.snapshot();
    assert_eq!(snap.cancelled_tag, vec!["sync".to_owned(), "premium".to_owned()]);
    assert_eq!(snap.cancelled_unique, vec!["nightly".to_owned()]);
}

#[test]
fn task_request_round_trips_through_bincode() {
    let request = sample_request("myapp.backup", "nightly");
    let bytes = istmo_core::codec::encode(&request).unwrap();
    let (decoded, _) = istmo_core::codec::decode::<TaskRequest>(&bytes).unwrap();
    assert_eq!(decoded, request);
}
