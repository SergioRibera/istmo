//! Standing grants on Windows: only borderless capture
//! (`GraphicsCaptureAccess.RequestAccessAsync(Borderless)`, build
//! 20348+). Every other scope has nothing to grant here.
//!
//! The status is read without prompting through the
//! `graphicsCaptureWithoutBorder` app capability; the prompt is the OS's
//! own consent dialog and runs only from `request_access`, only when the
//! app allowed it (`CapturePolicy::allow_borderless`), on a thread of its
//! own bounded by [`PROMPT_WAIT`]: never on the caller's (possibly STA UI)
//! thread, and never implicitly from `open`.
//!
//! The capability must be declared in a package manifest: an unpackaged
//! app reads `NotDeclaredByApp`, reported as `Restricted` (and named in
//! the error of a session that requires borderless capture).

use std::time::Duration;

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
/// How long the consent prompt may wait for its answer.
pub(crate) const PROMPT_WAIT: Duration = Duration::from_secs(120);

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

/// The borderless grant as the OS reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Grant {
    pub(crate) status: AccessStatus,
    /// `NotDeclaredByApp`: no package declares the capability (an
    /// unpackaged app), so no prompt can grant it.
    pub(crate) undeclared: bool,
}

/// The borderless grant now, without prompting.
pub(crate) fn borderless_grant() -> Grant {
    if !borderless_supported() {
        return Grant {
            status: AccessStatus::Restricted,
            undeclared: false,
        };
    }
    let checked = AppCapability::Create(&HSTRING::from(CAPABILITY))
        .and_then(|capability| capability.CheckAccess());
    checked.map_or(
        Grant {
            status: AccessStatus::NotDetermined,
            undeclared: false,
        },
        |status| Grant {
            status: status_of(status),
            undeclared: status == AppCapabilityAccessStatus::NotDeclaredByApp,
        },
    )
}

/// The borderless grant now, without prompting.
pub(crate) fn borderless_status() -> AccessStatus {
    borderless_grant().status
}

/// Ask for the borderless grant: the OS prompt when it was never
/// answered. Runs on a thread of its own (the process is in the MTA) and
/// waits at most [`PROMPT_WAIT`] for the answer; an unpackaged app is
/// never prompted (nothing could grant it).
pub(crate) fn request_borderless() -> Result<AccessStatus, CaptureError> {
    let grant = borderless_grant();
    if !borderless_supported() || grant.undeclared {
        return Ok(AccessStatus::Restricted);
    }
    if grant.status != AccessStatus::NotDetermined {
        return Ok(grant.status);
    }
    let (tx, rx) = flume::bounded(1);
    std::thread::Builder::new()
        .name("istmo-screen-capture-consent".to_owned())
        .spawn(move || {
            let answer =
                GraphicsCaptureAccess::RequestAccessAsync(GraphicsCaptureAccessKind::Borderless)
                    .and_then(|operation| operation.join());
            let _sent = tx.send(answer);
        })
        .map_err(prompt_err)?;
    match rx.recv_timeout(PROMPT_WAIT) {
        Ok(Ok(status)) => Ok(status_of(status)),
        Ok(Err(e)) => Err(CaptureError::os(
            BackendId::WindowsWgc,
            "GraphicsCaptureAccess.RequestAccessAsync",
            i64::from(e.code().0),
        )),
        Err(_) => Err(prompt_err("the consent prompt got no answer in time")),
    }
}

fn prompt_err(message: impl std::fmt::Display) -> CaptureError {
    CaptureError::protocol(
        BackendId::WindowsWgc,
        "GraphicsCaptureAccess.RequestAccessAsync",
        message,
    )
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
