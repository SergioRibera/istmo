//! The `Unavailable*` hosts answer every call with their typed error,
//! so clients on platforms without a backend never wait.

use istmo_core::{IstmoError, NativeHandleId, Runtime, codec};
use istmo_plugins::{
    ActivityLaunchError, ActivityResultsClient, ActivityResultsHost, AdError, AdMobClient,
    AdMobConfig, AdMobHost, IntentRequest, ServiceControlClient, ServiceControlError,
    ServiceControlHost, TaskHandle, TaskSchedulerClient, TaskSchedulerError, TaskSchedulerHost,
    UnavailableActivityResults, UnavailableAdMob, UnavailableServiceControl,
    UnavailableTaskScheduler,
};

fn domain<E: codec::Decode>(err: IstmoError) -> E {
    match err {
        IstmoError::PluginError { bytes } => codec::decode::<E>(&bytes).expect("typed error").0,
        other => panic!("expected a domain error, got {other:?}"),
    }
}

#[test]
fn admob_calls_fail_without_waiting() {
    let rt = Runtime::mock()
        .host(AdMobHost::new(UnavailableAdMob))
        .runtime;
    let client = pollster::block_on(AdMobClient::from_runtime_with(
        &rt,
        AdMobConfig {
            app_id: "app".to_owned(),
            test_device_ids: Vec::new(),
            child_directed_treatment: false,
        },
    ))
    .expect("instance");
    let err = pollster::block_on(client.load_interstitial("unit".to_owned())).unwrap_err();
    assert!(matches!(domain::<AdError>(err), AdError::Internal(_)));
    let err = pollster::block_on(client.hide_banner(NativeHandleId(1))).unwrap_err();
    assert!(matches!(domain::<AdError>(err), AdError::Internal(_)));
}

#[test]
fn activity_launches_find_no_activity() {
    let rt = Runtime::mock()
        .host(ActivityResultsHost::new(UnavailableActivityResults))
        .runtime;
    let client = ActivityResultsClient::from_runtime(&rt).expect("declared");
    let err = pollster::block_on(client.launch(IntentRequest {
        action: "android.intent.action.VIEW".to_owned(),
        uri: None,
        component: None,
        mime_type: None,
        categories: Vec::new(),
        extras: Vec::new(),
    }))
    .unwrap_err();
    assert_eq!(
        domain::<ActivityLaunchError>(err),
        ActivityLaunchError::NoActivityFound
    );
}

#[test]
fn task_scheduling_reports_the_missing_platform() {
    let rt = Runtime::mock()
        .host(TaskSchedulerHost::new(UnavailableTaskScheduler))
        .runtime;
    let client = TaskSchedulerClient::from_runtime(&rt).expect("declared");
    let err = pollster::block_on(client.cancel(TaskHandle {
        id: "task".to_owned(),
    }))
    .unwrap_err();
    assert!(matches!(
        domain::<TaskSchedulerError>(err),
        TaskSchedulerError::Platform(_)
    ));
}

#[test]
fn service_control_reports_the_missing_platform() {
    let rt = Runtime::mock()
        .host(ServiceControlHost::new(UnavailableServiceControl))
        .runtime;
    let client = ServiceControlClient::from_runtime(&rt).expect("declared");
    let err = pollster::block_on(client.stop_self("svc".to_owned())).unwrap_err();
    assert!(matches!(
        domain::<ServiceControlError>(err),
        ServiceControlError::Platform(_)
    ));
}
