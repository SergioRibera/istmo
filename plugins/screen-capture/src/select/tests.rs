use super::*;
use crate::types::{AutoSelect, CapsProvenance, FormatCaps, FormatChoice, FormatTier};

fn caps(backend: BackendId) -> BackendCaps {
    let mut caps = BackendCaps::new(backend);
    caps.video = vec![
        FormatCaps {
            format: PixelFormat::Bgrx8,
            memory: BufferMemory::DmaBuf,
            modifiers: vec![0x0300_0000_0060_6014, MOD_LINEAR],
            copies: Vec::new(),
        },
        FormatCaps {
            format: PixelFormat::Bgrx8,
            memory: BufferMemory::Cpu,
            modifiers: Vec::new(),
            copies: vec![CopyStage::ShmTransport],
        },
    ];
    caps
}

fn ready(backend: BackendId) -> Candidate {
    Candidate {
        backend,
        availability: Availability::Ready,
        caps: Some(caps(backend)),
    }
}

fn wayland() -> Vec<Candidate> {
    vec![
        Candidate {
            backend: BackendId::LinuxExtImageCopy,
            availability: Availability::NotOnThisSession("no ext-image-copy global".into()),
            caps: None,
        },
        ready(BackendId::LinuxWlrScreencopy),
        ready(BackendId::LinuxPortal),
    ]
}

fn auto() -> BackendSelection {
    BackendSelection::Auto(AutoSelect::default())
}

#[test]
fn auto_walks_the_platform_order_skipping_what_cannot_run() {
    let order = SessionKind::Wayland.linux_order();
    let choice = choose(
        &auto(),
        order,
        None,
        &wayland(),
        &CapturePolicy::default(),
        &Requirements::default(),
    )
    .expect("a backend");
    assert_eq!(
        choice.backends,
        [BackendId::LinuxWlrScreencopy, BackendId::LinuxPortal]
    );
    assert_eq!(choice.skipped.len(), 1);
    assert_eq!(choice.skipped[0].backend, BackendId::LinuxExtImageCopy);
}

#[test]
fn the_override_replaces_auto_only() {
    let order = SessionKind::Wayland.linux_order();
    let env = parse_override("linux-portal, nonsense").expect("one id");
    assert_eq!(env, [BackendId::LinuxPortal]);
    let policy = CapturePolicy::default();
    let req = Requirements::default();
    let choice = choose(&auto(), order, Some(&env), &wayland(), &policy, &req).expect("portal");
    assert_eq!(choice.backends, [BackendId::LinuxPortal]);
    let fixed = BackendSelection::Fixed(BackendId::LinuxWlrScreencopy);
    let choice = choose(&fixed, order, Some(&env), &wayland(), &policy, &req).expect("wlr");
    assert_eq!(choice.backends, [BackendId::LinuxWlrScreencopy]);
    assert!(parse_override("auto").is_none());
    assert!(parse_override("").is_none());
}

#[test]
fn the_override_cannot_name_a_backend_the_policy_denies() {
    let env = [BackendId::LinuxPortal];
    let policy =
        CapturePolicy::default().with_allowed_backends(vec![BackendId::LinuxWlrScreencopy]);
    let err = choose(
        &auto(),
        &[],
        Some(&env),
        &wayland(),
        &policy,
        &Requirements::default(),
    )
    .expect_err("portal is denied");
    let CaptureError::NoBackend(attempts) = err else {
        panic!("unexpected {err:?}");
    };
    assert!(matches!(
        attempts[0].failure,
        AttemptFailure::Unavailable(Availability::PolicyDenied(_))
    ));
}

#[test]
fn fixed_answers_with_that_backends_own_error() {
    let order = SessionKind::Wayland.linux_order();
    let policy = CapturePolicy::default();
    let req = Requirements::default();
    let err = choose(
        &BackendSelection::Fixed(BackendId::LinuxX11),
        order,
        None,
        &wayland(),
        &policy,
        &req,
    )
    .expect_err("not compiled");
    assert_eq!(err, CaptureError::BackendNotCompiled(BackendId::LinuxX11));
    let err = choose(
        &BackendSelection::Fixed(BackendId::LinuxExtImageCopy),
        order,
        None,
        &wayland(),
        &policy,
        &req,
    )
    .expect_err("not on this session");
    assert!(matches!(err, CaptureError::BackendUnavailable(_)));
}

#[test]
fn ordered_reports_every_attempt() {
    let selection =
        BackendSelection::Ordered(vec![BackendId::LinuxX11, BackendId::LinuxExtImageCopy]);
    let err = choose(
        &selection,
        &[],
        None,
        &wayland(),
        &CapturePolicy::default(),
        &Requirements::default(),
    )
    .expect_err("none");
    let CaptureError::NoBackend(attempts) = err else {
        panic!("unexpected {err:?}");
    };
    assert_eq!(attempts.len(), 2);
}

#[test]
fn nothing_compiled_is_its_own_error() {
    let err = choose(
        &auto(),
        &[],
        None,
        &[],
        &CapturePolicy::default(),
        &Requirements::default(),
    )
    .expect_err("empty");
    assert_eq!(err, CaptureError::NoBackendCompiled);
}

