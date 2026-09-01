//! Android demo cdylib exercising the istmo M2 transport and the M3 core
//! plugins (permissions, lifecycle, deeplinks, activity results) end-to-end
//! against real platform integrations on the Kotlin side.
//!
//! Every native method exported here is called from `dev.istmo.demo.DemoBridge`.

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

use istmo::plugins::{
    ActivityLaunchError, ActivityOutcome, ActivityResults, AppLifecycle, DeepLink, DeepLinkStream,
    DeepLinks, IntentRequest, LIFECYCLE_CHANNEL, LifecycleState, LifecycleStream,
    PermissionStatus, Permissions,
};
use istmo::{Runtime, codec};
use jni::JNIEnv;
use jni::objects::{JClass, JString};
use jni::sys::{jint, jstring};

#[istmo::plugin(name = "dev.istmo.demo.echo")]
trait Echo {
    async fn echo(&self, text: String) -> String;
}

/// Kotlin: `external fun callEcho(text: String): String`.
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

// ---- Lifecycle ---------------------------------------------------------------

/// Kotlin: `external fun pushLifecycle(state: Int)`.
///
/// `state` is the ordinal of [`LifecycleState`] (Kotlin side derives the same
/// order from the enum declaration).
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_istmo_demo_DemoBridge_pushLifecycle<'local>(
    _env: JNIEnv<'local>,
    _caller: JClass<'local>,
    state: jint,
) {
    let Some(state) = lifecycle_from_ordinal(state) else {
        tracing::warn!(state, "pushLifecycle: unknown ordinal");
        return;
    };
    let Ok(rt) = Runtime::global() else {
        return;
    };
    let Ok(bytes) = codec::encode(&state) else {
        return;
    };
    rt.publish_early_latest(LIFECYCLE_CHANNEL, bytes);
}

/// Kotlin: `external fun pollLifecycle(): Int` — returns the current lifecycle
/// state ordinal (or -1 if not yet observed).
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_istmo_demo_DemoBridge_pollLifecycle<'local>(
    _env: JNIEnv<'local>,
    _caller: JClass<'local>,
) -> jint {
    let Ok(rt) = Runtime::global() else {
        return -1;
    };
    let plugin = AppLifecycle::from_runtime(&rt);
    match plugin.current() {
        Ok(Some(state)) => lifecycle_ordinal(state),
        _ => -1,
    }
}

/// Kotlin: `external fun awaitLifecycleTransition(): Int` — blocks for the
/// next lifecycle transition and returns its ordinal. `-1` on error.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_istmo_demo_DemoBridge_awaitLifecycleTransition<'local>(
    _env: JNIEnv<'local>,
    _caller: JClass<'local>,
) -> jint {
    let Ok(rt) = Runtime::global() else {
        return -1;
    };
    let stream: LifecycleStream = AppLifecycle::from_runtime(&rt).stream();
    stream.recv().map_or(-1, lifecycle_ordinal)
}

const fn lifecycle_ordinal(state: LifecycleState) -> jint {
    match state {
        LifecycleState::Created => 0,
        LifecycleState::Started => 1,
        LifecycleState::Resumed => 2,
        LifecycleState::Paused => 3,
        LifecycleState::Stopped => 4,
        LifecycleState::Destroyed => 5,
        LifecycleState::LowMemory => 6,
        LifecycleState::ConfigurationChanged => 7,
    }
}

const fn lifecycle_from_ordinal(ordinal: jint) -> Option<LifecycleState> {
    match ordinal {
        0 => Some(LifecycleState::Created),
        1 => Some(LifecycleState::Started),
        2 => Some(LifecycleState::Resumed),
        3 => Some(LifecycleState::Paused),
        4 => Some(LifecycleState::Stopped),
        5 => Some(LifecycleState::Destroyed),
        6 => Some(LifecycleState::LowMemory),
        7 => Some(LifecycleState::ConfigurationChanged),
        _ => None,
    }
}

// ---- Deep links --------------------------------------------------------------

/// Kotlin: `external fun pushDeepLink(uri: String, source: String?)`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_istmo_demo_DemoBridge_pushDeepLink<'local>(
    mut env: JNIEnv<'local>,
    _caller: JClass<'local>,
    uri: JString<'local>,
    source: JString<'local>,
) {
    let Ok(uri): Result<String, _> = env.get_string(&uri).map(Into::into) else {
        return;
    };
    let source = if source.is_null() {
        None
    } else {
        env.get_string(&source).ok().map(Into::into)
    };
    let link = DeepLink {
        uri,
        source,
        received_at_ms: None,
    };
    let Ok(rt) = Runtime::global() else {
        return;
    };
    let Ok(bytes) = codec::encode(&link) else {
        return;
    };
    rt.publish_early_queue(istmo::plugins::DEEPLINKS_CHANNEL, 16, bytes);
}

// ---- Permissions -------------------------------------------------------------

