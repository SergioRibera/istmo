//! Demo of the `activity_results` core plugin using the settled mock pattern.
//!
//! Run with `cargo run --example activity_results`.

use istmo::Runtime;
use istmo::plugins::{
    ActivityLaunchError, ActivityOutcome, ActivityResult, ActivityResults, ActivityResultsClient,
    ActivityResultsHost, ExtraValue, IntentRequest,
};

#[derive(Debug, Default)]
pub struct MockActivityResults;

impl ActivityResults for MockActivityResults {
    async fn launch(&self, request: IntentRequest) -> Result<ActivityResult, ActivityLaunchError> {
        match request.action.as_str() {
            "android.intent.action.VIEW" => Ok(ActivityResult {
                outcome: ActivityOutcome::Ok,
                data_uri: request.uri,
                extras: vec![],
            }),
            other => Err(ActivityLaunchError::Platform(format!(
                "no mock handler for `{other}`"
            ))),
        }
    }
}

fn main() {
    let init = Runtime::mock()
        .expects::<ActivityResultsClient>()
        .host(ActivityResultsHost::new(MockActivityResults))
        .finish();

    let plugin = ActivityResultsClient::from_runtime(&init.runtime).expect("declared");

    let view = IntentRequest {
        action: "android.intent.action.VIEW".to_owned(),
        uri: Some("https://example.com".to_owned()),
        component: None,
        mime_type: None,
        categories: vec![],
        extras: vec![("ref".to_owned(), ExtraValue::Text("demo".to_owned()))],
    };
    match pollster::block_on(plugin.launch(view)) {
        Ok(result) => println!("view result: {result:?}"),
        Err(err) => println!("view error: {err:?}"),
    }

    let share = IntentRequest {
        action: "android.intent.action.SEND".to_owned(),
        uri: None,
        component: None,
        mime_type: Some("text/plain".to_owned()),
        categories: vec![],
        extras: vec![("subject".to_owned(), ExtraValue::Text("hi".to_owned()))],
    };
    match pollster::block_on(plugin.launch(share)) {
        Ok(result) => println!("share result: {result:?}"),
        Err(istmo::IstmoError::PluginError { bytes }) => {
            let (decoded, _) =
                istmo::codec::decode::<ActivityLaunchError>(&bytes).expect("decode launch err");
            println!("share error: {decoded:?}");
        }
        Err(other) => println!("share transport error: {other}"),
    }
}
