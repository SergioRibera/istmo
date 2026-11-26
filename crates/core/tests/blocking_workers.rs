//! [`DispatchMode::Isolated`] hosts that block on each other.

use std::sync::{Arc, OnceLock, Weak};
use std::time::Duration;

use istmo_core::{
    CancelToken, Dispatch, DispatchFuture, DispatchMode, InstanceId, Outcome, Runtime,
};

/// `down(n)` blocks on its own `down(n - 1)` before answering.
#[derive(Default)]
struct Chain {
    runtime: OnceLock<Weak<Runtime>>,
}

impl Dispatch for Chain {
    fn plugin_id(&self) -> &'static str {
        "test.chain"
    }

    fn mode(&self) -> DispatchMode {
        DispatchMode::Isolated
    }

    fn runtime_attached(&self, runtime: Weak<Runtime>) {
        let _ = self.runtime.set(runtime);
    }

    fn dispatch<'a>(
        &'a self,
        _instance: Option<InstanceId>,
        _method: &'a str,
        payload: &'a [u8],
        _cancel: CancelToken,
    ) -> DispatchFuture<'a> {
        let depth = u32::from_le_bytes(payload.try_into().expect("depth"));
        let runtime = self.runtime.get().and_then(Weak::upgrade).expect("runtime");
        Box::pin(async move {
            if depth > 0 {
                let below = runtime
                    .call(
                        "test.chain",
                        None,
                        "down",
                        (depth - 1).to_le_bytes().to_vec(),
                    )
                    .expect("call")
                    .recv_blocking()
                    .expect("answer");
                assert_eq!(below, Ok(Vec::new()));
            }
            Ok(Outcome::Ok(Vec::new()))
        })
    }
}

#[test]
fn blocking_hosts_nested_far_deeper_than_the_core_count_finish() {
    let init = Runtime::mock();
    init.runtime.register_host(Chain::default());
    let runtime = Arc::clone(&init.runtime);
    let (tx, rx) = flume::bounded(1);
    std::thread::spawn(move || {
        let answer = runtime
            .call("test.chain", None, "down", 150u32.to_le_bytes().to_vec())
            .expect("call")
            .recv_blocking();
        let _ = tx.send(answer);
    });
    let answer = rx
        .recv_timeout(Duration::from_secs(60))
        .expect("150 blocking hosts, each waiting on the next, all finish");
    assert_eq!(answer.expect("recv"), Ok(Vec::new()));
}
