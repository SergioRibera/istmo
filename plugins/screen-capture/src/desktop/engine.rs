//! The host's logic, run on the plugin's threads: candidate probing,
//! selection, source resolution, screenshots (composite included) and
//! the session table.

use std::collections::HashMap;
use std::collections::hash_map::RandomState;
use std::hash::BuildHasher as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::time::{Duration, Instant};

use istmo_core::{CallOrigin, FrameSender, NativeBuffer, NativeHandleId, Runtime};

use crate::composite::{self, cpu};
use crate::pixels::CpuView;
use crate::policy::CapturePolicy;
use crate::select::{self, Candidate, Requirements};
use crate::types::{
    AccessScope, AccessState, AccessStatus, AudioMeta, Availability, BackendCaps, BackendId,
    BackendReason, BackendStatus, BufferMemory, CapabilityQuery, CapabilityReport, CaptureError,
    CaptureEvent, CaptureSession, CompositeFill, CompositeInfo, CompositePart, ConsentModel,
    CopyStage, CpuPixelFormat, DamageSupport, FormatRequest, FrameMeta, IndicatorState,
    NegotiatedStream, OrientationHandling, PauseReason, Persistence, PixelFormat, PixelLayout,
    PlaneLayout, Provenance, RawImage, Rect, Requirement, ScreenshotInfo, ScreenshotRequest,
    SessionConfig, SessionUpdate, Source, SourceFilter, SourceKind, SourceList, SourceListing,
    SourceSelector, StopReason, Strength,
};

use super::backend::{
    AudioBackend, Backend, BackendSession, Captured, ScreenshotPlan, Shot, StreamPlan, Target,
};
use super::offload::Caller;
use super::output::{AudioOut, Crop, EventHub, FrameOut};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // Short table / state updates only; poisoning leaves them valid.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// How long probed availability and capabilities are reused. There is no
/// hot-plug listener yet, so a short time bound stands in for
/// invalidation on output changes.
const PROBE_TTL: Duration = Duration::from_secs(5);

struct SessionEntry {
    backend: BackendId,
    /// Who opened the session: only callers of the same origin may
    /// update, close or watch it.
    opener: CallOrigin,
    out: FrameOut,
    events: Arc<EventHub>,
    state: Mutex<EntryState>,
}

struct EntryState {
    video: Option<Box<dyn BackendSession>>,
    audio: Option<(Box<dyn BackendSession>, AudioOut)>,
    negotiated: NegotiatedStream,
    source: Source,
}

impl Drop for SessionEntry {
    fn drop(&mut self) {
        let mut state = lock(&self.state);
        // Stop the producers before ending the sinks.
        drop(state.video.take());
        if let Some((session, out)) = state.audio.take() {
            drop(session);
            out.stop();
        }
        drop(state);
        self.out.stop(StopReason::Closed);
    }
}

pub(crate) struct Engine {
    backends: Vec<Arc<dyn Backend>>,
    audio: Option<Arc<dyn AudioBackend>>,
    /// Backends this platform knows, compiled or not.
    known: Vec<BackendId>,
    order: Vec<BackendId>,
    env: Option<Vec<BackendId>>,
    policy: CapturePolicy,
    sessions: Arc<Mutex<HashMap<NativeHandleId, Arc<SessionEntry>>>>,
    hook_installed: OnceLock<()>,
    probed: Mutex<Option<(Instant, Vec<Candidate>)>>,
    /// Keyed hash session ids are drawn from: random per engine, so ids
    /// cannot be guessed from the ones a caller already saw.
    ids: RandomState,
    draws: AtomicU64,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field(
                "backends",
                &self.backends.iter().map(|b| b.id()).collect::<Vec<_>>(),
            )
            .field("order", &self.order)
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
}

impl Engine {
    /// The compiled backends of this platform.
    pub(crate) fn platform(policy: CapturePolicy) -> Self {
        #[cfg(target_os = "linux")]
        let (backends, audio, known, kind) = (
            super::linux::backends(),
            super::linux::audio(),
            super::linux::KNOWN.to_vec(),
            super::linux::session_kind(),
        );
        #[cfg(not(target_os = "linux"))]
        let (backends, audio, known, kind) = (
            Vec::<Arc<dyn Backend>>::new(),
            None::<Arc<dyn AudioBackend>>,
            Vec::<BackendId>::new(),
            select::SessionKind::Unknown,
        );
        let env = std::env::var(select::BACKEND_ENV)
            .ok()
            .and_then(|v| select::parse_override(&v));
        Self::new(
            backends,
            audio,
            known,
            kind.linux_order().to_vec(),
            env,
            policy,
        )
    }

    pub(crate) fn new(
        backends: Vec<Arc<dyn Backend>>,
        audio: Option<Arc<dyn AudioBackend>>,
        known: Vec<BackendId>,
        order: Vec<BackendId>,
        env: Option<Vec<BackendId>>,
        policy: CapturePolicy,
    ) -> Self {
        Self {
            backends,
            audio,
            known,
            order,
            env,
            policy,
            sessions: Arc::new(Mutex::new(HashMap::new())),
            hook_installed: OnceLock::new(),
            probed: Mutex::new(None),
            ids: RandomState::new(),
            draws: AtomicU64::new(0),
        }
    }

    /// The platform `Auto` order without the backends that have not
    /// joined `Auto` yet.
    fn auto_order(&self) -> Vec<BackendId> {
        self.order
            .iter()
            .copied()
            .filter(|id| self.backend(*id).is_some_and(|b| b.joins_auto()))
            .collect()
    }

    pub(crate) const fn policy(&self) -> &CapturePolicy {
        &self.policy
    }

