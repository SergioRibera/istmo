//! `ScreenCaptureDefaults` in `native/android` and `native/ios` restate
//! the Rust `Default` values (generated Kotlin / Swift types have none).
//! This pins every restated value to the Rust one, so changing a default
//! in Rust without the native helpers fails here.

use std::fmt::Debug;

use istmo_screen_capture::{
    AutoSelect, BackendSelection, CompositeConfig, ConsentConfig, CursorRequest, FrameRate,
    LinuxOptions, PixelLayout, ScreenshotRequest, SessionConfig, VideoConfig, WindowsOptions,
};

fn native(path: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(path);
    std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
}

/// A unit variant as Rust's `Debug` prints it.
fn variant(value: &impl Debug) -> String {
    format!("{value:?}")
}

/// `Type.Variant`, Kotlin's spelling of a unit variant.
fn kt(ty: &str, value: &impl Debug) -> String {
    format!("{ty}.{}", variant(value))
}

/// `.variant`, Swift's spelling of a unit variant.
fn sw(value: &impl Debug) -> String {
    let name = variant(value);
    let mut chars = name.chars();
    chars.next().map_or_else(String::new, |first| {
        format!(".{}{}", first.to_lowercase(), chars.as_str())
    })
}

fn frame_rate_kt(fps: FrameRate) -> String {
    match fps {
        FrameRate::Native => "FrameRate.Native".to_owned(),
        FrameRate::Max(n) => format!("FrameRate.Max({n}u)"),
        FrameRate::Exact(n) => format!("FrameRate.Exact({n}u)"),
        FrameRate::OnDamage(n) => format!("FrameRate.OnDamage({n}u)"),
    }
}

fn frame_rate_sw(fps: FrameRate) -> String {
    match fps {
        FrameRate::Native => ".native".to_owned(),
        FrameRate::Max(n) => format!(".max({n})"),
        FrameRate::Exact(n) => format!(".exact({n})"),
        FrameRate::OnDamage(n) => format!(".onDamage({n})"),
    }
}

fn contains(file: &str, text: &str, expected: &str) {
    assert!(
        text.contains(expected),
        "{file} must restate the Rust default as `{expected}`"
    );
}

#[test]
fn kotlin_defaults_match_rust() {
    let file = "native/android/ScreenCaptureDefaults.kt";
    let kotlin = native(file);
    let video = VideoConfig::default();
    let cursor = CursorRequest::default();
    let consent = ConsentConfig::default();
    let composite = CompositeConfig::default();
    let layout = PixelLayout::default();
    let expected = [
        format!(
            "CursorRequest({}, {})",
            kt("CursorMode", &cursor.mode),
            kt("Strength", &cursor.strength)
        ),
        format!("allowPicker = {}", consent.allow_picker),
        format!("persist = {}", kt("PersistMode", &consent.persist)),
        format!(
            "CompositeConfig({}, {}, {})",
            kt("CompositeScale", &composite.scale),
            kt("CompositeFill", &composite.fill),
            kt("CompositeEngine", &composite.engine)
        ),
        format!("fps = {}", frame_rate_kt(video.fps)),
        format!("size = {}", kt("OutputSize", &video.size)),
        format!("damage = {}", kt("Strength", &video.damage)),
        format!("hdr = {}", kt("HdrPolicy", &video.hdr)),
        format!("poolDepth = {}u", video.pool_depth),
        format!("exhaustion = {}", kt("Exhaustion", &video.exhaustion)),
        format!("gpu = {}", kt("GpuPreference", &video.gpu)),
        format!(
            "orientation = {}",
            kt("OrientationHandling", &video.orientation)
        ),
        format!("excludeOwnProcess = {}", video.exclude_own_process),
        format!("liveBuffers = {}", video.live_buffers),
        format!(
            "LinuxOptions({})",
            kt("PipeWireConnection", &LinuxOptions::default().pipewire)
        ),
        format!(
            "WindowsOptions({})",
            kt("Borderless", &WindowsOptions::default().borderless)
        ),
        format!("composite = {}", ScreenshotRequest::default().composite),
        format!(
            "PixelLayout({}, upright = {}, tight = {})",
            kt("CpuPixelFormat", &layout.format),
            layout.upright,
            layout.tight
        ),
    ];
    for text in &expected {
        contains(file, &kotlin, text);
    }
    assert_eq!(video.formats.tiers.len() + video.exclude.len(), 0);
    contains(file, &kotlin, "formats = FormatRequest(emptyList())");
    let BackendSelection::Auto(AutoSelect { prefer, exclude }) = BackendSelection::default() else {
        panic!("the default selection is Auto");
    };
    assert_eq!(exclude.len(), 0);
    contains(
        file,
        &kotlin,
        &format!(
            "BackendSelection.Auto(AutoSelect({}, emptyList()))",
            kt("AutoPreference", &prefer)
        ),
    );
    assert_eq!(
        SessionConfig::default().source,
        istmo_screen_capture::SourceSelector::PrimaryMonitor
    );
    contains(
        file,
        &kotlin,
        "SourceSelector = SourceSelector.PrimaryMonitor",
    );
}

