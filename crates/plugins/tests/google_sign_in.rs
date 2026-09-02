//! `SignIn` plugin: `acquire_with` round-trip, `NativeHandle` adoption and
//! drop-cascade against a mock native backend.

use std::sync::Arc;
use std::thread;

use istmo_core::{Envelope, Frame, InstanceId, NativeHandleId, Runtime, codec};
use istmo_plugins::{
    GOOGLE_SIGN_IN_PLUGIN_ID, SignInAccount, SignInClient, SignInConfig, SignInError, SignInMode,
};

fn cfg() -> SignInConfig {
    SignInConfig::builder("123456789-abc.apps.googleusercontent.com")
        .scope("https://www.googleapis.com/auth/drive.file")
        .hosted_domain("example.com")
        .nonce("random-nonce")
        .auto_select(false)
        .build()
}

fn sample_account(handle: u64) -> SignInAccount {
    SignInAccount {
        id: "1189998819991197253".to_owned(),
        email: Some("skinner@example.com".to_owned()),
        display_name: Some("Seymour Skinner".to_owned()),
        photo_url: None,
        id_token: "eyJhbGciOi...".to_owned(),
        granted_scopes: vec![
            "openid".to_owned(),
            "email".to_owned(),
            "profile".to_owned(),
        ],
        credential: NativeHandleId(handle),
    }
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
        // 1. CreateInstance
        let env = outbound.recv().expect("create envelope");
        let (create_call_id, config_bytes) = match env.frame {
            Frame::CreateInstance {
                call_id,
                plugin_id,
                payload,
            } => {
                assert_eq!(plugin_id, GOOGLE_SIGN_IN_PLUGIN_ID);
                (call_id, payload)
            }
            other => panic!("expected CreateInstance, got {other:?}"),
        };
        let (decoded_cfg, _) = codec::decode::<SignInConfig>(&config_bytes).unwrap();
        assert_eq!(decoded_cfg, cfg());

        let instance_id = InstanceId(4242);
        rt.dispatch_inbound(Envelope::new(Frame::Respond {
            call_id: create_call_id,
            result: Ok(codec::encode(&instance_id).unwrap()),
        }))
        .unwrap();

        // 2. First method call
        let env = outbound.recv().expect("call envelope");
        let call_call_id = match env.frame {
            Frame::Call {
                call_id,
                plugin_id,
                instance_id: Some(inst),
                method: m,
                ..
            } => {
                assert_eq!(plugin_id, GOOGLE_SIGN_IN_PLUGIN_ID);
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
fn acquire_with_ships_config_and_sign_in_returns_owned_credential() {
    let init = Runtime::mock();
    let rt = init.runtime.clone();
    let account_bytes = codec::encode(&sample_account(7)).unwrap();
    let backend = spawn_backend(rt.clone(), init.outbound, "sign_in", Ok(account_bytes));

    let client = pollster::block_on(SignInClient::from_runtime_with(&rt, cfg()))
        .expect("acquire_with succeeds");
    let owned = pollster::block_on(client.sign_in_owned(SignInMode::Interactive))
        .expect("sign_in_owned succeeds");
    assert_eq!(owned.id, "1189998819991197253");
    assert_eq!(owned.email.as_deref(), Some("skinner@example.com"));
    assert_eq!(owned.credential.id(), NativeHandleId(7));

    backend.join().unwrap();
    // Deliberately don't assert on the release/destroy frames here —
    // the outbound receiver has been consumed by the backend thread which
    // is now joined; the release frame emitted on drop would fail its send
    // silently. `dropping_owned_account_emits_release_native_handle_frame`
    // owns the outbound side and asserts the cascade end-to-end.
    drop(owned);
    drop(client);
}

#[test]
fn dropping_owned_account_emits_release_native_handle_frame() {
    let init = Runtime::mock();
    let rt = init.runtime.clone();
    let outbound = init.outbound;

    // Manually adopt a NativeHandleId into an owned account shape by
    // exercising `sign_in_owned` end-to-end. The backend feeds an account
    // with credential id 314; the owned account is then dropped and the
    // outbound channel receives a ReleaseNativeHandle frame.
    let account_bytes = codec::encode(&sample_account(314)).unwrap();

    let backend_rt = rt.clone();
    let backend_outbound = outbound.clone();
    let backend = thread::spawn(move || {
        let env = backend_outbound.recv().expect("create");
        let create_id = match env.frame {
            Frame::CreateInstance { call_id, .. } => call_id,
            other => panic!("expected CreateInstance, got {other:?}"),
        };
        let instance_id = InstanceId(1);
        backend_rt
            .dispatch_inbound(Envelope::new(Frame::Respond {
                call_id: create_id,
                result: Ok(codec::encode(&instance_id).unwrap()),
            }))
            .unwrap();

        let env = backend_outbound.recv().expect("call");
        let call_id = match env.frame {
            Frame::Call { call_id, .. } => call_id,
            other => panic!("expected Call, got {other:?}"),
        };
        backend_rt
            .dispatch_inbound(Envelope::new(Frame::Respond {
                call_id,
                result: Ok(account_bytes),
            }))
            .unwrap();
    });

    let client = pollster::block_on(SignInClient::from_runtime_with(&rt, cfg())).unwrap();
    let owned = pollster::block_on(client.sign_in_owned(SignInMode::Interactive)).unwrap();
    backend.join().unwrap();

    drop(owned);
    let release = outbound.recv().expect("release frame");
    match release.frame {
        Frame::ReleaseNativeHandle { handle_id } => {
            assert_eq!(handle_id, NativeHandleId(314));
        }
        other => panic!("expected ReleaseNativeHandle, got {other:?}"),
    }

    // And when the client itself drops, a DestroyInstance frame follows.
    drop(client);
    let destroy = outbound.recv().expect("destroy frame");
    assert!(matches!(destroy.frame, Frame::DestroyInstance { .. }));
}

#[test]
fn silent_sign_in_returns_none_when_no_credential_available() {
    let init = Runtime::mock();
    let rt = init.runtime.clone();
    let none: Option<SignInAccount> = None;
    let backend = spawn_backend(
        rt.clone(),
        init.outbound,
        "silent_sign_in",
        Ok(codec::encode(&none).unwrap()),
    );

    let client = pollster::block_on(SignInClient::from_runtime_with(&rt, cfg())).unwrap();
    let maybe = pollster::block_on(client.silent_sign_in_owned()).unwrap();
    assert!(maybe.is_none());
    backend.join().unwrap();
}

#[test]
fn domain_error_surfaces_as_plugin_error_bytes() {
    let init = Runtime::mock();
    let rt = init.runtime.clone();
    let err_bytes = codec::encode(&SignInError::UserCancelled).unwrap();
    let backend = spawn_backend(rt.clone(), init.outbound, "sign_in", Err(err_bytes));

    let client = pollster::block_on(SignInClient::from_runtime_with(&rt, cfg())).unwrap();
    let err = pollster::block_on(client.sign_in(SignInMode::Interactive))
        .expect_err("domain error surfaces");
    let bytes = match err {
        istmo_core::IstmoError::PluginError { bytes } => bytes,
        other => panic!("expected PluginError, got {other:?}"),
    };
    let (decoded, _) = codec::decode::<SignInError>(&bytes).unwrap();
    assert_eq!(decoded, SignInError::UserCancelled);
    backend.join().unwrap();
}