/// Kotlin: `external fun callCheckPermission(permission: String): Int`.
///
/// Blocks until the native `istmo.permissions` handler replies. Returns the
/// [`PermissionStatus`] ordinal, or `-1` on transport failure.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_istmo_demo_DemoBridge_callCheckPermission<'local>(
    mut env: JNIEnv<'local>,
    _caller: JClass<'local>,
    permission: JString<'local>,
) -> jint {
    let Ok(permission): Result<String, _> = env.get_string(&permission).map(Into::into) else {
        return -1;
    };
    let Ok(rt) = Runtime::global() else {
        return -1;
    };
    let plugin = Permissions::from_runtime(&rt);
    match pollster::block_on(plugin.check(permission)) {
        Ok(status) => permission_status_ordinal(status),
        Err(err) => {
            tracing::error!(?err, "callCheckPermission failed");
            -1
        }
    }
}

/// Kotlin: `external fun callRequestPermission(permission: String): Int`.
///
/// Blocks until the user answers. Requests a single permission and returns
/// the resulting [`PermissionStatus`] ordinal (or `-1` on transport failure).
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_istmo_demo_DemoBridge_callRequestPermission<'local>(
    mut env: JNIEnv<'local>,
    _caller: JClass<'local>,
    permission: JString<'local>,
) -> jint {
    let Ok(permission): Result<String, _> = env.get_string(&permission).map(Into::into) else {
        return -1;
    };
    let Ok(rt) = Runtime::global() else {
        return -1;
    };
    let plugin = Permissions::from_runtime(&rt);
    match pollster::block_on(plugin.request(vec![permission])) {
        Ok(outcomes) => outcomes
            .first()
            .map_or(-1, |o| permission_status_ordinal(o.status)),
        Err(err) => {
            tracing::error!(?err, "callRequestPermission failed");
            -1
        }
    }
}

const fn permission_status_ordinal(status: PermissionStatus) -> jint {
    match status {
        PermissionStatus::Granted => 0,
        PermissionStatus::Denied => 1,
        PermissionStatus::PermanentlyDenied => 2,
        PermissionStatus::NotDetermined => 3,
        PermissionStatus::NotSupported => 4,
    }
}

// ---- Activity results --------------------------------------------------------

/// Kotlin: `external fun callLaunchIntent(action: String, uri: String?): String`.
///
/// Constructs a minimal [`IntentRequest`] (action + optional URI, no extras)
/// and awaits the [`ActivityResult`]. Returns a human-readable status line
/// suitable for the demo UI: `"OK: <data_uri>"`, `"CANCELLED"`,
/// `"CUSTOM: <code>"` or `"ERROR: <launch-error>"`. Transport failure
/// returns `"TRANSPORT_ERROR"`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_istmo_demo_DemoBridge_callLaunchIntent<'local>(
    mut env: JNIEnv<'local>,
    _caller: JClass<'local>,
    action: JString<'local>,
    uri: JString<'local>,
) -> jstring {
    let Ok(action): Result<String, _> = env.get_string(&action).map(Into::into) else {
        return std::ptr::null_mut();
    };
    let uri_opt = if uri.is_null() {
        None
    } else {
        env.get_string(&uri).ok().map(Into::into)
    };
    let request = IntentRequest {
        action,
        uri: uri_opt,
        component: None,
        mime_type: None,
        categories: vec![],
        extras: vec![],
    };
    let Ok(rt) = Runtime::global() else {
        return jstring_from(&env, "TRANSPORT_ERROR");
    };
    let plugin = ActivityResults::from_runtime(&rt);
    let text = match pollster::block_on(plugin.launch(request)) {
        Ok(result) => match result.outcome {
            ActivityOutcome::Ok => format!("OK: {}", result.data_uri.as_deref().unwrap_or("")),
            ActivityOutcome::Cancelled => "CANCELLED".to_owned(),
            ActivityOutcome::Custom(code) => format!("CUSTOM: {code}"),
        },
        Err(istmo::IstmoError::PluginError { bytes }) => codec::decode::<ActivityLaunchError>(&bytes)
            .map_or_else(
                |_| "ERROR: undecodable".to_owned(),
                |(err, _)| format!("ERROR: {err:?}"),
            ),
        Err(err) => {
            tracing::error!(?err, "callLaunchIntent transport failure");
            "TRANSPORT_ERROR".to_owned()
        }
    };
    jstring_from(&env, &text)
}

fn jstring_from(env: &JNIEnv<'_>, text: &str) -> jstring {
    env.new_string(text)
        .map_or_else(|_| std::ptr::null_mut(), JString::into_raw)
}


/// Kotlin: `external fun pollDeepLink(): String?` — non-blocking. Returns the
/// URI of the next queued/live link, or `null` if none is ready.
///
/// The stream is owned by a fresh [`DeepLinks`] client each call — the shared
/// [`istmo_core::early_events::PreMainQueue`] means the first attach drains
/// the pre-launch buffer once, then subsequent attaches only see live events.
/// For a real app you would hold onto one [`DeepLinkStream`] across calls; the
/// demo re-creates it each time for JNI simplicity.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_istmo_demo_DemoBridge_pollDeepLink<'local>(
    env: JNIEnv<'local>,
    _caller: JClass<'local>,
) -> jstring {
    let Ok(rt) = Runtime::global() else {
        return std::ptr::null_mut();
    };
    let stream: DeepLinkStream = DeepLinks::from_runtime(&rt).stream();
    match stream.try_recv() {
        Some(Ok(link)) => env
            .new_string(link.uri)
            .map_or_else(|_| std::ptr::null_mut(), JString::into_raw),
        _ => std::ptr::null_mut(),
    }
}
