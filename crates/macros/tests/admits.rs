//! `#[istmo::plugin(remote = false)]` makes the generated hosts refuse
//! requests from other processes.

use istmo::{CallOrigin, Dispatch};

#[istmo::plugin(name = "test.admits.local", remote = false)]
pub trait LocalOnly {
    async fn ping(&self) -> u32;
}

#[istmo::plugin(name = "test.admits.open")]
pub trait Open {
    async fn ping(&self) -> u32;
}

#[istmo::message]
#[derive(Debug, Clone)]
pub struct Config {
    pub value: u32,
}

#[istmo::plugin(name = "test.admits.stateful", init = Config, remote = false)]
pub trait StatefulLocalOnly {
    async fn value(&self) -> u32;
}

struct Impl;

impl LocalOnly for Impl {
    async fn ping(&self) -> u32 {
        1
    }
}

impl Open for Impl {
    async fn ping(&self) -> u32 {
        2
    }
}

struct Instance(u32);

impl StatefulLocalOnly for Instance {
    async fn value(&self) -> u32 {
        self.0
    }
}

struct Factory;

impl StatefulLocalOnlyFactory for Factory {
    type Instance = Instance;

    fn create(&self, config: Config) -> Instance {
        Instance(config.value)
    }
}

#[test]
fn remote_false_refuses_only_remote_callers() {
    let host = LocalOnlyHost::new(Impl);
    assert!(host.admits(CallOrigin::Local));
    assert!(host.admits(CallOrigin::Peer));
    assert!(!host.admits(CallOrigin::Remote));

    let stateful = StatefulLocalOnlyHost::new(Factory);
    assert!(stateful.admits(CallOrigin::Peer));
    assert!(!stateful.admits(CallOrigin::Remote));
}

#[test]
fn hosts_admit_every_origin_by_default() {
    let host = OpenHost::new(Impl);
    assert!(host.admits(CallOrigin::Remote));
}
