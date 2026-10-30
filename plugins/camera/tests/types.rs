//! Round-trip + API-surface checks for `istmo-camera`.
//!
//! These tests prove that:
//! - every wire type encodes / decodes through bincode without loss,
//! - the macro-generated plugin surface compiles with the expected
//!   method signatures (compile-only `const _` assertions),
//! - error decode via `From<IstmoError>` recovers the typed variant.

use istmo_camera::{
    AccessStatus, AppliedControls, CAMERA_PLUGIN_ID, Camera, CameraCapabilities, CameraControls,
    CameraDevice, CameraError, CameraFacing, CameraFrame, CameraInitConfig, CameraSession,
    ConcurrentPair, ConcurrentSupport, FlashMode, FocusMode, FramePlane, LensKind,
    NegotiatedFormat, PixelFormat, Resolution, SessionConfig, StillCaptureRequest, StillPhoto,
    SupportedFormat, WhiteBalanceMode,
};
use istmo_core::IstmoError;

fn roundtrip<T>(value: &T) -> T
where
    T: bincode::Encode + bincode::Decode<()> + PartialEq + std::fmt::Debug,
{
    let encoded = bincode::encode_to_vec(value, bincode::config::standard()).expect("encode");
    let (decoded, _) =
        bincode::decode_from_slice::<T, _>(&encoded, bincode::config::standard()).expect("decode");
    assert_eq!(value, &decoded, "round-trip failed");
    decoded
}

#[test]
fn plugin_id_is_stable() {
    assert_eq!(CAMERA_PLUGIN_ID, "istmo.camera");
}

#[test]
fn init_config_defaults() {
    let cfg = CameraInitConfig::default();
    assert_eq!(cfg.frame_pool_depth, 3);
}

#[test]
fn session_config_builders() {
    let cfg = SessionConfig::new("cam-0")
        .with_pixel_format(PixelFormat::Yuv420)
        .with_resolution(Resolution {
            width: 640,
            height: 480,
        })
        .with_target_fps(60)
        .preview_only();
    assert_eq!(cfg.device_id, "cam-0");
    assert_eq!(cfg.pixel_format, PixelFormat::Yuv420);
    assert_eq!(
        cfg.resolution,
        Some(Resolution {
            width: 640,
            height: 480,
        })
    );
    assert_eq!(cfg.target_fps, Some(60));
    assert!(!cfg.emit_frames);
}

#[test]
fn still_request_defaults_to_jpeg_quality_90() {
    let req = StillCaptureRequest::default();
    assert_eq!(req.pixel_format, PixelFormat::Jpeg(90));
    assert_eq!(req.flash, FlashMode::Auto);
    assert!(req.resolution.is_none());
}

#[test]
fn pixel_format_roundtrip() {
    for f in [
        PixelFormat::Nv21,
        PixelFormat::Yuv420,
        PixelFormat::Bgra8,
        PixelFormat::Rgba8,
        PixelFormat::Jpeg(0),
        PixelFormat::Jpeg(90),
        PixelFormat::Jpeg(100),
    ] {
        roundtrip(&f);
    }
}

#[test]
fn focus_mode_roundtrip() {
    for m in [
        FocusMode::Auto,
        FocusMode::ContinuousVideo,
        FocusMode::ContinuousPicture,
        FocusMode::Fixed,
        FocusMode::Manual(0.42),
    ] {
        roundtrip(&m);
    }
}

#[test]
fn camera_device_roundtrip() {
    let dev = CameraDevice {
        id: "0".into(),
        label: "Back Dual Wide Camera".into(),
        facing: CameraFacing::Back,
        lens: LensKind::Logical,
        physical_sensors: vec!["1".into(), "2".into()],
        capabilities: CameraCapabilities {
            min_zoom: 0.5,
            max_zoom: 10.0,
            optical_zoom_steps: vec![0.5, 1.0, 2.0],
            focal_lengths_mm: vec![13.0, 26.0, 52.0],
            supports_flash: true,
            supports_torch: true,
            supports_autofocus: true,
            supports_manual_focus: true,
            supports_ois: true,
            supports_eis: true,
            supports_hdr: true,
            supports_depth: false,
            supports_raw: true,
            max_still_resolution: Resolution {
                width: 4032,
                height: 3024,
            },
        },
        supported_formats: vec![SupportedFormat {
            resolution: Resolution {
                width: 1920,
                height: 1080,
            },
            pixel_format: PixelFormat::Yuv420,
            min_fps: 30,
            max_fps: 60,
        }],
    };
    roundtrip(&dev);
}

#[test]
fn session_wire_roundtrip() {
    let session = CameraSession {
        session: istmo_core::NativeHandleId::new(7),
        negotiated: NegotiatedFormat {
            resolution: Resolution {
                width: 1280,
                height: 720,
            },
            pixel_format: PixelFormat::Nv21,
            fps: 30,
            rotation_degrees: 90,
        },
    };
    let decoded = roundtrip(&session);
    assert_eq!(decoded.session.get(), 7);
}