    fn backend(&self, id: BackendId) -> Option<&Arc<dyn Backend>> {
        self.backends.iter().find(|b| b.id() == id)
    }

    fn availability(&self, backend: &dyn Backend) -> Availability {
        self.policy
            .denial(backend.id())
            .map_or_else(|| backend.availability(), Availability::PolicyDenied)
    }

    /// Every backend this platform knows and whether it can run, from the
    /// probe cache: at most one probe per [`PROBE_TTL`] however often
    /// (and by whom) it is asked.
    pub(crate) fn statuses(&self) -> Vec<BackendStatus> {
        let mut statuses: Vec<BackendStatus> = self
            .candidates()
            .into_iter()
            .map(|c| BackendStatus {
                backend: c.backend,
                availability: c.availability,
            })
            .collect();
        for id in &self.known {
            if self.backend(*id).is_none() {
                statuses.push(BackendStatus {
                    backend: *id,
                    availability: Availability::NotCompiled(id.as_str().to_owned()),
                });
            }
        }
        statuses
    }

    /// Availability and capabilities of every compiled backend, cached for
    /// [`PROBE_TTL`].
    fn candidates(&self) -> Vec<Candidate> {
        let mut probed = lock(&self.probed);
        if let Some((at, candidates)) = probed.as_ref()
            && at.elapsed() < PROBE_TTL
        {
            return candidates.clone();
        }
        let candidates: Vec<Candidate> = self
            .backends
            .iter()
            .map(|backend| {
                let availability = self.availability(backend.as_ref());
                if !availability.is_ready() {
                    return Candidate {
                        backend: backend.id(),
                        availability,
                        caps: None,
                    };
                }
                match backend.caps() {
                    Ok(caps) => Candidate {
                        backend: backend.id(),
                        availability,
                        caps: Some(self.with_audio(caps)),
                    },
                    Err(err) => Candidate {
                        backend: backend.id(),
                        availability: Availability::NotOnThisSession(format!(
                            "capability probe failed: {err}"
                        )),
                        caps: None,
                    },
                }
            })
            .collect();
        *probed = Some((Instant::now(), candidates.clone()));
        candidates
    }

    /// Audio comes from the audio backend, whatever the video backend.
    fn with_audio(&self, mut caps: BackendCaps) -> BackendCaps {
        if caps.audio.sources.is_empty()
            && let Some(audio) = &self.audio
        {
            caps.audio = audio.caps();
            caps.provenance.audio = Provenance::Table;
        }
        caps
    }

    pub(crate) fn capabilities(
        &self,
        query: &CapabilityQuery,
    ) -> Result<CapabilityReport, CaptureError> {
        if self.backends.is_empty() {
            return Err(CaptureError::NoBackendCompiled);
        }
        let wanted = query
            .backend
            .or_else(|| query.source.as_ref().map(|s| s.backend));
        let candidates = self.candidates();
        if let Some(id) = wanted {
            let candidate = candidates
                .iter()
                .find(|c| c.backend == id)
                .ok_or(CaptureError::BackendNotCompiled(id))?;
            if candidate.caps.is_none() {
                return Err(CaptureError::BackendUnavailable(BackendReason {
                    backend: id,
                    availability: candidate.availability.clone(),
                }));
            }
        }
        let backends = candidates
            .into_iter()
            .filter(|c| wanted.is_none_or(|id| c.backend == id))
            .filter_map(|c| c.caps)
            .filter(|caps| {
                query.kind.is_none_or(|kind| {
                    !matches!(
                        caps.provenance.source_kinds,
                        Provenance::Probe | Provenance::Table
                    ) || caps.source_kinds.contains(&kind)
                })
            })
            .collect();
        Ok(CapabilityReport { backends })
    }

    pub(crate) fn sources(&self, filter: &SourceFilter) -> Result<SourceList, CaptureError> {
        let backend = match filter.backend {
            Some(id) => {
                let backend = self
                    .backend(id)
                    .ok_or(CaptureError::BackendNotCompiled(id))?;
                self.ready(backend.as_ref())?;
                Arc::clone(backend)
            }
            None => self.first_lister()?,
        };
        let mut list = backend.sources()?;
        list.sources.retain(|s| filter.admits(s.kind));
        Ok(list)
    }

    fn ready(&self, backend: &dyn Backend) -> Result<(), CaptureError> {
        let availability = self.availability(backend);
        if availability.is_ready() {
            Ok(())
        } else {
            Err(CaptureError::BackendUnavailable(BackendReason {
                backend: backend.id(),
                availability,
            }))
        }
    }

    /// The first ready backend in `Auto` order; enumerable ones first.
    fn first_lister(&self) -> Result<Arc<dyn Backend>, CaptureError> {
        if self.backends.is_empty() {
            return Err(CaptureError::NoBackendCompiled);
        }
        let candidates = self.candidates();
        let order = self.env.as_deref().unwrap_or(&self.order);
        let ready: Vec<&Candidate> = order
            .iter()
            .filter_map(|id| {
                candidates
                    .iter()
                    .find(|c| c.backend == *id && c.caps.is_some())
            })
            .collect();
        let pick = ready
            .iter()
            .find(|c| {
                c.caps
                    .as_ref()
                    .is_some_and(|caps| caps.listing == SourceListing::Enumerable)
            })
            .or_else(|| ready.first())
            .and_then(|c| self.backend(c.backend))
            .ok_or_else(|| {
                CaptureError::NoBackend(
                    candidates
                        .iter()
                        .map(|c| crate::types::BackendAttempt {
                            backend: c.backend,
                            failure: crate::types::AttemptFailure::Unavailable(
                                c.availability.clone(),
                            ),
                        })
                        .collect(),
                )
            })?;
        Ok(Arc::clone(pick))
    }

