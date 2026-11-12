mod api {
    #[istmo::plugin(name = "ui.paths.echo")]
    pub trait Echo {
        async fn echo(&self, text: String) -> String;
    }

    #[istmo::plugin(name = "ui.paths.compute")]
    pub trait Compute {
        async fn crunch(&self, input: u32) -> u32;
    }

    #[istmo::plugin(name = "ui.paths.store")]
    pub trait Store {
        async fn get(&self) -> u32;
    }

    pub struct Loopback;

    impl Echo for Loopback {
        async fn echo(&self, text: String) -> String {
            text
        }
    }
}

istmo::runtime!(
    plugins: [api::EchoClient, crate::api::StoreClient],
    remote: [api::Compute, api::StoreClient],
    hosts: [api::Echo => api::Loopback],
);

fn main() {
    let init = __istmo_configure_runtime(istmo::Runtime::mock());
    assert!(init.runtime.is_remote_plugin("ui.paths.compute"));
}
