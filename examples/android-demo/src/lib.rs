//! Android demo cdylib for istmo M2 end-to-end validation.
//!
//! Exposes a single native method to the demo `MainActivity`:
//! `Java_dev_istmo_demo_DemoBridge_callEcho` — sends an `echo` Call through the
//! istmo runtime, blocks the calling thread until the Kotlin backend responds,
//! and returns the decoded response string.

// Re-export the JNI trampolines from istmo-android so this cdylib exposes them
// to the JVM. Without the `pub use`, `--gc-sections` would strip the symbols
// out of the final `.so`.
pub use istmo_android::{
    Java_dev_istmo_runtime_IstmoRuntime_nativeShutdown,
    Java_dev_istmo_runtime_IstmoRuntime_nativeStart,
    Java_dev_istmo_runtime_IstmoRuntime_nativeSubmitEvent,
    Java_dev_istmo_runtime_IstmoRuntime_nativeSubmitResponse,
    Java_dev_istmo_runtime_IstmoRuntime_nativeSubmitStreamEnd,
};

use jni::JNIEnv;
use jni::objects::{JClass, JString};
use jni::sys::jstring;

#[istmo::plugin(name = "dev.istmo.demo.echo")]
trait Echo {
    async fn echo(&self, text: String) -> String;
}

/// Kotlin: `external fun callEcho(text: String): String` on
/// `dev.istmo.demo.DemoBridge`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_istmo_demo_DemoBridge_callEcho<'local>(
    mut env: JNIEnv<'local>,
    _caller: JClass<'local>,
    text: JString<'local>,
) -> jstring {
    let request: String = match env.get_string(&text) {
        Ok(s) => s.into(),
        Err(err) => {
            tracing::error!(?err, "callEcho: failed to read jstring input");
            return std::ptr::null_mut();
        }
    };

    let response = pollster::block_on(async {
        let echo = Echo::acquire()?;
        echo.echo(request).await
    });

    match response {
        Ok(text) => env
            .new_string(text)
            .map_or_else(|_| std::ptr::null_mut(), JString::into_raw),
        Err(err) => {
            tracing::error!(?err, "callEcho: echo call failed");
            std::ptr::null_mut()
        }
    }
}
