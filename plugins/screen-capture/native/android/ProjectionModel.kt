package dev.istmo.plugins.screencapture

import android.content.Context
import android.hardware.display.DisplayManager
import android.os.Build
import android.util.DisplayMetrics
import android.view.Display
import dev.istmo.runtime.BackendException

/** The one backend this platform compiles. */
internal val PROJECTION: BackendId = BackendId.AndroidProjection

/** API levels the backend branches on. */
internal object Api {
    /** `Display.getMode`. */
    const val M = 23

    /** `Notification.Builder(Context, String)`, notification channels, `startForegroundService`. */
    const val O = 26

    /** `Image.getHardwareBuffer`. */
    const val P = 28

    /** `ImageReader.newInstance(.., usage)`, playback capture, typed foreground services. */
    const val Q = 29

    /** `HardwareBuffer.getId`. */
    const val S = 31

    /** `POST_NOTIFICATIONS`, `Image.getFence`. */
    const val TIRAMISU = 33

    /** `MediaProjectionConfig`, app sharing, single-use consent, content resize / visibility callbacks. */
    const val U = 34
}

internal fun fail(error: CaptureError): Nothing = throw BackendException(error)

/** Wall clock in UNIX-epoch microseconds, and the conversion of `CLOCK_MONOTONIC` stamps to it. */
internal object CaptureClock {
    fun wallNowUs(): Long =
        if (Build.VERSION.SDK_INT >= Api.O) {
            val now = java.time.Instant.now()
            now.epochSecond * 1_000_000L + now.nano / 1_000L
        } else {
            System.currentTimeMillis() * 1_000L
        }

    /**
     * [monotonicNs] (`System.nanoTime`, `Image.getTimestamp`) on the wall
     * clock, both clocks sampled now: a wall-clock step or a suspend shows
     * up on the next frame instead of drifting.
     */
    fun wallUsOf(monotonicNs: Long): Long {
        val wall = wallNowUs()
        val mono = System.nanoTime()
        return wall - (mono - monotonicNs) / 1_000L
    }
}

/** One display as the backend reports it. */
internal class DisplayFacts(
    val displayId: Int,
    val name: String,
    /** Pixels, in the current rotation. */
    val width: Int,
    val height: Int,
    val densityDpi: Int,
    val refreshHz: Float,
    val rotation: Rotation,
    val secure: Boolean,
) {
    val sourceId: SourceId get() = SourceId(PROJECTION, "display:$displayId")

    val scalePermille: UInt get() = (densityDpi.coerceAtLeast(1) * 1000 / DisplayMetrics.DENSITY_DEFAULT).toUInt()

    val pixels: Size get() = Size(width.toUInt(), height.toUInt())

    /** Logical (dp) size, the space `RegionSpace` rects use. */
    val logical: Size
        get() = Size(toLogical(width).toUInt(), toLogical(height).toUInt())

    fun toLogical(px: Int): Int = (px.toLong() * 1000 / scalePermille.toLong()).toInt()

    fun toPixels(dp: Int): Int = (dp.toLong() * scalePermille.toLong() / 1000).toInt()

    fun source(): Source = Source(
        id = sourceId,
        kind = SourceKind.Monitor,
        name = name,
        appId = null,
        pid = null,
        geometry = Rect(0, 0, logical.width, logical.height),
        physicalSize = pixels,
        scalePermille = scalePermille,
        refreshMhz = (refreshHz * 1000f).toUInt(),
        orientation = Orientation(false, rotation),
        flags = SourceFlags(
            primary = displayId == Display.DEFAULT_DISPLAY,
            minimised = false,
            occluded = false,
            protected = secure,
            ownProcess = false,
        ),
        make = null,
        model = null,
        connector = null,
        description = null,
        // MediaProjection mirrors the default display only; others are
        // listed for completeness and nothing captures them.
        capture = if (displayId == Display.DEFAULT_DISPLAY) SourceCapture.ById else SourceCapture.PickerOnly(emptyList()),
    )
}

/** The device's displays, through `DisplayManager`. */
internal class Displays(context: Context) {
    val manager: DisplayManager = context.getSystemService(Context.DISPLAY_SERVICE) as DisplayManager

    fun all(): List<DisplayFacts> = manager.displays.map(::facts)

    fun primary(): DisplayFacts {
        val display = manager.getDisplay(Display.DEFAULT_DISPLAY)
            ?: fail(CaptureError.SourceLost)
        return facts(display)
    }

    private fun facts(display: Display): DisplayFacts {
        val metrics = DisplayMetrics()
        // `getRealMetrics` is deprecated for window sizing, not for the
        // display's own size in its current rotation, which is what a
        // virtual display mirroring it needs.
        @Suppress("DEPRECATION")
        display.getRealMetrics(metrics)
        return DisplayFacts(
            displayId = display.displayId,
            name = display.name ?: "Display ${display.displayId}",
            width = metrics.widthPixels,
            height = metrics.heightPixels,
            densityDpi = metrics.densityDpi,
            refreshHz = display.refreshRate,
            rotation = when (display.rotation) {
                android.view.Surface.ROTATION_90 -> Rotation.R90
                android.view.Surface.ROTATION_180 -> Rotation.R180
                android.view.Surface.ROTATION_270 -> Rotation.R270
                else -> Rotation.R0
            },
            secure = (display.flags and Display.FLAG_SECURE) != 0,
        )
    }
}

