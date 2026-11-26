package dev.istmo.plugins.screencapture

/**
 * The Rust `Default` values of the screen-capture configuration types.
 * Generated data classes have no defaults, so Kotlin callers start
 * here and `copy(...)` what they change.
 */
object ScreenCaptureDefaults {
    fun cursor(): CursorRequest = CursorRequest(CursorMode.Embedded, Strength.Prefer)

    fun consent(): ConsentConfig = ConsentConfig(
        allowPicker = false,
        restoreToken = null,
        persist = PersistMode.DoNot,
        grant = null,
    )

    fun composite(): CompositeConfig =
        CompositeConfig(CompositeScale.HighestScale, CompositeFill.Transparent, CompositeEngine.Auto)

    fun backend(): BackendSelection = BackendSelection.Auto(AutoSelect(AutoPreference.ZeroCopy, emptyList()))

    fun video(): VideoConfig = VideoConfig(
        formats = FormatRequest(emptyList()),
        fps = FrameRate.Max(60u),
        size = OutputSize.Native,
        cursor = cursor(),
        damage = Strength.Ignore,
        hdr = HdrPolicy.Sdr,
        poolDepth = 0u,
        exhaustion = Exhaustion.Stall,
        gpu = GpuPreference.Auto,
        orientation = OrientationHandling.InMeta,
        exclude = emptyList(),
        excludeOwnProcess = false,
        liveBuffers = false,
    )

    /** Primary monitor, `Auto(ZeroCopy)`, `Max(60)`, no picker. */
    fun session(source: SourceSelector = SourceSelector.PrimaryMonitor): SessionConfig = SessionConfig(
        source = source,
        backend = backend(),
        video = video(),
        audio = null,
        consent = consent(),
        composite = composite(),
        linux = LinuxOptions(PipeWireConnection.PortalRemote),
        windows = WindowsOptions(Borderless.Off),
    )

    /** Whatever the person picks; `allowPicker` is set. */
    fun picker(): SessionConfig =
        session(SourceSelector.Picker(PickerOptions(emptyList(), false))).copy(consent = consent().copy(allowPicker = true))

    fun screenshot(source: SourceSelector = SourceSelector.PrimaryMonitor): ScreenshotRequest = ScreenshotRequest(
        source = source,
        backend = backend(),
        formats = FormatRequest(emptyList()),
        cursor = cursor(),
        hdr = HdrPolicy.Sdr,
        composite = true,
        compositeConfig = composite(),
        consent = consent(),
    )

    /** Tight, upright RGBA. */
    fun pixelLayout(): PixelLayout = PixelLayout(CpuPixelFormat.Rgba8, upright = true, tight = true)
}
