//! On-device checks of `istmo-screen-capture`'s Android backend. The
//! Kotlin `ProbeActivity` calls `Probe.run` through JNI with a command
//! (`caps`, `stream`, `shot`, ...) and shows the report it returns; each
//! report also goes to logcat under the `capture-probe` tag.

#![cfg(target_os = "android")]

mod probe;

use istmo_screen_capture::ScreenCaptureClient;
use jni::JNIEnv;
use jni::objects::{JClass, JString};
use jni::sys::{jlong, jstring};

istmo::runtime!(
    plugins: [ScreenCaptureClient],
);

/// `dev.istmo.screencaptureprobe.Probe.run(command, surface)`: blocks the
/// calling (Kotlin background) thread until the command finished.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_istmo_screencaptureprobe_Probe_run<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    command: JString<'local>,
    surface: jlong,
) -> jstring {
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Info)
            .with_tag("capture-probe"),
    );
    let command: String = env.get_string(&command).map(Into::into).unwrap_or_default();
    let report = probe::run(&command, surface);
    for line in report.lines() {
        log::info!("{line}");
    }
    env.new_string(report)
        .map_or(std::ptr::null_mut(), JString::into_raw)
}
