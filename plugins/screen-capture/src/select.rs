//! Backend selection (`Auto` / `Fixed` / `Ordered`), requirement checks
//! against the capability report, and format-tier negotiation with
//! in-band downgrades. Pure functions, shared by every host.

#![cfg_attr(
    not(target_os = "linux"),
    allow(
        dead_code,
        reason = "only the Linux backends negotiate tiers and cursors so far"
    )
)]

use std::sync::Once;

use crate::policy::CapturePolicy;
use crate::types::{
    AttemptFailure, AutoPreference, Availability, BackendAttempt, BackendCaps, BackendId,
    BackendReason, BackendSelection, BufferMemory, CaptureError, CompositeSupport, CopyStage,
    CursorMode, CursorRequest, DamageSupport, Downgrade, DowngradeReason, Exhaustion,
    FormatRequest, HdrPolicy, OrientationHandling, OutputSize, PixelFormat, Provenance,
    Requirement, Scaling, SourceKind, SourceListing, Strength, Unmet,
};

/// `DRM_FORMAT_MOD_INVALID`: the implicit modifier. Chosen only when the
/// caller listed it.
pub(crate) const MOD_INVALID: u64 = 0x00ff_ffff_ffff_ffff;
/// `DRM_FORMAT_MOD_LINEAR`.
pub(crate) const MOD_LINEAR: u64 = 0;

/// The environment override of `Auto`.
pub(crate) const BACKEND_ENV: &str = "ISTMO_SCREEN_CAPTURE_BACKEND";

/// The kind of desktop session, which decides the `Auto` order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionKind {
    /// A Wayland session, not sandboxed.
    Wayland,
    /// Flatpak / Snap: only the portal.
    Sandboxed,
    /// X11 without Wayland.
    X11,
    /// Nothing detected (headless, a TTY).
    Unknown,
}

impl SessionKind {
    /// The `Auto` order on Linux for this session.
    #[must_use]
    pub(crate) const fn linux_order(self) -> &'static [BackendId] {
        match self {
            Self::Wayland => &[
                BackendId::LinuxExtImageCopy,
                BackendId::LinuxWlrScreencopy,
                BackendId::LinuxPortal,
            ],
            Self::Sandboxed => &[BackendId::LinuxPortal],
            Self::X11 => &[BackendId::LinuxX11, BackendId::LinuxPortal],
            Self::Unknown => &[],
        }
    }
}

/// What a call needs from a backend, derived from its config.
#[derive(Debug, Clone, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag is one independent requirement of the call"
)]
pub(crate) struct Requirements {
    /// Kinds of the sources the call names.
    pub(crate) kinds: Vec<SourceKind>,
    /// A window / application named by id.
    pub(crate) by_id: bool,
    pub(crate) allow_picker: bool,
    /// Several monitors (or a region spanning them).
    pub(crate) multi_monitor: bool,
    /// A live session rather than a screenshot.
    pub(crate) streaming: bool,
    pub(crate) formats: FormatRequest,
    pub(crate) cursor: CursorRequest,
    pub(crate) damage: Strength,
    pub(crate) hdr: HdrPolicy,
    pub(crate) exhaustion: Exhaustion,
    pub(crate) size: OutputSize,
    pub(crate) orientation: OrientationHandling,
    pub(crate) audio: bool,
}

impl Default for Requirements {
    fn default() -> Self {
        Self {
            kinds: vec![SourceKind::Monitor],
            by_id: false,
            allow_picker: false,
            multi_monitor: false,
            streaming: false,
            formats: FormatRequest::default(),
            cursor: CursorRequest::default(),
            damage: Strength::Ignore,
            hdr: HdrPolicy::Sdr,
            exhaustion: Exhaustion::Stall,
            size: OutputSize::Native,
            orientation: OrientationHandling::InMeta,
            audio: false,
        }
    }
}

const fn known(provenance: Provenance) -> bool {
    !matches!(provenance, Provenance::UnknownUntilNegotiated)
}

