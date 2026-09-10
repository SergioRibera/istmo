//! `DataStore` plugin: `acquire_with` round-trip against a mock native
//! backend, plus domain-error surface check.

use std::sync::Arc;
use std::thread;

use istmo_core::{Envelope, Frame, InstanceId, Runtime, codec};
use istmo_data_store::{
    DATA_STORE_PLUGIN_ID, DataStoreClient, DataStoreConfig, DataStoreError,
};

fn cfg() -> DataStoreConfig {
    DataStoreConfig::new("app_prefs")
}

/// Backend thread: services `CreateInstance` + one `Call` for `method`,
/// answering with `respond`.
fn spawn_backend(
    rt: Arc<Runtime>,
    outbound: flume::Receiver<Envelope>,
    method: &'static str,
    respond: Result<Vec<u8>, Vec<u8>>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let env = outbound.recv().expect("create envelope");
        let (create_call_id, config_bytes) = match env.frame {
            Frame::CreateInstance { call_id, plugin_id, payload } => {
                assert_eq!(plugin_id, DATA_STORE_PLUGIN_ID);
                (call_id, payload)
            }
            other => panic!("expected CreateInstance, got {other:?}"),
        };
        let (decoded_cfg, _) = codec::decode::<DataStoreConfig>(&config_bytes).unwrap();
        assert_eq!(decoded_cfg, cfg());

        let instance_id = InstanceId(4242);
        rt.dispatch_inbound(Envelope::new(Frame::Respond {
            call_id: create_call_id,
            result: Ok(codec::encode(&instance_id).unwrap()),
        }))
        .unwrap();

        let env = outbound.recv().expect("call envelope");
        let call_call_id = match env.frame {
            Frame::Call {
                call_id,
                plugin_id,
                instance_id: Some(inst),
                method: m,
                ..
            } => {
                assert_eq!(plugin_id, DATA_STORE_PLUGIN_ID);
                assert_eq!(inst, instance_id);
                assert_eq!(m, method);
                call_id
            }
            other => panic!("expected Call, got {other:?}"),
        };
        rt.dispatch_inbound(Envelope::new(Frame::Respond {
            call_id: call_call_id,
            result: respond,
        }))
        .unwrap();
    })
}

#[test]
fn set_string_round_trips_through_backend() {
    let init = Runtime::mock();
    let rt = init.runtime.clone();
    let ok_bytes = codec::encode(&()).unwrap();
    let backend = spawn_backend(rt.clone(), init.outbound, "set_string", Ok(ok_bytes));

    let client =
        pollster::block_on(DataStoreClient::from_runtime_with(&rt, cfg())).expect("acquire");
    pollster::block_on(client.set_string("user_id".to_owned(), "u_42".to_owned()))
        .expect("set succeeds");

    backend.join().unwrap();
    drop(client);
}

#[test]
fn get_string_returns_optional_value() {
    let init = Runtime::mock();
    let rt = init.runtime.clone();
    let present: Option<String> = Some("u_42".to_owned());
    let backend = spawn_backend(
        rt.clone(),
        init.outbound,
        "get_string",
        Ok(codec::encode(&present).unwrap()),
    );

    let client =
        pollster::block_on(DataStoreClient::from_runtime_with(&rt, cfg())).expect("acquire");
    let value = pollster::block_on(client.get_string("user_id".to_owned())).expect("get");
    assert_eq!(value.as_deref(), Some("u_42"));

    backend.join().unwrap();
}

#[test]
fn missing_key_surfaces_as_none() {
    let init = Runtime::mock();
    let rt = init.runtime.clone();
    let absent: Option<String> = None;
    let backend = spawn_backend(
        rt.clone(),
        init.outbound,
        "get_string",
        Ok(codec::encode(&absent).unwrap()),
    );

    let client =
        pollster::block_on(DataStoreClient::from_runtime_with(&rt, cfg())).expect("acquire");
    let value = pollster::block_on(client.get_string("missing".to_owned())).expect("get");
    assert!(value.is_none());

    backend.join().unwrap();
}

#[test]
fn backend_error_surfaces_as_plugin_error_bytes() {
    let init = Runtime::mock();
    let rt = init.runtime.clone();
    let err = DataStoreError::Backend("disk full".to_owned());
    let backend = spawn_backend(
        rt.clone(),
        init.outbound,
        "set_bytes",
        Err(codec::encode(&err).unwrap()),
    );

    let client =
        pollster::block_on(DataStoreClient::from_runtime_with(&rt, cfg())).expect("acquire");
    let result =
        pollster::block_on(client.set_bytes("blob".to_owned(), vec![0xDE, 0xAD, 0xBE, 0xEF]));
    let bytes = match result.expect_err("domain error surfaces") {
        istmo_core::IstmoError::PluginError { bytes } => bytes,
        other => panic!("expected PluginError, got {other:?}"),
    };
    let (decoded, _) = codec::decode::<DataStoreError>(&bytes).unwrap();
    assert_eq!(decoded, DataStoreError::Backend("disk full".to_owned()));

    backend.join().unwrap();
}
