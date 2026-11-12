use std::sync::Arc;

use istmo::{CallId, Envelope, Frame, IstmoError, Runtime};

pub mod api {
    #[istmo::plugin(name = "test.paths.echo")]
    pub trait Echo {
        async fn ping(&self, msg: String) -> String;
    }

    #[istmo::plugin(name = "test.paths.compute")]
    pub trait Compute {
        async fn crunch(&self, input: u32) -> u32;
    }

    #[istmo::plugin(name = "test.paths.store")]
    pub trait Store {
        async fn get(&self) -> u32;
    }

    #[istmo::plugin(name = "test.paths.absent")]
    pub trait Absent {
        async fn nope(&self) -> u32;
    }

    #[derive(Debug)]
    pub struct EchoImpl;

    impl Echo for EchoImpl {
        async fn ping(&self, msg: String) -> String {
            format!("echo:{msg}")
        }
    }
}

istmo::runtime!(
    plugins: [api::StoreClient, crate::api::EchoClient],
    remote: [api::Compute, crate::api::StoreClient],
    hosts: [api::Echo => api::EchoImpl],
);

fn wired() -> (Arc<Runtime>, flume::Receiver<Envelope>) {
    let init = __istmo_configure_runtime(Runtime::mock());
    (init.runtime, init.outbound)
}

#[test]
fn plugins_section_accepts_paths() {
    let (rt, _outbound) = wired();
    api::StoreClient::from_runtime(&rt).expect("declared through a relative path");
    api::EchoClient::from_runtime(&rt).expect("declared through a crate path");
    let err = api::AbsentClient::from_runtime(&rt).expect_err("undeclared");
    assert!(
        matches!(err, IstmoError::PluginNotDeclared("test.paths.absent")),
        "got {err:?}",
    );
}

#[test]
fn remote_section_accepts_trait_and_client_paths() {
    let (rt, _outbound) = wired();
    assert!(
        rt.is_remote_plugin("test.paths.compute"),
        "trait path gets `Client` appended"
    );
    assert!(
        rt.is_remote_plugin("test.paths.store"),
        "client path is used as-is"
    );
    assert!(!rt.is_remote_plugin("test.paths.echo"));
}

#[test]
fn hosts_section_accepts_trait_paths() {
    let (rt, outbound) = wired();
    let call_id = CallId(77);
    rt.dispatch_inbound(Envelope::new(Frame::Call {
        call_id,
        plugin_id: "test.paths.echo".to_owned(),
        instance_id: None,
        method: "ping".to_owned(),
        payload: istmo::codec::encode(&("hola".to_owned(),)).expect("encode"),
    }))
    .expect("dispatch");

    let envelope = outbound
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("respond");
    let Frame::Respond {
        call_id: cid,
        result: Ok(bytes),
    } = envelope.frame
    else {
        panic!("expected a successful Respond, got {:?}", envelope.frame);
    };
    assert_eq!(cid, call_id);
    let (reply, _) = istmo::codec::decode::<String>(&bytes).expect("decode");
    assert_eq!(reply, "echo:hola");
}