impl Requirements {
    /// Requirements `caps` cannot meet. Sections marked
    /// `UnknownUntilNegotiated` count as "maybe": they never reject.
    #[must_use]
    pub(crate) fn unmet(&self, caps: &BackendCaps) -> Vec<Requirement> {
        let p = caps.provenance;
        let mut unmet = Vec::new();
        if known(p.source_kinds) {
            for kind in &self.kinds {
                if !caps.source_kinds.contains(kind) {
                    unmet.push(Requirement::Kind(*kind));
                }
            }
        }
        if self.by_id && caps.listing == SourceListing::PickerOnly && !self.allow_picker {
            unmet.push(Requirement::WindowById);
        }
        if self.multi_monitor {
            let support = if self.streaming {
                caps.composite.streaming
            } else {
                caps.composite.screenshot
            };
            if support == CompositeSupport::Unsupported {
                unmet.push(if self.streaming {
                    Requirement::CompositeStreaming
                } else {
                    Requirement::MultiMonitor
                });
            }
        }
        if known(p.video)
            && !self.formats.tiers.is_empty()
            && negotiate(&self.formats, &offers_of(caps), 0).is_none()
        {
            unmet.push(Requirement::Formats);
        }
        if self.cursor.strength == Strength::Require
            && known(p.cursor)
            && !caps.cursor.contains(&self.cursor.mode)
        {
            unmet.push(Requirement::Cursor);
        }
        if self.damage == Strength::Require
            && known(p.damage)
            && caps.damage != DamageSupport::Rects
        {
            unmet.push(Requirement::Damage);
        }
        if self.hdr == HdrPolicy::RequireHdr && known(p.colour) && !caps.colour.hdr {
            unmet.push(Requirement::Hdr);
        }
        if self.exhaustion == Exhaustion::CopyToCpu
            && !caps.pool.exhaustion.contains(&Exhaustion::CopyToCpu)
        {
            unmet.push(Requirement::ExhaustionReadback);
        }
        if !matches!(self.size, OutputSize::Native) && caps.scaling == Scaling::Unsupported {
            unmet.push(Requirement::Scaling);
        }
        if self.audio && known(p.audio) && caps.audio.sources.is_empty() {
            unmet.push(Requirement::Audio);
        }
        unmet
    }
}

/// One backend as the host sees it before choosing.
#[derive(Debug, Clone)]
pub(crate) struct Candidate {
    pub(crate) backend: BackendId,
    pub(crate) availability: Availability,
    /// `None` when the backend could not report (it is then not ready).
    pub(crate) caps: Option<BackendCaps>,
}

/// The backends to try, best first. A host tries them in order and
/// stops at the first that serves the call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Choice {
    pub(crate) backends: Vec<BackendId>,
    /// Why each passed-over backend was skipped (for `NoBackend`).
    pub(crate) skipped: Vec<BackendAttempt>,
}

/// Parse the environment override: `auto` (or empty) is no override,
/// otherwise a comma list of backend ids. Unknown ids are logged and
/// dropped.
#[must_use]
pub(crate) fn parse_override(value: &str) -> Option<Vec<BackendId>> {
    let value = value.trim();
    if value.is_empty() || value.eq_ignore_ascii_case("auto") {
        return None;
    }
    let ids: Vec<BackendId> = value
        .split(',')
        .filter(|part| !part.trim().is_empty())
        .filter_map(|part| match part.parse() {
            Ok(id) => Some(id),
            Err(err) => {
                tracing::warn!(%err, "{BACKEND_ENV}: ignoring entry");
                None
            }
        })
        .collect();
    (!ids.is_empty()).then_some(ids)
}

fn reorder(order: &[BackendId], prefer: AutoPreference) -> Vec<BackendId> {
    let mut order = order.to_vec();
    match prefer {
        // The platform order already puts the zero-copy, low-latency
        // direct protocols first.
        AutoPreference::ZeroCopy | AutoPreference::LowLatency => {}
        // Compatibility: the most widely supported paths first.
        AutoPreference::Compatibility => order.sort_by_key(|id| {
            !matches!(
                id,
                BackendId::LinuxPortal | BackendId::LinuxX11 | BackendId::WindowsGdi
            )
        }),
    }
    order
}

const fn attempt(backend: BackendId, failure: AttemptFailure) -> BackendAttempt {
    BackendAttempt { backend, failure }
}

