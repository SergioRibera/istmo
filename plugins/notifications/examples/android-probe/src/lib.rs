//! On-device checks of `istmo-notifications`' Android backend. The
//! Kotlin `ProbeActivity` calls `Probe.run` through JNI with a command
//! (`caps`, `show`, `progress`, ...) and shows the report it returns;
//! every report line and every response also goes to logcat under the
//! `notif-probe` tag.

#![cfg(target_os = "android")]

mod probe;

use istmo_notifications::NotificationsClient;
use jni::JNIEnv;
use jni::objects::{JClass, JString};
use jni::sys::jstring;

istmo::runtime!(
    plugins: [NotificationsClient],
);

/// `dev.istmo.notificationsprobe.Probe.run(command)`: blocks the calling
/// (Kotlin background) thread until the command finished.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_istmo_notificationsprobe_Probe_run<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    command: JString<'local>,
) -> jstring {
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Info)
            .with_tag("notif-probe"),
    );
    let command: String = env.get_string(&command).map(Into::into).unwrap_or_default();
    let report = probe::run(&command);
    for line in report.lines() {
        log::info!("{line}");
    }
    env.new_string(report)
        .map_or(std::ptr::null_mut(), JString::into_raw)
}