    pub(crate) const fn access(scope: AccessScope) -> AccessState {
        AccessState {
            scope,
            status: AccessStatus::NotApplicable,
            model: ConsentModel::NoneNeeded,
            persistence: Persistence::Unsupported,
            revocable_mid_session: false,
            indicator: IndicatorState::NotApplicable,
        }
    }

    /// A selector naming sources of one backend pins `Auto` to it.
    fn pinned(
        selection: &crate::types::BackendSelection,
        selector: &SourceSelector,
    ) -> crate::types::BackendSelection {
        use crate::types::BackendSelection;
        let owner = match selector {
            SourceSelector::Id(id) => Some(id.backend),
            SourceSelector::Monitors(ids) => ids.first().map(|id| id.backend),
            SourceSelector::Region(region) => match &region.space {
                crate::types::RegionSpace::MonitorRelative(id) => Some(id.backend),
                crate::types::RegionSpace::Global => None,
            },
            _ => None,
        };
        match (selection, owner) {
            (BackendSelection::Auto(_), Some(owner)) => BackendSelection::Fixed(owner),
            _ => selection.clone(),
        }
    }

    fn requirements_for(selector: &SourceSelector, allow_picker: bool) -> Requirements {
        let multi_monitor = match selector {
            SourceSelector::AllMonitors => true,
            SourceSelector::Monitors(ids) => ids.len() > 1,
            _ => false,
        };
        let (kinds, by_id) = match selector {
            SourceSelector::Picker(options) if !options.kinds.is_empty() => {
                (options.kinds.clone(), false)
            }
            _ => (
                vec![SourceKind::Monitor],
                matches!(selector, SourceSelector::Id(_)),
            ),
        };
        Requirements {
            kinds,
            by_id,
            allow_picker,
            multi_monitor,
            ..Requirements::default()
        }
    }