/// Check one backend for `Fixed` / `Ordered` / `Auto`.
fn check(
    backend: BackendId,
    candidates: &[Candidate],
    policy: &CapturePolicy,
    requirements: &Requirements,
) -> Result<(), (CaptureError, AttemptFailure)> {
    let Some(candidate) = candidates.iter().find(|c| c.backend == backend) else {
        return Err((
            CaptureError::BackendNotCompiled(backend),
            AttemptFailure::Unavailable(Availability::NotCompiled(backend.as_str().to_owned())),
        ));
    };
    if let Some(why) = policy.denial(backend) {
        return Err((
            CaptureError::PolicyDenied(format!("{backend}: {why}")),
            AttemptFailure::Unavailable(Availability::PolicyDenied(why)),
        ));
    }
    if !candidate.availability.is_ready() {
        return Err((
            CaptureError::BackendUnavailable(BackendReason {
                backend,
                availability: candidate.availability.clone(),
            }),
            AttemptFailure::Unavailable(candidate.availability.clone()),
        ));
    }
    let unmet = candidate
        .caps
        .as_ref()
        .map(|caps| requirements.unmet(caps))
        .unwrap_or_default();
    if unmet.is_empty() {
        Ok(())
    } else {
        Err((
            CaptureError::Unsatisfiable(Unmet {
                backend: Some(backend),
                requirements: unmet.clone(),
            }),
            AttemptFailure::Unsatisfiable(unmet),
        ))
    }
}

/// Choose the backends for a call.
///
/// `order` is the platform's `Auto` order; `env` the parsed
/// [`BACKEND_ENV`] override, which replaces `Auto` only (it is ignored,
/// and logged once, under `Fixed` / `Ordered`). The override can only
/// name backends the policy allows.
///
/// # Errors
/// `Fixed`: that backend's error. `Ordered` / `Auto`: `NoBackend` with
/// every attempt, or [`CaptureError::NoBackendCompiled`] when nothing is
/// compiled for this platform.
pub(crate) fn choose(
    selection: &BackendSelection,
    order: &[BackendId],
    env: Option<&[BackendId]>,
    candidates: &[Candidate],
    policy: &CapturePolicy,
    requirements: &Requirements,
) -> Result<Choice, CaptureError> {
    if candidates.is_empty() {
        return Err(CaptureError::NoBackendCompiled);
    }
    let walk: Vec<BackendId> = match selection {
        BackendSelection::Fixed(backend) => {
            warn_ignored_override(env);
            return check(*backend, candidates, policy, requirements)
                .map(|()| Choice {
                    backends: vec![*backend],
                    skipped: Vec::new(),
                })
                .map_err(|(err, _)| err);
        }
        BackendSelection::Ordered(backends) => {
            warn_ignored_override(env);
            backends.clone()
        }
        BackendSelection::Auto(auto) => {
            let base = env.unwrap_or(order);
            reorder(base, auto.prefer)
                .into_iter()
                .filter(|id| !auto.exclude.contains(id))
                .collect()
        }
    };
    let mut choice = Choice {
        backends: Vec::new(),
        skipped: Vec::new(),
    };
    for backend in walk {
        match check(backend, candidates, policy, requirements) {
            Ok(()) => choice.backends.push(backend),
            Err((_, failure)) => choice.skipped.push(attempt(backend, failure)),
        }
    }
    if choice.backends.is_empty() {
        return Err(CaptureError::NoBackend(choice.skipped));
    }
    Ok(choice)
}

fn warn_ignored_override(env: Option<&[BackendId]>) {
    static ONCE: Once = Once::new();
    if env.is_some() {
        ONCE.call_once(|| {
            tracing::info!(
                "{BACKEND_ENV} replaces `Auto` only; ignored for Fixed / Ordered selections"
            );
        });
    }
}

/// One format a producer offers right now (linux-dmabuf tranches,
/// session constraints, PipeWire `EnumFormat`, or the table).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Offer {
    pub(crate) format: PixelFormat,
    pub(crate) memory: BufferMemory,
    /// Producer preference order (tranche order). Empty for CPU memory,
    /// or for DMA-BUF when the producer does not say (the caller's list
    /// is then used as is).
    pub(crate) modifiers: Vec<u64>,
    /// Copies needed to deliver it.
    pub(crate) copies: Vec<CopyStage>,
}

