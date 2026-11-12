//! Pure-Rust desktop sampler for
//! [`CapabilitiesClient`](super::capabilities::CapabilitiesClient).
//!
//! Capability probing on desktop is uneven: Linux inspects `/sys` +
//! `/dev` entries, Windows probes services via Registry/APIs, macOS
//! would need `IOKit` (heavier dep). This sampler ships what the
//! stdlib + minimal FFI can resolve; everything else stays `false`
//! and the per-app observer / native bridge covers higher-fidelity
//! probes.

use super::capabilities::Capabilities;
use crate::early::LatestPublisher;

/// Publishes the capability flags the host can detect.
pub type CapabilitiesPublisher = LatestPublisher<Capabilities>;

impl LatestPublisher<Capabilities> {
    /// Capability flags rarely change at runtime — publish once at
    /// startup. Calls after the first successful publish are no-ops.
    pub fn publish(&mut self) {
        if !self.has_published() {
            self.publish_if_changed(sample());
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
            && fs::read_dir("/sys/class/bluetooth").is_ok_and(|mut it| it.next().is_some())
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
        // Hardware probe first — more reliable than daemon-install
        // path checks (distros vary: Fedora puts `fprintd` under
        // `/usr/libexec`, Arch under `/usr/lib`, NixOS under
        // `/nix/store/...`).
        if let Ok(entries) = fs::read_dir("/sys/class") {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy().to_ascii_lowercase();
                if name.contains("fingerprint") || name.contains("fprint") {
                    return true;
                }
            }
        }
        Path::new("/usr/libexec/fprintd").exists()
            || Path::new("/usr/lib/fprintd").exists()
            || Path::new("/run/current-system/sw/libexec/fprintd").exists()
    }
}
