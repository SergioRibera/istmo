//! `CapturePolicy`: the app's own rules. Not a wire type; no method
//! changes it.

use crate::types::BackendId;

/// Methods that stay same-process whatever the policy says: they move
/// sinks, grants or restore tokens, watch a session (`events`,
/// `audio_events`), or watch the sources (titles as they change,
/// `watch_sources`).
const ALWAYS_LOCAL: [&str; 10] = [
    "events",
    "watch_sources",
    "open_audio",
    "audio_events",
    "screenshot",
    "open",
    "open_with_audio",
    "open_preview_with_frames",
    "request_projection",
    "open_preview",
];

/// What the app allows. Built in Rust before the host is registered
/// (`DesktopCapture::with_policy`), frozen afterwards.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CapturePolicy {
    /// Methods opened to `CallOrigin::Remote` callers. Default none.
    /// Sink-taking methods, `open_preview` and `request_projection`
    /// stay same-process regardless.
    pub remote_methods: Vec<&'static str>,
    /// `None` = every compiled backend.
    pub allowed_backends: Option<Vec<BackendId>>,
    /// KMS capture (also needs the `linux-kms` feature).
    pub allow_kms: bool,
    /// Borderless WGC capture.
    pub allow_borderless: bool,
}

impl CapturePolicy {
    /// Whether a remote caller may call `method`.
    #[must_use]
    pub fn admits_remote(&self, method: &str) -> bool {
        !ALWAYS_LOCAL.contains(&method) && self.remote_methods.contains(&method)
    }

    /// Whether the app allows `backend` at all.
    #[must_use]
    pub fn allows(&self, backend: BackendId) -> bool {
        if backend == BackendId::LinuxKms && !self.allow_kms {
            return false;
        }
        self.allowed_backends
            .as_ref()
            .is_none_or(|allowed| allowed.contains(&backend))
    }

    /// Why `backend` is refused, for `Availability::PolicyDenied`.
    #[must_use]
    pub fn denial(&self, backend: BackendId) -> Option<String> {
        if self.allows(backend) {
            None
        } else if backend == BackendId::LinuxKms && !self.allow_kms {
            Some("allow_kms is false".to_owned())
        } else {
            Some("not in allowed_backends".to_owned())
        }
    }

    /// Open `method` to remote callers.
    #[must_use]
    pub fn with_remote_method(mut self, method: &'static str) -> Self {
        self.remote_methods.push(method);
        self
    }

    /// Restrict capture to these backends.
    #[must_use]
    pub fn with_allowed_backends(mut self, backends: Vec<BackendId>) -> Self {
        self.allowed_backends = Some(backends);
        self
    }

    #[must_use]
    pub const fn with_kms(mut self, allow: bool) -> Self {
        self.allow_kms = allow;
        self
    }

    #[must_use]
    pub const fn with_borderless(mut self, allow: bool) -> Self {
        self.allow_borderless = allow;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_callers_need_an_explicit_opening() {
        let policy = CapturePolicy::default();
        assert!(!policy.admits_remote("sources"));
        let policy = policy
            .with_remote_method("sources")
            .with_remote_method("open")
            .with_remote_method("watch_sources");
        assert!(policy.admits_remote("sources"));
        assert!(
            !policy.admits_remote("open"),
            "sink-taking methods stay local"
        );
        assert!(
            !policy.admits_remote("watch_sources"),
            "source watches stay local"
        );
    }

    #[test]
    fn kms_needs_its_own_switch() {
        let policy = CapturePolicy::default();
        assert!(!policy.allows(BackendId::LinuxKms));
        assert!(policy.allows(BackendId::LinuxX11));
        let policy = policy
            .with_kms(true)
            .with_allowed_backends(vec![BackendId::LinuxX11]);
        assert!(!policy.allows(BackendId::LinuxKms));
        assert_eq!(
            policy.denial(BackendId::LinuxKms).as_deref(),
            Some("not in allowed_backends")
        );
    }
}
