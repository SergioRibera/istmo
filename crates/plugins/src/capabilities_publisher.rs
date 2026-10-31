//! Pure-Rust desktop sampler for
//! [`super::capabilities::CapabilitiesClient`].
//!
//! Capability probing on desktop is uneven: Linux inspects `/sys` +
//! `/dev` entries, Windows probes services via Registry/APIs, macOS
//! would need IOKit (heavier dep). This sampler ships what the
//! stdlib + minimal FFI can resolve; everything else stays `false`
//! and the per-app observer / native bridge covers higher-fidelity
//! probes.

use std::sync::{Arc, Weak};

use istmo_core::{IstmoError, Runtime, codec};

use super::capabilities::{CAPABILITIES_CHANNEL, CAPABILITIES_PLUGIN_ID, Capabilities};

#[derive(Debug)]
pub struct CapabilitiesPublisher {
    runtime: Weak<Runtime>,
    emitted: bool,
}

impl CapabilitiesPublisher {
    pub fn install(runtime: &Arc<Runtime>) -> Result<Self, IstmoError> {
        runtime.check_declared(CAPABILITIES_PLUGIN_ID)?;
        Ok(Self {
            runtime: Arc::downgrade(runtime),
            emitted: false,
        })
    }

    /// Capability flags rarely change at runtime — publish once at
    /// startup. Subsequent calls are no-ops.
    pub fn publish(&mut self) {
        if self.emitted {
            return;
        }
        let Some(runtime) = self.runtime.upgrade() else {
            return;
        };
        let caps = sample();
        match codec::encode(&caps) {
            Ok(bytes) => {
                runtime.publish_early_latest(CAPABILITIES_CHANNEL, bytes);
                self.emitted = true;
            }
            Err(err) => tracing::warn!(target = "istmo.capabilities", ?err, "encode failed"),
        }
    }
}

fn sample() -> Capabilities {
    #[cfg(target_os = "linux")]
    {
        linux::sample()
    }
    #[cfg(not(target_os = "linux"))]
    {
        Capabilities::default()
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::fs;
    use std::path::Path;

    use super::Capabilities;

    pub(super) fn sample() -> Capabilities {
        Capabilities {
            has_biometric: false,
            has_face_id: false,
            has_fingerprint: has_fingerprint_device(),
            has_nfc: false,
            has_haptics: false,
            has_camera_front: false,
            has_camera_back: has_any_video_device(),
            has_gps: false,
            has_bluetooth: has_bluetooth_device(),
            has_microphone: has_microphone_device(),
            has_telephony: false,
        }
    }

    fn has_any_video_device() -> bool {
        if let Ok(entries) = fs::read_dir("/dev") {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with("video") && name[5..].parse::<u32>().is_ok() {
                    return true;
                }
            }
        }
        false
    }

    fn has_bluetooth_device() -> bool {
        Path::new("/sys/class/bluetooth").exists()
            && fs::read_dir("/sys/class/bluetooth")
                .is_ok_and(|mut it| it.next().is_some())
    }

    fn has_microphone_device() -> bool {
        // Minimal check — any ALSA capture device.
        if let Ok(entries) = fs::read_dir("/proc/asound") {
            for entry in entries.flatten() {
                let name = entry.file_name();
                if name.to_string_lossy().starts_with("card") {
                    return true;
                }
            }
        }
        false
    }

    fn has_fingerprint_device() -> bool {
        // `fprintd` typical install path.
        Path::new("/usr/libexec/fprintd").exists()
            || Path::new("/usr/lib/fprintd").exists()
    }
}