#[test]
fn swift_defaults_match_rust() {
    let file = "native/ios/ScreenCaptureDefaults.swift";
    let swift = native(file);
    let video = VideoConfig::default();
    let cursor = CursorRequest::default();
    let consent = ConsentConfig::default();
    let composite = CompositeConfig::default();
    let layout = PixelLayout::default();
    let expected = [
        format!(
            "ScreenCapture.CursorRequest(mode: {}, strength: {})",
            sw(&cursor.mode),
            sw(&cursor.strength)
        ),
        format!("allowPicker: Bool = {}", consent.allow_picker),
        format!("persist: {}", sw(&consent.persist)),
        format!(
            "ScreenCapture.CompositeConfig(scale: {}, fill: {}, engine: {})",
            sw(&composite.scale),
            sw(&composite.fill),
            sw(&composite.engine)
        ),
        format!("fps: {}", frame_rate_sw(video.fps)),
        format!("size: {}", sw(&video.size)),
        format!("damage: {}", sw(&video.damage)),
        format!("hdr: {}", sw(&video.hdr)),
        format!("poolDepth: {}", video.pool_depth),
        format!("exhaustion: {}", sw(&video.exhaustion)),
        format!("gpu: {}", sw(&video.gpu)),
        format!("orientation: {}", sw(&video.orientation)),
        format!("excludeOwnProcess: {}", video.exclude_own_process),
        format!("liveBuffers: {}", video.live_buffers),
        format!(
            "ScreenCapture.LinuxOptions(pipewire: {})",
            sw(&LinuxOptions::default().pipewire)
        ),
        format!(
            "ScreenCapture.WindowsOptions(borderless: {})",
            sw(&WindowsOptions::default().borderless)
        ),
        format!("composite: {}", ScreenshotRequest::default().composite),
        format!(
            "ScreenCapture.PixelLayout(format: {}, upright: {}, tight: {})",
            sw(&layout.format),
            layout.upright,
            layout.tight
        ),
    ];
    for text in &expected {
        contains(file, &swift, text);
    }
    contains(
        file,
        &swift,
        "formats: ScreenCapture.FormatRequest(tiers: [])",
    );
    let BackendSelection::Auto(AutoSelect { prefer, .. }) = BackendSelection::default() else {
        panic!("the default selection is Auto");
    };
    contains(
        file,
        &swift,
        &format!(
            ".auto(ScreenCapture.AutoSelect(prefer: {}, exclude: []))",
            sw(&prefer)
        ),
    );
    contains(
        file,
        &swift,
        "source: ScreenCapture.SourceSelector = .primaryMonitor",
    );
}
