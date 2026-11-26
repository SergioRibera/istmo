//! Standing grants on Windows: only borderless capture
//! (`GraphicsCaptureAccess.RequestAccessAsync(Borderless)`, build
//! 20348+). Every other scope has nothing to grant here.
//!
//! The status is read without prompting through the
//! `graphicsCaptureWithoutBorder` app capability; the prompt is the OS's
//! own consent dialog and runs only from `request_access` (or a session
//! asking for `Borderless::Prefer` / `Require`), and only when the app
//! allowed it (`CapturePolicy::allow_borderless`).

use windows::Graphics::Capture::{GraphicsCaptureAccess, GraphicsCaptureAccessKind};
use windows::Security::Authorization::AppCapabilityAccess::{
    AppCapability, AppCapabilityAccessStatus,
};
use windows_core::HSTRING;

use crate::policy::CapturePolicy;
use crate::types::{
    AccessScope, AccessState, AccessStatus, BackendId, CaptureError, ConsentModel, IndicatorState,
    Persistence,
};

use super::os;

/// The capability a packaged app declares for borderless capture.
const CAPABILITY: &str = "graphicsCaptureWithoutBorder";

/// Whether this Windows knows borderless capture at all.
pub(crate) fn borderless_supported() -> bool {
    os::has_property(
        "Windows.Graphics.Capture.GraphicsCaptureSession",
        "IsBorderRequired",
    ) && os::has_type("Windows.Graphics.Capture.GraphicsCaptureAccess")
}

const fn status_of(status: AppCapabilityAccessStatus) -> AccessStatus {
    match status {
        AppCapabilityAccessStatus::Allowed => AccessStatus::Granted,
        AppCapabilityAccessStatus::DeniedByUser => AccessStatus::Denied,
        AppCapabilityAccessStatus::UserPromptRequired => AccessStatus::NotDetermined,
        // DeniedBySystem, NotDeclaredByApp, and anything newer.
        _ => AccessStatus::Restricted,
    }
}

/// The borderless grant now, without prompting.
pub(crate) fn borderless_status() -> AccessStatus {
    if !borderless_supported() {
        return AccessStatus::Restricted;
    }
    AppCapability::Create(&HSTRING::from(CAPABILITY))
        .and_then(|capability| capability.CheckAccess())
        .map_or(AccessStatus::NotDetermined, status_of)
}

/// Ask for the borderless grant: the OS prompt when it was never
/// answered. Blocks until the person answers.
pub(crate) fn request_borderless() -> Result<AccessStatus, CaptureError> {
    if !borderless_supported() {
        return Ok(AccessStatus::Restricted);
    }
    let status = GraphicsCaptureAccess::RequestAccessAsync(GraphicsCaptureAccessKind::Borderless)
        .and_then(|operation| operation.join())
        .map_err(|e| {
            CaptureError::os(
                BackendId::WindowsWgc,
                "GraphicsCaptureAccess.RequestAccessAsync",
                i64::from(e.code().0),
            )
        })?;
    Ok(status_of(status))
}

const fn state(scope: AccessScope, status: AccessStatus) -> AccessState {
    AccessState {
        scope,
        status,
        model: ConsentModel::SystemSetting,
        persistence: Persistence::SystemSetting,
        revocable_mid_session: false,
        // Without the grant the OS draws its yellow border.
        indicator: IndicatorState::Shown,
    }
}

/// `access(scope)` on Windows; `None` for scopes Windows has no grant
/// for (the host answers those itself).
pub(crate) fn current(scope: AccessScope) -> Option<AccessState> {
    (scope == AccessScope::BorderlessCapture).then(|| state(scope, borderless_status()))
}

/// `request_access(scope)` on Windows; `None` for scopes without a grant.
pub(crate) fn request(
    scope: AccessScope,
    policy: &CapturePolicy,
) -> Option<Result<AccessState, CaptureError>> {
    if scope != AccessScope::BorderlessCapture {
        return None;
    }
    if !policy.allow_borderless {
        return Some(Err(CaptureError::PolicyDenied(
            "allow_borderless is false".to_owned(),
        )));
    }
    Some(request_borderless().map(|status| state(scope, status)))
}