#[test]
fn frame_roundtrip() {
    let frame = CameraFrame {
        seq: 42,
        timestamp_ns: 1_234_567_890,
        resolution: Resolution {
            width: 640,
            height: 480,
        },
        rotation_degrees: 90,
        pixel_format: PixelFormat::Nv21,
        planes: vec![FramePlane {
            offset: 0,
            row_stride: 640,
            pixel_stride: 1,
            width: 640,
            height: 480,
        }],
        bytes: vec![0xAB; 640 * 480 * 3 / 2],
        dropped_before: 2,
    };
    let decoded = roundtrip(&frame);
    assert_eq!(decoded.bytes.len(), 640 * 480 * 3 / 2);
}

#[test]
fn concurrent_support_roundtrip() {
    let s = ConcurrentSupport {
        max_concurrent: 2,
        approved_pairs: vec![ConcurrentPair {
            primary: "0".into(),
            secondary: "1".into(),
        }],
        any_combination: false,
    };
    roundtrip(&s);
}

#[test]
fn controls_roundtrip_with_partial_fields() {
    let partial = CameraControls {
        zoom: Some(1.5),
        torch: None,
        focus: Some(FocusMode::Manual(0.1)),
        exposure_compensation_ev: None,
        white_balance: Some(WhiteBalanceMode::Shade),
        flash: None,
    };
    roundtrip(&partial);
    roundtrip(&CameraControls::default());

    let applied = AppliedControls {
        zoom: 2.0,
        torch: true,
        focus: FocusMode::ContinuousVideo,
        exposure_compensation_ev: -0.3,
        white_balance: WhiteBalanceMode::Auto,
        flash: FlashMode::Off,
    };
    roundtrip(&applied);
}

#[test]
fn still_photo_roundtrip() {
    let photo = StillPhoto {
        resolution: Resolution {
            width: 4000,
            height: 3000,
        },
        rotation_degrees: 0,
        pixel_format: PixelFormat::Jpeg(90),
        bytes: vec![0xFF, 0xD8, 0xFF, 0xE0],
        timestamp_ns: 0,
    };
    roundtrip(&photo);
}

#[test]
fn access_status_roundtrip() {
    for s in [
        AccessStatus::Granted,
        AccessStatus::Denied,
        AccessStatus::NotDetermined,
        AccessStatus::Restricted,
    ] {
        roundtrip(&s);
    }
}

#[test]
fn error_decode_from_plugin_bytes() {
    let original = CameraError::DeviceNotFound("cam-7".into());
    let payload =
        bincode::encode_to_vec(&original, bincode::config::standard()).expect("encode error");
    let wire = IstmoError::PluginError { bytes: payload };
    let decoded: CameraError = CameraError::from(wire);
    assert_eq!(decoded, original);
}

#[test]
fn error_display_messages() {
    assert!(
        CameraError::PermissionDenied
            .to_string()
            .contains("permission")
    );
    assert!(
        CameraError::DeviceNotFound("abc".into())
            .to_string()
            .contains("abc")
    );
    assert!(
        CameraError::Backend("hw fault".into())
            .to_string()
            .contains("hw fault")
    );
}

/// Compile-time check that [`Camera`] carries the surface this plugin
/// is contracted to expose. If a method name or sig drifts, this
/// stops compiling.
#[allow(dead_code)]
fn _assert_trait_surface<T: Camera>(t: &T) {
    use istmo_core::NativeHandleId;

    let id = NativeHandleId::new(0);
    let _d: &dyn Fn() -> _ = &|| async {
        let _: Result<Vec<CameraDevice>, CameraError> = t.devices().await;
    };
    let _c: &dyn Fn() -> _ = &|| async {
        let _: Result<ConcurrentSupport, CameraError> = t.concurrent_support().await;
    };
    let _a: &dyn Fn() -> _ = &|| async {
        let _: Result<AccessStatus, CameraError> = t.access_status().await;
        let _: Result<AccessStatus, CameraError> = t.request_access().await;
    };
    let _o: &dyn Fn() -> _ = &|| async {
        let _: Result<CameraSession, CameraError> = t.open(SessionConfig::new("x")).await;
    };
    let _f: &dyn Fn() -> _ = &|| {
        let _: flume::Receiver<CameraFrame> = t.frames(id);
    };
    let _ac: &dyn Fn() -> _ = &|| async {
        let _: Result<AppliedControls, CameraError> =
            t.apply_controls(id, CameraControls::default()).await;
    };
    let _s: &dyn Fn() -> _ = &|| async {
        let _: Result<StillPhoto, CameraError> =
            t.capture_still(id, StillCaptureRequest::default()).await;
    };
    let _close: &dyn Fn() -> _ = &|| async {
        let _: Result<(), CameraError> = t.close(id).await;
    };
}
