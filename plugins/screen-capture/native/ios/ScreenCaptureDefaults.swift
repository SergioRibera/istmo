import Foundation

/// The Rust `Default` values of the screen-capture configuration types.
/// Generated structs have no defaults, so Swift callers start here.
public enum ScreenCaptureDefaults {
    public static func cursor() -> ScreenCapture.CursorRequest {
        ScreenCapture.CursorRequest(mode: .embedded, strength: .prefer)
    }

    public static func consent(allowPicker: Bool = false) -> ScreenCapture.ConsentConfig {
        ScreenCapture.ConsentConfig(allowPicker: allowPicker, restoreToken: nil, persist: .doNot, grant: nil)
    }

    public static func composite() -> ScreenCapture.CompositeConfig {
        ScreenCapture.CompositeConfig(scale: .highestScale, fill: .transparent, engine: .auto)
    }

    public static func backend() -> ScreenCapture.BackendSelection {
        .auto(ScreenCapture.AutoSelect(prefer: .zeroCopy, exclude: []))
    }

    public static func video() -> ScreenCapture.VideoConfig {
        ScreenCapture.VideoConfig(
            formats: ScreenCapture.FormatRequest(tiers: []),
            fps: .max(60),
            size: .native,
            cursor: cursor(),
            damage: .ignore,
            hdr: .sdr,
            poolDepth: 0,
            exhaustion: .stall,
            gpu: .auto,
            orientation: .inMeta,
            exclude: [],
            excludeOwnProcess: false,
            liveBuffers: false
        )
    }

    /// Primary monitor, `Auto(ZeroCopy)`, `Max(60)`, no picker.
    public static func session(source: ScreenCapture.SourceSelector = .primaryMonitor) -> ScreenCapture.SessionConfig {
        ScreenCapture.SessionConfig(
            source: source,
            backend: backend(),
            video: video(),
            audio: nil,
            consent: consent(),
            composite: composite(),
            linux: ScreenCapture.LinuxOptions(pipewire: .portalRemote),
            windows: ScreenCapture.WindowsOptions(borderless: .off)
        )
    }

    /// Whatever the person picks; `allowPicker` is set.
    public static func picker() -> ScreenCapture.SessionConfig {
        ScreenCapture.SessionConfig(
            source: .picker(ScreenCapture.PickerOptions(kinds: [], multiple: false)),
            backend: backend(),
            video: video(),
            audio: nil,
            consent: consent(allowPicker: true),
            composite: composite(),
            linux: ScreenCapture.LinuxOptions(pipewire: .portalRemote),
            windows: ScreenCapture.WindowsOptions(borderless: .off)
        )
    }

    public static func screenshot(source: ScreenCapture.SourceSelector = .primaryMonitor) -> ScreenCapture.ScreenshotRequest {
        ScreenCapture.ScreenshotRequest(
            source: source,
            backend: backend(),
            formats: ScreenCapture.FormatRequest(tiers: []),
            cursor: cursor(),
            hdr: .sdr,
            composite: true,
            compositeConfig: composite(),
            consent: consent()
        )
    }

    /// Tight, upright RGBA.
    public static func pixelLayout() -> ScreenCapture.PixelLayout {
        ScreenCapture.PixelLayout(format: .rgba8, upright: true, tight: true)
    }
}
