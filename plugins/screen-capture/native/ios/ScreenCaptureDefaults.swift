import Foundation

/// The Rust `Default` values of the screen-capture configuration types.
/// Generated structs have no defaults, so Swift callers start here.
public enum ScreenCaptureDefaults {
    public static func cursor() -> CursorRequest {
        CursorRequest(mode: .embedded, strength: .prefer)
    }

    public static func consent(allowPicker: Bool = false) -> ConsentConfig {
        ConsentConfig(allowPicker: allowPicker, restoreToken: nil, persist: .doNot, grant: nil)
    }

    public static func composite() -> CompositeConfig {
        CompositeConfig(scale: .highestScale, fill: .transparent, engine: .auto)
    }

    public static func backend() -> BackendSelection {
        .auto(AutoSelect(prefer: .zeroCopy, exclude: []))
    }

    public static func video() -> VideoConfig {
        VideoConfig(
            formats: FormatRequest(tiers: []),
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
    public static func session(source: SourceSelector = .primaryMonitor) -> SessionConfig {
        SessionConfig(
            source: source,
            backend: backend(),
            video: video(),
            audio: nil,
            consent: consent(),
            composite: composite(),
            linux: LinuxOptions(pipewire: .portalRemote),
            windows: WindowsOptions(borderless: .off)
        )
    }

    /// Whatever the person picks; `allowPicker` is set.
    public static func picker() -> SessionConfig {
        SessionConfig(
            source: .picker(PickerOptions(kinds: [], multiple: false)),
            backend: backend(),
            video: video(),
            audio: nil,
            consent: consent(allowPicker: true),
            composite: composite(),
            linux: LinuxOptions(pipewire: .portalRemote),
            windows: WindowsOptions(borderless: .off)
        )
    }

    public static func screenshot(source: SourceSelector = .primaryMonitor) -> ScreenshotRequest {
        ScreenshotRequest(
            source: source,
            backend: backend(),
            formats: FormatRequest(tiers: []),
            cursor: cursor(),
            hdr: .sdr,
            composite: true,
            compositeConfig: composite(),
            consent: consent()
        )
    }

    /// Tight, upright RGBA.
    public static func pixelLayout() -> PixelLayout {
        PixelLayout(format: .rgba8, upright: true, tight: true)
    }
}