/** A way to deliver frames: what the reader allocates and what it costs. */
internal enum class Offer(val format: PixelFormat, val memory: BufferMemory, val copies: List<CopyStage>) {
    /** `RGBA_8888` `HardwareBuffer`s, handed over without a copy (API 29+). */
    Gpu(PixelFormat.Rgba8, BufferMemory.HardwareBuffer, emptyList()),

    /** `RGBA_8888` CPU-readable images the GPU composed, read back into Rust memory. */
    Cpu(PixelFormat.Rgba8, BufferMemory.Cpu, listOf(CopyStage.GpuReadback)),
    ;

    companion object {
        fun available(): List<Offer> = if (Build.VERSION.SDK_INT >= Api.Q) listOf(Gpu, Cpu) else listOf(Cpu)
    }
}

/** The offer a tier list settles on, and the tier's index. */
internal class Negotiated(val offer: Offer, val tier: Int)

/**
 * The first tier (in order) with a choice an offer meets, the copies it
 * makes being among those the tier allows. An empty request takes the
 * native offer, or the CPU one first when [cpuFirst] (screenshots).
 */
internal fun negotiate(request: FormatRequest, cpuFirst: Boolean): Negotiated? {
    val offers = Offer.available()
    if (request.tiers.isEmpty()) {
        return Negotiated(if (cpuFirst) Offer.Cpu else offers.first(), 0)
    }
    request.tiers.forEachIndexed { index, tier ->
        for (choice in tier.choices) {
            val offer = offers.firstOrNull { offer ->
                offer.format == choice.format && offer.memory == choice.memory &&
                    tier.allowsCopy.containsAll(offer.copies)
            }
            if (offer != null) return Negotiated(offer, index)
        }
    }
    return null
}

/** Feature name of a backend id (`BackendId::as_str` in Rust). */
internal fun BackendId.feature(): String = when (this) {
    BackendId.LinuxPortal -> "linux-portal"
    BackendId.LinuxExtImageCopy -> "linux-ext-image-copy"
    BackendId.LinuxWlrScreencopy -> "linux-wlr-screencopy"
    BackendId.LinuxX11 -> "linux-x11"
    BackendId.LinuxKms -> "linux-kms"
    BackendId.WindowsWgc -> "windows-wgc"
    BackendId.WindowsDxgi -> "windows-dxgi"
    BackendId.WindowsGdi -> "windows-gdi"
    BackendId.MacosSck -> "macos-sck"
    BackendId.IosSck -> "ios-sck"
    BackendId.IosReplayKit -> "ios-replaykit"
    BackendId.AndroidProjection -> "android-projection"
    BackendId.Web -> "web"
}

/**
 * Backend selection on a platform with one backend: refuses a selection
 * that leaves it out, and turns unmet requirements into the error the
 * selection asks for (`Unsatisfiable` for `Fixed`, `NoBackend` otherwise).
 */
internal class Selection(private val selection: BackendSelection) {
    init {
        when (selection) {
            is BackendSelection.Auto ->
                if (PROJECTION in selection.value.exclude) fail(CaptureError.NoBackend(emptyList()))
            is BackendSelection.Fixed ->
                if (selection.value != PROJECTION) fail(CaptureError.BackendNotCompiled(selection.value))
            is BackendSelection.Ordered ->
                if (PROJECTION !in selection.value) fail(CaptureError.NoBackend(selection.value.map(::notCompiled)))
        }
    }

    fun unsatisfiable(vararg unmet: Requirement): Nothing = unsatisfiable(unmet.toList())

    fun unsatisfiable(unmet: List<Requirement>): Nothing = when (selection) {
        is BackendSelection.Fixed -> fail(CaptureError.Unsatisfiable(Unmet(PROJECTION, unmet)))
        is BackendSelection.Auto ->
            fail(CaptureError.NoBackend(listOf(BackendAttempt(PROJECTION, AttemptFailure.Unsatisfiable(unmet)))))
        is BackendSelection.Ordered -> fail(
            CaptureError.NoBackend(
                selection.value.map { id ->
                    if (id == PROJECTION) BackendAttempt(id, AttemptFailure.Unsatisfiable(unmet)) else notCompiled(id)
                },
            ),
        )
    }

    private fun notCompiled(id: BackendId) =
        BackendAttempt(id, AttemptFailure.Unavailable(Availability.NotCompiled(id.feature())))
}

/** Constant parts of every stream description. */
internal val SRGB = Colour(ColourSpace.Srgb, TransferFunction.Srgb, ColourRange.Full)
internal val UPRIGHT = Orientation(false, Rotation.R0)
