//! Android demo cdylib — the frame protocol is the only crossing.
//!
//! The whole surface of this cdylib fits in this file: a plugin trait, an
//! implementation of the one plugin this process hosts, and the
//! `istmo::runtime!` invocation that wires everything up.
//!
//! Every Kotlin ↔ Rust call goes through the five JNI symbols that
//! `runtime!` re-exports from `::istmo::android::entrypoint`. No
//! `Java_dev_istmo_demo_*` symbol, no `no_mangle`, no `JNIEnv` — that is
//! the entire point of the redesign.

use istmo::plugins::{ActivityResultsClient, AppLifecycle, DeepLinks, PermissionsClient};
use istmo::{message, plugin};

#[message]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EchoError {
    pub reason: String,
}

#[plugin(name = "dev.istmo.demo.echo")]
pub trait Echo {
    /// Kotlin sees this as `suspend fun echo(text: String): String`.
    async fn echo(&self, text: String) -> Result<String, EchoError>;
}

/// Concrete implementation the demo hosts. Kotlin's `EchoClient` sends
/// `Frame::Call` frames that the runtime dispatches straight into this impl.
#[derive(Debug, Default)]
pub struct EchoImpl;

impl Echo for EchoImpl {
    async fn echo(&self, text: String) -> Result<String, EchoError> {
        if text.is_empty() {
            return Err(EchoError {
                reason: "empty input".to_owned(),
            });
        }
        Ok(format!("echo: {text}"))
    }
}

// The `runtime!` macro rewrites plugin names to `<Name>Client` under the hood;
// users import the `*Client` structs from `istmo::plugins` above so the
// generated code resolves them via absolute paths.
istmo::runtime!(
    plugins: [
        PermissionsClient,
        AppLifecycle,
        DeepLinks,
        ActivityResultsClient,
    ],
    hosts: [
        Echo => EchoImpl,
    ],
);