/// The offers a capability report implies.
#[must_use]
pub(crate) fn offers_of(caps: &BackendCaps) -> Vec<Offer> {
    caps.video
        .iter()
        .map(|f| Offer {
            format: f.format,
            memory: f.memory,
            modifiers: f.modifiers.clone(),
            copies: f.copies.clone(),
        })
        .collect()
}

/// What negotiation picked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Chosen {
    /// Index of the tier in the request (0 for an empty request).
    pub(crate) tier: u8,
    pub(crate) format: PixelFormat,
    pub(crate) memory: BufferMemory,
    /// DMA-BUF: the modifiers to allocate with, caller order first.
    /// Empty for CPU memory.
    pub(crate) modifiers: Vec<u64>,
    pub(crate) copies: Vec<CopyStage>,
}

fn modifiers_for(wanted: &[u64], offered: &[u64]) -> Vec<u64> {
    if wanted.is_empty() {
        // "Any": the producer's order, never the implicit modifier.
        return offered
            .iter()
            .copied()
            .filter(|m| *m != MOD_INVALID)
            .collect();
    }
    if offered.is_empty() {
        return wanted.to_vec();
    }
    // The caller's order wins inside a tier.
    wanted
        .iter()
        .copied()
        .filter(|m| offered.contains(m))
        .collect()
}

/// Pick the first tier (from `start_tier`) that some offer satisfies.
///
/// Inside a tier, the caller's choice order and then its modifier order
/// win; the producer's order breaks the remaining ties. An empty request
/// takes the producer's first copy-free offer (else its first offer).
#[must_use]
pub(crate) fn negotiate(
    request: &FormatRequest,
    offers: &[Offer],
    start_tier: usize,
) -> Option<Chosen> {
    if request.tiers.is_empty() {
        if start_tier > 0 {
            return None;
        }
        let offer = offers
            .iter()
            .find(|o| o.copies.is_empty())
            .or_else(|| offers.first())?;
        return Some(Chosen {
            tier: 0,
            format: offer.format,
            memory: offer.memory,
            modifiers: offer
                .modifiers
                .iter()
                .copied()
                .filter(|m| *m != MOD_INVALID)
                .collect(),
            copies: offer.copies.clone(),
        });
    }
    for (index, tier) in request.tiers.iter().enumerate().skip(start_tier) {
        for choice in &tier.choices {
            for offer in offers {
                if offer.format != choice.format
                    || offer.memory != choice.memory
                    || !offer.copies.iter().all(|c| tier.allows_copy.contains(c))
                {
                    continue;
                }
                let modifiers = if choice.memory == BufferMemory::DmaBuf {
                    let modifiers = modifiers_for(&choice.modifiers, &offer.modifiers);
                    if modifiers.is_empty() {
                        continue;
                    }
                    modifiers
                } else {
                    Vec::new()
                };
                return Some(Chosen {
                    tier: u8::try_from(index).unwrap_or(u8::MAX),
                    format: offer.format,
                    memory: offer.memory,
                    modifiers,
                    copies: offer.copies.clone(),
                });
            }
        }
    }
    None
}

/// Move a session below `current` after `reason`, to a tier the caller
/// listed.
///
/// # Errors
/// [`CaptureError::FormatLost`] when no lower listed tier can be served.
pub(crate) fn downgrade(
    request: &FormatRequest,
    offers: &[Offer],
    current: u8,
    reason: DowngradeReason,
) -> Result<(Chosen, Downgrade), CaptureError> {
    let chosen =
        negotiate(request, offers, usize::from(current) + 1).ok_or(CaptureError::FormatLost)?;
    let downgrade = Downgrade {
        from_tier: current,
        to_tier: chosen.tier,
        reason,
    };
    Ok((chosen, downgrade))
}

/// The cursor mode a backend delivers for `request` given what it can
/// do: the requested mode when available, else embedded, else hidden.
#[must_use]
pub(crate) fn cursor_mode(request: CursorRequest, available: &[CursorMode]) -> CursorMode {
    if available.contains(&request.mode) {
        request.mode
    } else if request.mode != CursorMode::Hidden && available.contains(&CursorMode::Embedded) {
        CursorMode::Embedded
    } else {
        CursorMode::Hidden
    }
}

#[cfg(test)]
mod tests;
