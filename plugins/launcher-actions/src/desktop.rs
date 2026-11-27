//! [`DesktopLauncherActions`]: the desktop backend.
//!
//! * Linux: the `com.canonical.Unity.LauncherEntry` D-Bus protocol
//!   (badge count, progress, urgent) read by KDE Plasma, Dash to Dock /
//!   Dash to Panel and Plank. Actions are static (`.desktop`
//!   `Actions=`, written by packaging from `[[app.launcher_action]]`).
//! * Windows: Jump List tasks, taskbar progress, `FlashWindowEx` and the
//!   shell's recent documents, on one COM thread of the plugin's own.
//! * macOS: the Dock tile badge, `requestUserAttention`, a Dock menu and
//!   `NSDocumentController`'s recent documents, on the main thread.
//!
//! Each platform object is built on first use and kept: a badge or
//! progress update reuses the D-Bus connection, the COM objects or the
//! Dock state instead of setting them up again.

use std::sync::{Arc, OnceLock, Weak};

use istmo_core::Runtime;

use crate::{
    Attention, Badge, Capabilities, LauncherAction, LauncherActions, LauncherError, PinOutcome,
    Progress,
};

#[cfg(any(target_os = "windows", test))]
mod ico;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
#[allow(unsafe_code, reason = "AppKit through objc2: Dock menu method, views")]
mod macos;
#[cfg(target_os = "windows")]
#[allow(unsafe_code, reason = "COM: Jump List, taskbar, shell")]
mod windows;

#[cfg(target_os = "linux")]
use linux::Platform;
#[cfg(target_os = "macos")]
use macos::Platform;
#[cfg(target_os = "windows")]
use windows::Platform;

/// Settings fixed before the first call.
#[derive(Debug, Clone, Default)]
pub(crate) struct Config {
    /// Linux desktop id (`<id>.desktop`), when the app sets it.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code, reason = "Linux only"))]
    pub(crate) desktop_id: Option<String>,
    /// Runtime the macOS Dock menu publishes deep links on.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code, reason = "macOS only"))]
    pub(crate) runtime: Option<Weak<Runtime>>,
}

impl Config {
    /// The runtime to publish on: the attached one, else the global one.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code, reason = "macOS only"))]
    pub(crate) fn runtime(&self) -> Option<Arc<Runtime>> {
        self.runtime
            .as_ref()
            .and_then(Weak::upgrade)
            .or_else(|| Runtime::global().ok())
    }
}

/// Desktop backend of [`LauncherActions`]; register it as the host:
///
/// ```ignore
/// istmo::runtime!(hosts: [LauncherActions => DesktopLauncherActions::default()
///     .with_desktop_id(option_env!("ISTMO_APP_BUNDLE_ID").unwrap_or("com.example.notes"))]);
/// ```
///
/// Cheap to clone; clones share the platform state.
#[derive(Debug, Clone, Default)]
pub struct DesktopLauncherActions {
    config: Config,
    #[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
    platform: Arc<OnceLock<Platform>>,
}

impl DesktopLauncherActions {
    /// The Linux desktop id (`<id>.desktop`, as installed) the
    /// `LauncherEntry` updates name. `FLATPAK_ID` wins inside Flatpak;
    /// without either, the executable's file name is used. Usually the
    /// app's `[app] id`, which `istmo-build` bakes as
    /// `option_env!("ISTMO_APP_BUNDLE_ID")` in the app crate.
    #[must_use]
    pub fn with_desktop_id(mut self, id: impl Into<String>) -> Self {
        self.config.desktop_id = Some(id.into());
        self
    }

    /// The runtime the macOS Dock menu publishes deep links on, instead
    /// of the global one.
    #[must_use]
    pub fn with_runtime(mut self, runtime: &Arc<Runtime>) -> Self {
        self.config.runtime = Some(Arc::downgrade(runtime));
        self
    }

    #[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
    fn platform(&self) -> &Platform {
        self.platform.get_or_init(|| Platform::new(&self.config))
    }
}

#[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
impl LauncherActions for DesktopLauncherActions {
    async fn capabilities(&self) -> Capabilities {
        Platform::capabilities()
    }

    async fn set_actions(&self, actions: Vec<LauncherAction>) -> Result<(), LauncherError> {
        LauncherAction::validate_set(&actions)?;
        let max = Platform::capabilities().max_actions;
        if max == 0 {
            return Err(LauncherError::Unsupported);
        }
        if actions.len() > max as usize {
            return Err(LauncherError::TooMany(max));
        }
        self.platform().set_actions(actions).await
    }

    async fn clear_actions(&self) -> Result<(), LauncherError> {
        if !Platform::capabilities().dynamic_actions {
            return Err(LauncherError::Unsupported);
        }
        self.platform().set_actions(Vec::new()).await
    }

    async fn report_used(&self, _id: String) -> Result<(), LauncherError> {
        // Only Android ranks shortcuts by use; elsewhere a no-op.
        Ok(())
    }

    async fn request_pin(&self, action: LauncherAction) -> Result<PinOutcome, LauncherError> {
        action.validate()?;
        Err(LauncherError::Unsupported)
    }

    async fn set_badge(&self, badge: Badge) -> Result<(), LauncherError> {
        self.platform().set_badge(badge).await
    }

    async fn set_progress(&self, progress: Progress) -> Result<(), LauncherError> {
        self.platform().set_progress(progress).await
    }

    async fn request_attention(&self, attention: Attention) -> Result<(), LauncherError> {
        self.platform().request_attention(attention).await
    }

    async fn add_recent_document(&self, path: String) -> Result<(), LauncherError> {
        if path.is_empty() || path.contains('\0') {
            return Err(LauncherError::Backend("invalid document path".into()));
        }
        self.platform().add_recent_document(path).await
    }

    async fn clear_recent_documents(&self) -> Result<(), LauncherError> {
        self.platform().clear_recent_documents().await
    }
}

/// Other desktops (the BSDs, …): nothing is supported.
#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
impl LauncherActions for DesktopLauncherActions {
    async fn capabilities(&self) -> Capabilities {
        Capabilities::default()
    }

    async fn set_actions(&self, actions: Vec<LauncherAction>) -> Result<(), LauncherError> {
        LauncherAction::validate_set(&actions)?;
        Err(LauncherError::Unsupported)
    }

    async fn clear_actions(&self) -> Result<(), LauncherError> {
        Err(LauncherError::Unsupported)
    }

    async fn report_used(&self, _id: String) -> Result<(), LauncherError> {
        Ok(())
    }

    async fn request_pin(&self, _action: LauncherAction) -> Result<PinOutcome, LauncherError> {
        Err(LauncherError::Unsupported)
    }

    async fn set_badge(&self, _badge: Badge) -> Result<(), LauncherError> {
        Err(LauncherError::Unsupported)
    }

    async fn set_progress(&self, _progress: Progress) -> Result<(), LauncherError> {
        Err(LauncherError::Unsupported)
    }

    async fn request_attention(&self, _attention: Attention) -> Result<(), LauncherError> {
        Err(LauncherError::Unsupported)
    }

    async fn add_recent_document(&self, _path: String) -> Result<(), LauncherError> {
        Err(LauncherError::Unsupported)
    }

    async fn clear_recent_documents(&self) -> Result<(), LauncherError> {
        Err(LauncherError::Unsupported)
    }
}