#[test]
fn every_unsatisfiable_names_its_requirement() {
    let mut caps = caps(BackendId::LinuxWlrScreencopy);
    caps.cursor = vec![CursorMode::Embedded];
    let req = Requirements {
        kinds: vec![SourceKind::Window],
        cursor: CursorRequest {
            mode: CursorMode::Metadata,
            strength: Strength::Require,
        },
        damage: Strength::Require,
        hdr: HdrPolicy::RequireHdr,
        exhaustion: Exhaustion::CopyToCpu,
        multi_monitor: true,
        streaming: true,
        ..Requirements::default()
    };
    assert_eq!(
        req.unmet(&caps),
        [
            Requirement::Kind(SourceKind::Window),
            Requirement::CompositeStreaming,
            Requirement::Cursor,
            Requirement::Damage,
            Requirement::Hdr,
            Requirement::ExhaustionReadback,
        ]
    );
}

#[test]
fn unknown_sections_never_reject() {
    let mut caps = caps(BackendId::LinuxPortal);
    caps.provenance = CapsProvenance::all(Provenance::UnknownUntilNegotiated);
    caps.video.clear();
    let req = Requirements {
        formats: FormatRequest {
            tiers: vec![FormatTier::new(vec![FormatChoice::cpu(
                PixelFormat::Rgba16f,
            )])],
        },
        damage: Strength::Require,
        ..Requirements::default()
    };
    assert_eq!(req.unmet(&caps), []);
}

fn offers() -> Vec<Offer> {
    offers_of(&caps(BackendId::LinuxWlrScreencopy))
}

#[test]
fn the_callers_modifier_order_wins_inside_a_tier() {
    let request = FormatRequest {
        tiers: vec![FormatTier::new(vec![FormatChoice::dmabuf(
            PixelFormat::Bgrx8,
            vec![MOD_LINEAR, 0x0300_0000_0060_6014, 0x1234],
        )])],
    };
    let chosen = negotiate(&request, &offers(), 0).expect("tier 0");
    assert_eq!(chosen.tier, 0);
    assert_eq!(chosen.modifiers, [MOD_LINEAR, 0x0300_0000_0060_6014]);
}

#[test]
fn the_implicit_modifier_needs_to_be_listed() {
    let mut offers = offers();
    offers[0].modifiers = vec![MOD_INVALID];
    let any = FormatRequest {
        tiers: vec![FormatTier::new(vec![FormatChoice::dmabuf(
            PixelFormat::Bgrx8,
            Vec::new(),
        )])],
    };
    assert!(negotiate(&any, &offers, 0).is_none());
    let listed = FormatRequest {
        tiers: vec![FormatTier::new(vec![FormatChoice::dmabuf(
            PixelFormat::Bgrx8,
            vec![MOD_INVALID],
        )])],
    };
    assert_eq!(
        negotiate(&listed, &offers, 0).expect("listed").modifiers,
        [MOD_INVALID]
    );
}

#[test]
fn a_cpu_fallback_exists_only_when_listed() {
    let gpu_only = FormatRequest {
        tiers: vec![FormatTier::new(vec![FormatChoice::dmabuf(
            PixelFormat::Bgrx8,
            vec![0x99],
        )])],
    };
    assert!(negotiate(&gpu_only, &offers(), 0).is_none());
    let mut with_cpu = gpu_only;
    with_cpu.tiers.push(FormatTier::cpu_rgb());
    let chosen = negotiate(&with_cpu, &offers(), 0).expect("cpu tier");
    assert_eq!((chosen.tier, chosen.memory), (1, BufferMemory::Cpu));
    assert_eq!(chosen.copies, [CopyStage::ShmTransport]);
}

#[test]
fn a_tier_without_the_copy_allowed_rejects_shm() {
    let request = FormatRequest {
        tiers: vec![FormatTier::new(vec![FormatChoice::cpu(PixelFormat::Bgrx8)])],
    };
    assert!(negotiate(&request, &offers(), 0).is_none());
}

#[test]
fn downgrades_go_only_to_listed_tiers() {
    let request = FormatRequest {
        tiers: vec![
            FormatTier::new(vec![FormatChoice::dmabuf(PixelFormat::Bgrx8, Vec::new())]),
            FormatTier::cpu_rgb(),
        ],
    };
    let (chosen, downgrade) =
        downgrade(&request, &offers(), 0, DowngradeReason::ImportRejected).expect("tier 1");
    assert_eq!(chosen.memory, BufferMemory::Cpu);
    assert_eq!((downgrade.from_tier, downgrade.to_tier), (0, 1));
    assert_eq!(
        super::downgrade(&request, &offers(), 1, DowngradeReason::AllocationFailed),
        Err(CaptureError::FormatLost)
    );
}

#[test]
fn an_empty_request_takes_the_first_copy_free_offer() {
    let chosen = negotiate(&FormatRequest::default(), &offers(), 0).expect("native");
    assert_eq!(chosen.memory, BufferMemory::DmaBuf);
    assert!(negotiate(&FormatRequest::default(), &offers(), 1).is_none());
}

#[test]
fn compatibility_puts_the_portal_first() {
    let order = SessionKind::Wayland.linux_order();
    let selection = BackendSelection::Auto(AutoSelect {
        prefer: AutoPreference::Compatibility,
        exclude: Vec::new(),
    });
    let choice = choose(
        &selection,
        order,
        None,
        &wayland(),
        &CapturePolicy::default(),
        &Requirements::default(),
    )
    .expect("a backend");
    assert_eq!(choice.backends[0], BackendId::LinuxPortal);
}

#[test]
fn cursor_falls_back_to_embedded_then_hidden() {
    let metadata = CursorRequest {
        mode: CursorMode::Metadata,
        strength: Strength::Prefer,
    };
    assert_eq!(
        cursor_mode(metadata, &[CursorMode::Embedded, CursorMode::Hidden]),
        CursorMode::Embedded
    );
    assert_eq!(
        cursor_mode(metadata, &[CursorMode::Hidden]),
        CursorMode::Hidden
    );
}