    /// Resolve `selector` on `backend`.
    #[expect(
        clippy::too_many_lines,
        reason = "one match over every selector; splitting it scatters the rules"
    )]
    fn resolve(
        backend: &dyn Backend,
        caps: Option<&BackendCaps>,
        selector: &SourceSelector,
        consent: &crate::types::ConsentConfig,
    ) -> Result<Target, CaptureError> {
        let id = backend.id();
        if matches!(selector, SourceSelector::Monitors(ids) if ids.is_empty()) {
            return Err(CaptureError::unsatisfiable(
                Some(id),
                Requirement::Kind(SourceKind::Monitor),
            ));
        }
        let picker_only = caps.is_some_and(|c| c.listing == SourceListing::PickerOnly);
        if picker_only {
            let options = match selector {
                SourceSelector::Picker(options) => options.clone(),
                SourceSelector::AllMonitors | SourceSelector::Monitors(_) => {
                    crate::types::PickerOptions {
                        kinds: vec![SourceKind::Monitor],
                        multiple: true,
                    }
                }
                SourceSelector::Id(_) => {
                    if !consent.allow_picker {
                        return Err(CaptureError::unsatisfiable(
                            Some(id),
                            Requirement::WindowById,
                        ));
                    }
                    crate::types::PickerOptions::default()
                }
                // The person picks the monitor: the consent dialog is the
                // only way to name one here.
                SourceSelector::PrimaryMonitor => crate::types::PickerOptions {
                    kinds: vec![SourceKind::Monitor],
                    multiple: false,
                },
                // A picker cannot honour these, and handing back whatever
                // the person picked would be a silent substitution.
                SourceSelector::Region(_) => {
                    return Err(CaptureError::unsatisfiable(Some(id), Requirement::Region));
                }
                SourceSelector::Focused | SourceSelector::UnderCursor => {
                    return Err(CaptureError::unsatisfiable(
                        Some(id),
                        Requirement::SourceSelector,
                    ));
                }
            };
            if !consent.allow_picker && consent.restore_token.is_none() {
                return Err(CaptureError::ConsentRequired);
            }
            return Ok(Target::Picker(options));
        }
        let list = || backend.sources().map(|l| l.sources);
        let monitors = |sources: Vec<Source>| -> Vec<Source> {
            sources
                .into_iter()
                .filter(|s| s.kind == SourceKind::Monitor)
                .collect()
        };
        let single = |source: Source| Target::Sources {
            sources: vec![source],
            region: None,
        };
        match selector {
            SourceSelector::Id(wanted) => list()?
                .into_iter()
                .find(|s| &s.id == wanted)
                .map(single)
                .ok_or_else(|| CaptureError::SourceNotFound(wanted.clone())),
            SourceSelector::PrimaryMonitor => {
                let monitors = monitors(list()?);
                monitors
                    .iter()
                    .find(|s| s.flags.primary)
                    .or_else(|| monitors.first())
                    .cloned()
                    .map(single)
                    .ok_or_else(|| {
                        CaptureError::unsatisfiable(
                            Some(id),
                            Requirement::Kind(SourceKind::Monitor),
                        )
                    })
            }
            SourceSelector::AllMonitors => {
                let monitors = monitors(list()?);
                if monitors.is_empty() {
                    return Err(CaptureError::unsatisfiable(
                        Some(id),
                        Requirement::Kind(SourceKind::Monitor),
                    ));
                }
                Ok(Target::Sources {
                    sources: monitors,
                    region: None,
                })
            }
            SourceSelector::Monitors(ids) => {
                let all = list()?;
                let sources = ids
                    .iter()
                    .map(|wanted| {
                        all.iter()
                            .find(|s| &s.id == wanted)
                            .cloned()
                            .ok_or_else(|| CaptureError::SourceNotFound(wanted.clone()))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(Target::Sources {
                    sources,
                    region: None,
                })
            }
            SourceSelector::Region(region) => {
                let monitors = monitors(list()?);
                let global = match &region.space {
                    crate::types::RegionSpace::Global => region.rect,
                    crate::types::RegionSpace::MonitorRelative(owner) => {
                        let monitor = monitors
                            .iter()
                            .find(|s| &s.id == owner)
                            .ok_or_else(|| CaptureError::SourceNotFound(owner.clone()))?;
                        let origin = monitor.geometry.unwrap_or_default();
                        Rect::new(
                            region.rect.x.saturating_add(origin.x),
                            region.rect.y.saturating_add(origin.y),
                            region.rect.width,
                            region.rect.height,
                        )
                    }
                };
                let sources: Vec<Source> = monitors
                    .into_iter()
                    .filter(|s| s.geometry.is_some_and(|g| g.intersect(global).is_some()))
                    .collect();
                if sources.is_empty() {
                    return Err(CaptureError::unsatisfiable(Some(id), Requirement::Region));
                }
                Ok(Target::Sources {
                    sources,
                    region: Some(global),
                })
            }
            SourceSelector::Picker(_) => {
                Err(CaptureError::unsatisfiable(Some(id), Requirement::Picker))
            }
            SourceSelector::Focused | SourceSelector::UnderCursor => {
                backend.resolve_special(selector).map(single)
            }
        }
    }

    // ----- screenshots -------------------------------------------------

    /// Walk the chosen backends until one serves `attempt`.
    fn walk<T>(
        &self,
        selection: &crate::types::BackendSelection,
        requirements: &Requirements,
        mut attempt: impl FnMut(&Arc<dyn Backend>, Option<&BackendCaps>) -> Result<T, CaptureError>,
    ) -> Result<T, CaptureError> {
        let candidates = self.candidates();
        let order = self.auto_order();
        let choice = select::choose(
            selection,
            &order,
            self.env.as_deref(),
            &candidates,
            &self.policy,
            requirements,
        )?;
        let single = choice.backends.len() == 1 && choice.skipped.is_empty();
        let mut attempts = choice.skipped;
        let mut last = None;
        for id in choice.backends {
            let Some(backend) = self.backend(id) else {
                continue;
            };
            let caps = candidates
                .iter()
                .find(|c| c.backend == id)
                .and_then(|c| c.caps.as_ref());
            match attempt(backend, caps) {
                Ok(value) => return Ok(value),
                // Consent answers are the person's, not the backend's:
                // never fall through to another backend after them.
                Err(
                    err @ (CaptureError::ConsentDenied
                    | CaptureError::ConsentRequired
                    | CaptureError::SinkUnreachable),
                ) => {
                    return Err(err);
                }
                Err(err) => {
                    tracing::debug!(backend = %id, %err, "screen-capture: backend failed, trying the next");
                    attempts.push(crate::types::BackendAttempt {
                        backend: id,
                        failure: crate::types::AttemptFailure::Failed(err.to_string()),
                    });
                    last = Some(err);
                }
            }
        }
        match last {
            Some(err)
                if single || matches!(selection, crate::types::BackendSelection::Fixed(_)) =>
            {
                Err(err)
            }
            _ => Err(CaptureError::NoBackend(attempts)),
        }
    }

    fn shoot(
        &self,
        request: &ScreenshotRequest,
        force_cpu: bool,
    ) -> Result<(Shot, Option<Rect>, Vec<Source>), CaptureError> {
        let selection = Self::pinned(&request.backend, &request.source);
        let mut requirements =
            Self::requirements_for(&request.source, request.consent.allow_picker);
        requirements.cursor = request.cursor;
        requirements.hdr = request.hdr;
        let formats = if force_cpu || request.formats.tiers.is_empty() {
            FormatRequest::cpu()
        } else {
            request.formats.clone()
        };
        requirements.formats = formats.clone();
        self.walk(&selection, &requirements, |backend, caps| {
            let target = Self::resolve(backend.as_ref(), caps, &request.source, &request.consent)?;
            let (region, sources) = match &target {
                Target::Sources { sources, region } => (*region, sources.clone()),
                Target::Picker(_) => (None, Vec::new()),
            };
            let several = target.len() > 1;
            if several && !formats.has_cpu_tier() {
                // The CPU composite needs CPU parts.
                return Err(CaptureError::unsatisfiable(
                    Some(backend.id()),
                    Requirement::Formats,
                ));
            }
            let plan = ScreenshotPlan {
                target,
                formats: if several {
                    FormatRequest::cpu()
                } else {
                    formats.clone()
                },
                cursor: request.cursor,
                hdr: request.hdr,
                consent: request.consent.clone(),
                linux: crate::types::LinuxOptions::default(),
            };
            let shot = backend.screenshot(&plan)?;
            if shot.frames.is_empty() {
                return Err(CaptureError::protocol(
                    backend.id(),
                    "screenshot",
                    "no frame captured",
                ));
            }
            Ok((shot, region, sources))
        })
    }

    /// Composite several CPU frames (or crop one) per the request.
    #[expect(
        clippy::too_many_lines,
        reason = "the composite hand-off fills every negotiated field in one place"
    )]
    fn finish(
        request: &ScreenshotRequest,
        shot: Shot,
        region: Option<Rect>,
    ) -> Result<(Vec<Captured>, crate::types::NegotiatedStream), CaptureError> {
        let Shot {
            mut frames,
            mut negotiated,
        } = shot;
        let backend = negotiated.backend;
        let spans = frames.len() > 1;
        if !spans {
            if let (Some(region), Some(frame)) = (region, frames.first_mut()) {
                let geometry = frame.source.geometry.unwrap_or_default();
                let logical = region
                    .intersect(geometry)
                    .unwrap_or(region)
                    .relative_to(geometry.x, geometry.y);
                let crop = Crop {
                    logical,
                    scale_permille: frame.source.scale(),
                };
                frame.meta.content_rect = super::output::crop_rect_for(&frame.meta, crop);
                negotiated.content_rect = frame.meta.content_rect;
            }
            return Ok((frames, negotiated));
        }
        if !request.composite {
            return Ok((frames, negotiated));
        }
        let sources: Vec<Source> = frames.iter().map(|f| f.source.clone()).collect();
        let layout = composite::layout(&sources, region, &request.composite_config)
            .map_err(|err| composite_error(backend, &err))?;
        let views = frames
            .iter()
            .map(|frame| {
                let bytes = frame.buffer.as_bytes().ok_or_else(|| {
                    CaptureError::protocol(backend, "composite", "a part is not in CPU memory")
                })?;
                let stride = frame.meta.planes.first().map_or(0, |p| p.stride);
                let view = CpuView::new(bytes, frame.meta.size, stride, frame.meta.format)
                    .map_err(|err| CaptureError::protocol(backend, "composite", err))?;
                Ok(cpu::Part {
                    view,
                    orientation: frame.meta.orientation,
                })
            })
            .collect::<Result<Vec<_>, CaptureError>>()?;
        // Parts in layout order (a region may leave some out).
        let ordered: Vec<cpu::Part<'_>> = layout
            .parts
            .iter()
            .filter_map(|plan| {
                sources
                    .iter()
                    .position(|s| s.id == plan.source)
                    .and_then(|i| views.get(i).copied())
            })
            .collect();
        let composed = cpu::compose(&layout, &ordered, request.composite_config.fill)
            .map_err(|err| composite_error(backend, &err))?;
        let first_pts = frames.iter().map(|f| f.meta.pts_ns).min().unwrap_or(0);
        let last_pts = frames.iter().map(|f| f.meta.pts_ns).max().unwrap_or(0);
        let info = CompositeInfo {
            parts: layout
                .parts
                .iter()
                .map(|plan| CompositePart {
                    source: plan.source.clone(),
                    rect: plan.dest,
                    pts_ns: frames
                        .iter()
                        .find(|f| f.source.id == plan.source)
                        .map_or(0, |f| f.meta.pts_ns),
                    scale_permille: plan.scale_permille,
                })
                .collect(),
            skew_ns: last_pts - first_pts,
        };
        let mut meta = frames.remove(0).meta;
        let alpha = request.composite_config.fill == CompositeFill::Transparent;
        meta.size = composed.size;
        meta.content_rect = Rect::from_size(composed.size);
        meta.format = if alpha {
            PixelFormat::Bgra8
        } else {
            PixelFormat::Bgrx8
        };
        meta.memory = BufferMemory::Cpu;
        meta.planes = vec![PlaneLayout {
            offset: 0,
            stride: composed.size.width * 4,
            size: u64::from(composed.size.width) * 4 * u64::from(composed.size.height),
        }];
        meta.modifier = None;
        meta.copied = Some(CopyStage::Composite);
        meta.orientation = crate::types::Orientation::UPRIGHT;
        meta.damage = crate::types::Damage::Full;
        meta.pts_ns = last_pts;
        meta.scale_permille = layout.scale_permille;
        meta.composite = Some(info.clone());
        let mut source = Source::new(meta.source_id.clone(), SourceKind::Monitor, "composite");
        source.geometry = Some(layout.area);
        source.physical_size = Some(composed.size);
        source.scale_permille = Some(layout.scale_permille);
        negotiated.size = composed.size;
        negotiated.content_rect = meta.content_rect;
        negotiated.format = meta.format;
        negotiated.memory = BufferMemory::Cpu;
        negotiated.modifier = None;
        if !negotiated.copies.contains(&CopyStage::Composite) {
            negotiated.copies.push(CopyStage::Composite);
        }
        negotiated.composite = Some(info);
        negotiated.source = source.clone();
        let buffer = NativeBuffer::from_bytes(composed.bytes);
        Ok((
            vec![Captured {
                meta,
                buffer,
                source,
            }],
            negotiated,
        ))
    }

    pub(crate) fn screenshot_into(
        &self,
        request: &ScreenshotRequest,
        sink: &FrameSender<FrameMeta>,
        origin: CallOrigin,
    ) -> Result<ScreenshotInfo, CaptureError> {
        let (shot, region, _) = self.shoot(request, false)?;
        let (frames, mut negotiated) = Self::finish(request, shot, region)?;
        redact_for(origin, &mut negotiated);
        let count = u32::try_from(frames.len()).unwrap_or(u32::MAX);
        for frame in frames {
            let mut meta = frame.meta;
            meta.wall_us = super::clock::wall_us_for(meta.pts_ns, meta.pts_clock);
            sink.push(meta, frame.buffer).map_err(|err| {
                if err.is_closed() {
                    CaptureError::SinkUnreachable
                } else {
                    CaptureError::SinkFull
                }
            })?;
        }
        Ok(ScreenshotInfo {
            frames: count,
            backend: negotiated.backend,
            negotiated,
        })
    }

    pub(crate) fn screenshot_pixels(
        &self,
        request: &ScreenshotRequest,
        layout: PixelLayout,
    ) -> Result<RawImage, CaptureError> {
        let (shot, region, _) = self.shoot(request, layout.format != CpuPixelFormat::Rgba16f)?;
        if shot.frames.len() > 1 && !request.composite {
            return Err(CaptureError::unsatisfiable(
                Some(shot.negotiated.backend),
                Requirement::MultiMonitor,
            ));
        }
        let (frames, negotiated) = Self::finish(request, shot, region)?;
        let backend = negotiated.backend;
        let frame = frames
            .into_iter()
            .next()
            .ok_or_else(|| CaptureError::protocol(backend, "screenshot", "no frame captured"))?;
        let bytes = frame.buffer.as_bytes().ok_or_else(|| {
            CaptureError::protocol(backend, "pixels", "the frame is not in CPU memory")
        })?;
        let stride = frame.meta.planes.first().map_or(0, |p| p.stride);
        let view = CpuView::new(bytes, frame.meta.size, stride, frame.meta.format)
            .map_err(|err| CaptureError::protocol(backend, "pixels", err))?;
        let pixels = view
            .convert(frame.meta.content_rect, frame.meta.orientation, layout)
            .map_err(|err| CaptureError::protocol(backend, "pixels", err))?;
        Ok(RawImage {
            width: pixels.size.width,
            height: pixels.size.height,
            stride: pixels.stride,
            format: pixels.format,
            colour: frame.meta.colour,
            source: frame.meta.source_id,
            bytes: pixels.bytes,
        })
    }

    // ----- sessions ----------------------------------------------------

    fn install_release_hook(&self, runtime: &Arc<Runtime>) {
        self.hook_installed.get_or_init(|| {
            let sessions: Weak<Mutex<HashMap<NativeHandleId, Arc<SessionEntry>>>> =
                Arc::downgrade(&self.sessions);
            runtime.install_native_handle_release_hook(Arc::new(move |id| {
                // Every release in the runtime reaches every hook; ids
                // this host never handed out are not in the table.
                let Some(sessions) = sessions.upgrade() else {
                    return;
                };
                let Some(entry) = lock(&sessions).remove(&id) else {
                    return;
                };
                // Dropping the entry stops the capture (joins its threads)
                // and ends the sinks: never on the thread that dropped the
                // handle, which may be an executor's.
                super::offload::detach("teardown", move || drop(entry));
            }));
        });
    }

    pub(crate) fn open(
        &self,
        runtime: &Arc<Runtime>,
        config: &SessionConfig,
        video: &FrameSender<FrameMeta>,
        audio: Option<FrameSender<AudioMeta>>,
        origin: CallOrigin,
        caller: &Caller,
    ) -> Result<CaptureSession, CaptureError> {
        if config.audio.is_some() != audio.is_some() {
            // `open` with an audio config, or `open_with_audio` without.
            return Err(CaptureError::unsatisfiable(None, Requirement::Audio));
        }
        if audio.is_some() && self.audio.is_none() {
            return Err(CaptureError::unsatisfiable(None, Requirement::Audio));
        }
        let selection = Self::pinned(&config.backend, &config.source);
        let mut requirements = Self::requirements_for(&config.source, config.consent.allow_picker);
        requirements.streaming = true;
        requirements.formats = config.video.formats.clone();
        requirements.cursor = config.video.cursor;
        requirements.damage = config.video.damage;
        requirements.hdr = config.video.hdr;
        requirements.exhaustion = config.video.exhaustion;
        requirements.size = config.video.size;
        requirements.orientation = config.video.orientation;
        requirements.audio = config.audio.is_some();
        let (backend_id, session, negotiated, out, events, source) =
            self.walk(&selection, &requirements, |backend, caps| {
                Self::open_on(backend, caps, config, video, &requirements)
            })?;
        let mut negotiated = negotiated;
        let mut audio_session = None;
        if let (Some(audio_config), Some(sender), Some(audio_backend)) =
            (&config.audio, audio, &self.audio)
        {
            let audio_out = AudioOut::new(sender, Arc::clone(&events));
            match audio_backend.open(audio_config, audio_out.clone()) {
                Ok((session, audio_negotiated)) => {
                    negotiated.audio = Some(audio_negotiated);
                    audio_session = Some((session, audio_out));
                }
                Err(err) => {
                    drop(session);
                    out.fail(err.clone());
                    return Err(err);
                }
            }
        }
        if caller.is_gone() {
            // Nobody holds the id: a session started now (portal consent
            // given after the app gave up) could never be stopped.
            tracing::debug!(backend = %backend_id, "screen-capture: open abandoned; session discarded");
            drop(audio_session);
            drop(session);
            out.stop(StopReason::Closed);
            return Err(CaptureError::SessionNotFound);
        }
        redact_for(origin, &mut negotiated);
        self.install_release_hook(runtime);
        out.event(CaptureEvent::Started(negotiated.clone()));
        let entry = Arc::new(SessionEntry {
            backend: backend_id,
            opener: origin,
            out,
            events,
            state: Mutex::new(EntryState {
                video: Some(session),
                audio: audio_session,
                negotiated: negotiated.clone(),
                source,
            }),
        });
        let id = self.insert(entry);
        Ok(CaptureSession {
            session: id,
            negotiated,
        })
    }

    #[expect(
        clippy::type_complexity,
        reason = "one private hand-off of everything a successful open produced"
    )]
    #[expect(
        clippy::too_many_lines,
        reason = "resolution, plan, open and the post-consent re-check of one session"
    )]
    fn open_on(
        backend: &Arc<dyn Backend>,
        caps: Option<&BackendCaps>,
        config: &SessionConfig,
        video: &FrameSender<FrameMeta>,
        requirements: &Requirements,
    ) -> Result<
        (
            BackendId,
            Box<dyn BackendSession>,
            NegotiatedStream,
            FrameOut,
            Arc<EventHub>,
            Source,
        ),
        CaptureError,
    > {
        let id = backend.id();
        let target = Self::resolve(backend.as_ref(), caps, &config.source, &config.consent)?;
        let (source, picker, crop) = match target {
            Target::Sources { sources, .. } if sources.len() > 1 => {
                return Err(CaptureError::unsatisfiable(
                    Some(id),
                    Requirement::CompositeStreaming,
                ));
            }
            Target::Sources {
                mut sources,
                region,
            } => {
                let source = sources
                    .pop()
                    .ok_or_else(|| CaptureError::unsatisfiable(Some(id), Requirement::Region))?;
                let crop = region.map(|region| {
                    let geometry = source.geometry.unwrap_or_default();
                    Crop {
                        logical: region
                            .intersect(geometry)
                            .unwrap_or(region)
                            .relative_to(geometry.x, geometry.y),
                        scale_permille: source.scale(),
                    }
                });
                (source, None, crop)
            }
            Target::Picker(options) => {
                let placeholder = Source::new(
                    crate::types::SourceId::new(id, "picker"),
                    SourceKind::Monitor,
                    "picker",
                );
                (placeholder, Some(options), None)
            }
        };
        let pool_depth = Self::pool_depth(
            config.video.pool_depth,
            caps.map(|c| &c.pool),
            config.video.fps,
        );
        let events = Arc::new(EventHub::default());
        let out = FrameOut::new(id, video.clone(), Arc::clone(&events), config.video.fps);
        out.set_crop(crop);
        if config.video.orientation == OrientationHandling::Upright {
            out.set_upright(true);
        }
        let plan = StreamPlan {
            source,
            formats: config.video.formats.clone(),
            fps: config.video.fps,
            size: config.video.size,
            cursor: config.video.cursor,
            damage: config.video.damage,
            hdr: config.video.hdr,
            pool_depth,
            exhaustion: config.video.exhaustion,
            gpu: config.video.gpu,
            orientation: config.video.orientation,
            live_buffers: config.video.live_buffers,
            consent: config.consent.clone(),
            linux: config.linux,
            picker,
        };
        let (session, mut negotiated) = match backend.open(plan, out.clone()) {
            Ok(opened) => opened,
            Err(err) => {
                out.fail(err.clone());
                return Err(err);
            }
        };
        // Facts that were unknown before consent are checked again.
        let mut unmet = Vec::new();
        if requirements.cursor.strength == Strength::Require
            && negotiated.cursor != requirements.cursor.mode
        {
            unmet.push(Requirement::Cursor);
        }
        if requirements.damage == Strength::Require && negotiated.damage != DamageSupport::Rects {
            unmet.push(Requirement::Damage);
        }
        // Frames the plugin has to turn: only a source whose frames arrive
        // rotated. An upright source needs no turn, whatever its memory.
        let turns = negotiated.orientation == crate::types::OrientationSupport::InMeta
            && !negotiated.source.orientation.is_upright();
        if requirements.orientation == OrientationHandling::Upright
            && turns
            && negotiated.memory != BufferMemory::Cpu
        {
            unmet.push(Requirement::Upright);
        }
        if !unmet.is_empty() {
            drop(session);
            let err = CaptureError::Unsatisfiable(crate::types::Unmet {
                backend: Some(id),
                requirements: unmet,
            });
            out.fail(err.clone());
            return Err(err);
        }
        if config.video.orientation == OrientationHandling::Upright
            && turns
            && !negotiated.copies.contains(&CopyStage::OrientationUpright)
        {
            negotiated.copies.push(CopyStage::OrientationUpright);
        }
        if let Some(crop) = crop {
            negotiated.content_rect = super::output::crop_rect_for_size(
                negotiated.size,
                negotiated.source.orientation,
                crop,
            );
        }
        negotiated.fps = config.video.fps;
        let captured = negotiated.source.clone();
        Ok((id, session, negotiated, out, events, captured))
    }

    /// Store `entry` under a fresh unguessable id: random, with the two
    /// high bits set so it stays in the Rust-allocated range and away
    /// from the runtime's own counter.
    fn insert(&self, entry: Arc<SessionEntry>) -> NativeHandleId {
        const HIGH: u64 = 0b11 << 62;
        let mut sessions = lock(&self.sessions);
        let id = loop {
            let draw = self.draws.fetch_add(1, Ordering::Relaxed);
            let id = NativeHandleId(self.ids.hash_one(draw) | HIGH);
            if let std::collections::hash_map::Entry::Vacant(slot) = sessions.entry(id) {
                slot.insert(entry);
                break id;
            }
        };
        drop(sessions);
        id
    }

    /// The session `id`, for a caller of the origin that opened it.
    /// The pool depth to ask the backend for: the caller's (or the
    /// backend default) plus the frames the plugin's own pacer keeps (a
    /// pending one under `Max` / `OnDamage`, plus the repeated one under
    /// `Exact`), which the caller cannot see; at least one frame for the
    /// sink and one buffer for the producer; clamped to the backend.
    fn pool_depth(
        requested: u8,
        pool: Option<&crate::types::PoolCaps>,
        fps: crate::types::FrameRate,
    ) -> u8 {
        use crate::types::FrameRate;
        let kept: u8 = match fps {
            FrameRate::Native => 0,
            FrameRate::Max(_) | FrameRate::OnDamage(_) => 1,
            FrameRate::Exact(_) => 2,
        };
        let floor = kept.saturating_add(2);
        pool.map_or_else(
            || requested.saturating_add(kept).max(floor),
            |pool| {
                let wanted = if requested == 0 {
                    pool.default_depth
                } else {
                    requested
                };
                wanted
                    .saturating_add(kept)
                    .max(floor)
                    .clamp(pool.min_depth, pool.max_depth)
            },
        )
    }

    fn entry(
        &self,
        id: NativeHandleId,
        origin: CallOrigin,
    ) -> Result<Arc<SessionEntry>, CaptureError> {
        let entry = lock(&self.sessions)
            .get(&id)
            .cloned()
            .ok_or(CaptureError::SessionNotFound)?;
        if entry.opener == origin {
            Ok(entry)
        } else {
            Err(CaptureError::OriginRefused)
        }
    }

    pub(crate) fn update(
        &self,
        id: NativeHandleId,
        update: &SessionUpdate,
        origin: CallOrigin,
    ) -> Result<NegotiatedStream, CaptureError> {
        let entry = self.entry(id, origin)?;
        let mut state = lock(&entry.state);
        match update {
            SessionUpdate::Fps(fps) => {
                entry.out.set_fps(*fps);
                state.negotiated.fps = *fps;
            }
            SessionUpdate::Pause => {
                entry.out.set_paused(true);
                entry
                    .out
                    .event(CaptureEvent::Paused(PauseReason::Requested));
            }
            SessionUpdate::Resume => {
                entry.out.set_paused(false);
                entry.out.event(CaptureEvent::Resumed);
            }
            SessionUpdate::Region(region) => {
                let source = state.source.clone();
                let geometry = source.geometry.ok_or_else(|| {
                    CaptureError::unsatisfiable(Some(entry.backend), Requirement::Region)
                })?;
                let global = match &region.space {
                    crate::types::RegionSpace::Global => region.rect,
                    crate::types::RegionSpace::MonitorRelative(owner) if owner == &source.id => {
                        Rect::new(
                            region.rect.x.saturating_add(geometry.x),
                            region.rect.y.saturating_add(geometry.y),
                            region.rect.width,
                            region.rect.height,
                        )
                    }
                    crate::types::RegionSpace::MonitorRelative(_) => {
                        // Another monitor would change the source.
                        return Err(CaptureError::unsatisfiable(
                            Some(entry.backend),
                            Requirement::Region,
                        ));
                    }
                };
                let logical = global
                    .intersect(geometry)
                    .ok_or_else(|| {
                        CaptureError::unsatisfiable(Some(entry.backend), Requirement::Region)
                    })?
                    .relative_to(geometry.x, geometry.y);
                let crop = Crop {
                    logical,
                    scale_permille: source.scale(),
                };
                entry.out.set_crop(Some(crop));
                state.negotiated.content_rect = super::output::crop_rect_for_size(
                    state.negotiated.size,
                    source.orientation,
                    crop,
                );
            }
            SessionUpdate::Cursor(_) | SessionUpdate::Formats(_) => {
                let session = state.video.as_mut().ok_or(CaptureError::SessionNotFound)?;
                let mut negotiated = session.update(update)?;
                negotiated.fps = state.negotiated.fps;
                negotiated.audio.clone_from(&state.negotiated.audio);
                state.negotiated = negotiated;
            }
        }
        let mut negotiated = state.negotiated.clone();
        drop(state);
        entry
            .out
            .event(CaptureEvent::Renegotiated(negotiated.clone()));
        redact_for(origin, &mut negotiated);
        Ok(negotiated)
    }

    pub(crate) fn close(&self, id: NativeHandleId, origin: CallOrigin) -> Result<(), CaptureError> {
        self.entry(id, origin)?;
        let entry = lock(&self.sessions).remove(&id);
        drop(entry);
        Ok(())
    }

    pub(crate) fn events(
        &self,
        id: NativeHandleId,
        origin: CallOrigin,
    ) -> flume::Receiver<Result<CaptureEvent, CaptureError>> {
        match self.entry(id, origin) {
            Ok(entry) => entry.events.subscribe(),
            Err(err) => {
                let (tx, rx) = flume::bounded(1);
                let _queued = tx.try_send(Err(err));
                rx
            }
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        let sessions: Vec<_> = lock(&self.sessions).drain().collect();
        drop(sessions);
    }
}

/// What a caller of `origin` may see of a negotiated stream: restore
/// tokens and the captured source's title, app id and pid stay in this
/// process (titles and pids are why `sources` is local by default).
fn redact_for(origin: CallOrigin, negotiated: &mut NegotiatedStream) {
    match origin {
        CallOrigin::Local | CallOrigin::Peer => {}
        // Remote, and any origin added later.
        _ => {
            negotiated.restore_token = None;
            negotiated.source.name.clear();
            negotiated.source.app_id = None;
            negotiated.source.pid = None;
        }
    }
}

/// A composite that cannot be built, as the requirement it misses.
fn composite_error(backend: BackendId, err: &composite::LayoutError) -> CaptureError {
    let requirement = match err {
        composite::LayoutError::TooLarge => Requirement::CompositeSize,
        composite::LayoutError::Empty => Requirement::Region,
        composite::LayoutError::Unreadable(_) => Requirement::Formats,
        composite::LayoutError::NoGeometry(_) => Requirement::MultiMonitor,
    };
    tracing::debug!(%backend, %err, "screen-capture: composite refused");
    CaptureError::unsatisfiable(Some(backend), requirement)
}
