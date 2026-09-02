//! Android demo cdylib — the frame protocol is the only crossing.
//!
//! The whole surface of this cdylib is a plugin trait, its impl, and the
//! `istmo::runtime!` invocation. Every Kotlin ↔ Rust call goes through the
//! five JNI symbols `runtime!` re-exports from `::istmo::android::entrypoint`.
//!
//! To exercise every M3 plugin end-to-end the `Echo` trait carries one
//! method per plugin: Rust receives the invocation on the hosted side,
//! consumes the corresponding client plugin (`PermissionsClient`,
//! `ActivityResultsClient`, `AppLifecycle`, `DeepLinks`) and reports back a
//! human-readable summary. That way the demo UI touches every direction of
//! the frame protocol through a single trait.

use istmo::plugins::{
    ActivityLaunchError, ActivityOutcome, ActivityResultsClient, AppLifecycle, DeepLinks,
    IntentRequest, PermissionsClient,
};
use istmo::{IstmoError, message, plugin};

#[message]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EchoError {
    pub reason: String,
}

fn as_echo_error(err: &IstmoError) -> EchoError {
    EchoError {
        reason: err.to_string(),
    }
}

fn launch_error_to_echo(err: &ActivityLaunchError) -> EchoError {
    EchoError {
        reason: format!("{err:?}"),
    }
}

#[plugin(name = "dev.istmo.demo.echo")]
pub trait Echo {
    async fn echo(&self, text: String) -> Result<String, EchoError>;

    async fn check_permission(&self, permission: String) -> Result<String, EchoError>;

    async fn request_permission(&self, permission: String) -> Result<String, EchoError>;

    async fn open_url(&self, url: String) -> Result<String, EchoError>;

    async fn lifecycle_snapshot(&self) -> Result<String, EchoError>;

    async fn drain_deeplinks(&self) -> Result<Vec<String>, EchoError>;
}

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

    async fn check_permission(&self, permission: String) -> Result<String, EchoError> {
        let plugin = PermissionsClient::acquire().map_err(|e| as_echo_error(&e))?;
        let status = plugin
            .check(permission.clone())
            .await
            .map_err(|e| as_echo_error(&e))?;
        Ok(format!("check({permission}) -> {status:?}"))
    }

    async fn request_permission(&self, permission: String) -> Result<String, EchoError> {
        let plugin = PermissionsClient::acquire().map_err(|e| as_echo_error(&e))?;
        let outcomes = plugin
            .request(vec![permission])
            .await
            .map_err(|e| as_echo_error(&e))?;
        let rendered = outcomes
            .into_iter()
            .map(|o| format!("{} -> {:?}", o.permission, o.status))
            .collect::<Vec<_>>()
            .join(", ");
        Ok(rendered)
    }

    async fn open_url(&self, url: String) -> Result<String, EchoError> {
        let plugin = ActivityResultsClient::acquire().map_err(|e| as_echo_error(&e))?;
        let request = IntentRequest {
            action: "android.intent.action.VIEW".to_owned(),
            uri: Some(url),
            component: None,
            mime_type: None,
            categories: vec![],
            extras: vec![],
        };
        let result = plugin.launch(request).await.map_err(|err| match err {
            IstmoError::PluginError { bytes } => istmo::codec::decode::<ActivityLaunchError>(&bytes)
                .map_or_else(
                    |_| EchoError {
                        reason: "undecodable launch error".to_owned(),
                    },
                    |(decoded, _)| launch_error_to_echo(&decoded),
                ),
            ref other => as_echo_error(other),
        })?;
        let data = result.data_uri.as_deref().unwrap_or("");
        let outcome = match result.outcome {
            ActivityOutcome::Ok => "Ok",
            ActivityOutcome::Cancelled => "Cancelled",
            ActivityOutcome::Custom(_) => "Custom",
        };
        Ok(format!("outcome={outcome} data={data}"))
    }

    async fn lifecycle_snapshot(&self) -> Result<String, EchoError> {
        let plugin = AppLifecycle::acquire().map_err(|e| as_echo_error(&e))?;
        let state = plugin.current().map_err(|e| as_echo_error(&e))?;
        Ok(state.map_or_else(
            || "(no lifecycle event observed yet)".to_owned(),
            |s| format!("{s:?}"),
        ))
    }

    async fn drain_deeplinks(&self) -> Result<Vec<String>, EchoError> {
        let plugin = DeepLinks::acquire().map_err(|e| as_echo_error(&e))?;
        let stream = plugin.stream();
        let mut collected = Vec::new();
        while let Some(next) = stream.try_recv() {
            let link = next.map_err(|e| as_echo_error(&e))?;
            collected.push(link.uri);
        }
        Ok(collected)
    }
}

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
