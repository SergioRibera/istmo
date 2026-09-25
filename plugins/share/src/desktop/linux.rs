//! Linux: no system share sheet.
//!
//! freedesktop.org has no share portal, and neither GNOME nor KDE ship
//! a cross-desktop share UI (KDE's Purpose framework is KDE-only). The
//! backend therefore declines every share and reports no capabilities;
//! apps render their own targets (clipboard, `mailto:` via the `OpenURI`
//! portal, …) instead. Flutter's `share_plus` takes a similar stance:
//! it opens a `mailto:` link for text and rejects files.

use istmo_window::WindowRegistry;

use super::ShareJob;
use crate::{ShareCapabilities, ShareError, ShareOutcome};

pub(super) const CAPABILITIES: ShareCapabilities = ShareCapabilities {
    send: false,
    files: false,
    mixed_content: false,
    rich_preview: false,
    reports_completion: false,
    reports_target: false,
    receive: false,
};

pub(super) const UNSUPPORTED_REASON: &str =
    "Linux desktops have no standard share sheet; render an in-app share UI instead";

#[derive(Debug)]
pub(super) struct Backend;

impl Backend {
    #[allow(clippy::unnecessary_wraps)]
    pub(super) const fn attach(_registry: &WindowRegistry) -> Result<Self, ShareError> {
        Ok(Self)
    }

    #[allow(clippy::unused_async)]
    pub(super) async fn present(&self, _job: ShareJob) -> Result<ShareOutcome, ShareError> {
        Err(ShareError::Unsupported(UNSUPPORTED_REASON.to_owned()))
    }
}
