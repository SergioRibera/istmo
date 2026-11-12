package dev.istmo.plugins.camera

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.ImageFormat
import android.graphics.Rect
import android.hardware.camera2.CameraCharacteristics
import android.hardware.camera2.CameraManager
import android.hardware.camera2.CaptureRequest
import android.os.Build
import android.util.Range
import android.util.Size
import androidx.activity.ComponentActivity
import androidx.activity.result.contract.ActivityResultContracts
import androidx.camera.camera2.interop.Camera2CameraInfo
import androidx.camera.camera2.interop.Camera2Interop
import androidx.camera.core.CameraSelector
import androidx.camera.core.ImageAnalysis
import androidx.camera.core.ImageCapture
import androidx.camera.core.ImageCaptureException
import androidx.camera.core.ImageProxy
import androidx.camera.core.Preview
import androidx.camera.core.TorchState
import androidx.camera.core.resolutionselector.ResolutionSelector
import androidx.camera.core.resolutionselector.ResolutionStrategy
import androidx.camera.lifecycle.ProcessCameraProvider
import androidx.core.content.ContextCompat
import dev.istmo.runtime.*
import java.io.ByteArrayOutputStream
import java.lang.ref.WeakReference
import java.nio.ByteBuffer
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicLong
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.channels.BufferOverflow
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.flow
import kotlinx.coroutines.launch
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.withContext

/**
 * CameraX-backed implementation of [CameraBackend]. Enumerates every
 * camera the OS exposes, drives one or more concurrent sessions, and
 * fans analyzer frames out to every [frames] subscriber through a
 * per-subscriber channel sized by [CameraInitConfig.framePoolDepth].
 *
 * The host activity is held weakly: the registry builds a fresh
 * backend on every activity `onCreate`, and a backend outliving its
 * activity must not keep the destroyed activity alive.
 */
class CameraBackendImpl(
    activity: ComponentActivity,
    private val config: CameraInitConfig,
) : CameraBackend, HandleReleaser {

    private val activityRef = WeakReference(activity)
    private val appContext: Context = activity.applicationContext
    private val sessions = ConcurrentHashMap<Long, SessionState>()
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
    private val cameraManager by lazy {
        appContext.getSystemService(Context.CAMERA_SERVICE) as CameraManager
    }
    private val poolDepth: Int =
        if (config.framePoolDepth > 0u) config.framePoolDepth.toInt() else 3

    private fun activity(): ComponentActivity =
        activityRef.get() ?: throw BackendException(CameraError.Backend("host activity is gone"))

    // ---- Discovery -------------------------------------------------

    override suspend fun devices(): List<CameraDevice> = withContext(Dispatchers.IO) {
        cameraManager.cameraIdList.mapNotNull { id ->
            runCatching { describeDevice(id) }.getOrNull()
        }
    }

    override suspend fun concurrent_support(): ConcurrentSupport {
        val pairs: List<ConcurrentPair> =
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
                runCatching { cameraManager.concurrentCameraIds }
                    .getOrDefault(emptySet())
                    .filter { it.size >= 2 }
                    .map { ids ->
                        val list = ids.toList()
                        ConcurrentPair(primary = list[0], secondary = list[1])
                    }
            } else {
                emptyList()
            }
        return ConcurrentSupport(
            maxConcurrent = if (pairs.isEmpty()) 1u else 2u,
            approvedPairs = pairs,
            anyCombination = false,
        )
    }

    // ---- Permissions ----------------------------------------------

    override suspend fun access_status(): AccessStatus = when (
        ContextCompat.checkSelfPermission(appContext, Manifest.permission.CAMERA)
    ) {
        PackageManager.PERMISSION_GRANTED -> AccessStatus.Granted
        else -> AccessStatus.NotDetermined
    }

    override suspend fun request_access(): AccessStatus = withContext(Dispatchers.Main) {
        val activity = activity()
        suspendCancellableCoroutine { cont ->
            // Registered outside a lifecycle, so the registry keeps the
            // launcher until it is unregistered explicitly.
            lateinit var launcher: androidx.activity.result.ActivityResultLauncher<String>
            launcher = activity.activityResultRegistry.register(
                "istmo.camera.permission.${System.nanoTime()}",
                ActivityResultContracts.RequestPermission(),
            ) { granted ->
                launcher.unregister()
                if (cont.isActive) {
                    cont.resume(if (granted) AccessStatus.Granted else AccessStatus.Denied)
                }
            }
            cont.invokeOnCancellation { launcher.unregister() }
            launcher.launch(Manifest.permission.CAMERA)
        }
    }

    // ---- Session lifecycle ----------------------------------------

    override suspend fun open(config: SessionConfig): CameraSession = withContext(Dispatchers.Main) {
        if (ContextCompat.checkSelfPermission(appContext, Manifest.permission.CAMERA)
            != PackageManager.PERMISSION_GRANTED
        ) {
            throw BackendException(CameraError.PermissionDenied)
        }
        val knownIds = runCatching { cameraManager.cameraIdList.toSet() }.getOrDefault(emptySet())
        if (config.deviceId !in knownIds) {
            throw BackendException(CameraError.DeviceNotFound(config.deviceId))
        }
        val outputFormat = outputFormat(config.pixelFormat)
            ?: throw BackendException(CameraError.FormatUnsupported)
        val activity = activity()
        val provider = awaitProvider()
        val selector = CameraSelector.Builder().addCameraFilter { cams ->
            cams.filter { Camera2CameraInfo.from(it).cameraId == config.deviceId }
        }.build()

        val targetSize = config.resolution?.let { Size(it.width.toInt(), it.height.toInt()) }
        val resolutionSelector = targetSize?.let {
            ResolutionSelector.Builder()
                .setResolutionStrategy(
                    ResolutionStrategy(it, ResolutionStrategy.FALLBACK_RULE_CLOSEST_HIGHER_THEN_LOWER)
                )
                .build()
        }
        val lockedFps: UInt? = config.targetFps?.takeIf { supportsFps(config.deviceId, it.toInt()) }

        val preview = Preview.Builder().apply {
            resolutionSelector?.let { setResolutionSelector(it) }
        }.build()
        val analysis = ImageAnalysis.Builder()
            .setBackpressureStrategy(ImageAnalysis.STRATEGY_KEEP_ONLY_LATEST)
            .setOutputImageFormat(outputFormat)
            .apply {
                resolutionSelector?.let { setResolutionSelector(it) }
                lockedFps?.let { fps ->
                    Camera2Interop.Extender(this).setCaptureRequestOption(
                        CaptureRequest.CONTROL_AE_TARGET_FPS_RANGE,
                        Range(fps.toInt(), fps.toInt()),
                    )
                }
            }
            .build()
        val stills = ImageCapture.Builder()
            .setCaptureMode(ImageCapture.CAPTURE_MODE_MINIMIZE_LATENCY)
            .setJpegQuality(CAPTURE_JPEG_QUALITY)
            .build()

        val analyzer = Executors.newSingleThreadExecutor()
        val fanout = FrameFanout(poolDepth)
        if (config.emitFrames) {
            val seq = AtomicLong(0)
            analysis.setAnalyzer(analyzer) { image ->
                emitFrame(fanout, seq, image, config.pixelFormat)
            }
        }

        val camera = try {
            provider.bindToLifecycle(activity, selector, preview, analysis, stills)
        } catch (e: IllegalArgumentException) {
            // The selector matched no camera: it vanished since the check above.
            analysis.clearAnalyzer()
            analyzer.shutdown()
            throw BackendException(CameraError.DeviceNotFound(config.deviceId))
        } catch (e: Exception) {
            analysis.clearAnalyzer()
            analyzer.shutdown()
            throw BackendException(CameraError.Backend(e.message ?: e.javaClass.name))
        }

        val state = SessionState(camera, preview, analysis, stills, analyzer, provider, fanout)
        applyInitialControls(state, config.initialControls)
        val handleId: Long = IstmoRuntime.allocHandleId("istmo.camera")
        sessions[handleId] = state

        val negotiatedSize = analysis.resolutionInfo?.resolution ?: Size(0, 0)
        CameraSession(
            session = handleId,
            negotiated = NegotiatedFormat(
                resolution = Resolution(negotiatedSize.width.toUInt(), negotiatedSize.height.toUInt()),
                pixelFormat = config.pixelFormat,
                // CameraX does not report the AE range it settled on, so
                // only a locked rate is known; 0 means "device default".
                fps = lockedFps ?: 0u,
                rotationDegrees = analysis.resolutionInfo?.rotationDegrees?.toUShort() ?: 0u,
            ),
        )
    }

    override suspend fun close(session: Long) = withContext(Dispatchers.Main) {
        val state = sessions.remove(session) ?: return@withContext
        state.provider.unbind(state.preview, state.analysis, state.stills)
        state.analysis.clearAnalyzer()
        state.analyzer.shutdown()
        // Completes every subscriber's flow so the Rust stream ends.
        state.fanout.close()
        IstmoRuntime.forgetHandle(session)
    }

    override fun frames(session: Long): Flow<CameraFrame> = flow {
        val state = sessions[session] ?: throw BackendException(CameraError.SessionClosed)
        val channel = state.fanout.subscribe()
        try {
            for (frame in channel) emit(frame)
        } finally {
            state.fanout.unsubscribe(channel)
        }
    }

    override fun releaseNativeHandle(handleId: Long) {
        // Every release in the runtime reaches every releaser; ids this
        // backend never handed out are not in `sessions` and no-op.
        if (!sessions.containsKey(handleId)) return
        scope.launch { close(handleId) }
    }

    // ---- Controls --------------------------------------------------

    override suspend fun apply_controls(
        session: Long,
        controls: CameraControls,
    ): AppliedControls = withContext(Dispatchers.Main) {
        val state = sessions[session] ?: throw BackendException(CameraError.SessionClosed)
        val info = state.camera.cameraInfo
        val ctrl = state.camera.cameraControl

        controls.torch?.let { ctrl.enableTorch(it) }
        controls.zoom?.let { ctrl.setZoomRatio(it) }
        controls.exposureCompensationEv?.let {
            val step = info.exposureState.exposureCompensationStep.toFloat()
            if (step > 0f) ctrl.setExposureCompensationIndex((it / step).toInt())
        }

        AppliedControls(
            zoom = info.zoomState.value?.zoomRatio ?: 1f,
            torch = info.torchState.value == TorchState.ON,
            focus = controls.focus ?: FocusMode.ContinuousVideo,
            exposureCompensationEv = info.exposureState.exposureCompensationIndex.toFloat()
                * info.exposureState.exposureCompensationStep.toFloat(),
            whiteBalance = controls.whiteBalance ?: WhiteBalanceMode.Auto,
            flash = controls.flash ?: FlashMode.Off,
        )
    }

    // ---- Stills ----------------------------------------------------

    /**
     * JPEG stills through the session's [ImageCapture] use case. Other
     * pixel formats report [CameraError.FormatUnsupported];
     * [StillCaptureRequest.resolution] is not honoured (the capture uses
     * the use case's own resolution).
     */
    override suspend fun capture_still(
        session: Long,
        request: StillCaptureRequest,
    ): StillPhoto {
        val state = sessions[session] ?: throw BackendException(CameraError.SessionClosed)
        val quality = (request.pixelFormat as? PixelFormat.Jpeg)?.value?.toInt()
            ?: throw BackendException(CameraError.FormatUnsupported)
        state.stills.flashMode = when (request.flash) {
            FlashMode.Off -> ImageCapture.FLASH_MODE_OFF
            FlashMode.On -> ImageCapture.FLASH_MODE_ON
            FlashMode.Auto -> ImageCapture.FLASH_MODE_AUTO
        }
        val image = suspendCancellableCoroutine<ImageProxy> { cont ->
            state.stills.takePicture(
                state.analyzer,
                object : ImageCapture.OnImageCapturedCallback() {
                    override fun onCaptureSuccess(image: ImageProxy) {
                        if (cont.isActive) cont.resume(image) else image.close()
                    }

                    override fun onError(exception: ImageCaptureException) {
                        cont.resumeWithException(
                            BackendException(CameraError.Backend(exception.message ?: "capture failed"))
                        )
                    }
                },
            )
        }
        return image.use {
            if (it.format != ImageFormat.JPEG) {
                throw BackendException(CameraError.Backend("ImageCapture returned format ${it.format}"))
            }
            val buffer = it.planes[0].buffer
            val captured = ByteArray(buffer.remaining()).also { bytes -> buffer.get(bytes) }
            StillPhoto(
                resolution = Resolution(it.width.toUInt(), it.height.toUInt()),
                rotationDegrees = it.imageInfo.rotationDegrees.toUShort(),
                pixelFormat = request.pixelFormat,
                bytes = reencodeJpeg(captured, quality.coerceIn(0, 100)),
                timestampNs = it.imageInfo.timestamp.toULong(),
            )
        }
    }

    // ---- Internals -------------------------------------------------

    private class SessionState(
        val camera: androidx.camera.core.Camera,
        val preview: Preview,
        val analysis: ImageAnalysis,
        val stills: ImageCapture,
        val analyzer: ExecutorService,
        val provider: ProcessCameraProvider,
        val fanout: FrameFanout,
    )

    /**
     * One channel per [frames] collector. A slow collector loses its own
     * oldest frames without stalling the others; [close] closes every
     * channel so each collector's flow completes.
     */
    private class FrameFanout(private val depth: Int) {
        private class Subscriber(val channel: Channel<CameraFrame>, val dropped: AtomicInteger)

        private val subscribers = ArrayList<Subscriber>()
        private var closed = false

        @Synchronized
        fun subscribe(): Channel<CameraFrame> {
            val dropped = AtomicInteger(0)
            val channel = Channel<CameraFrame>(
                capacity = depth,
                onBufferOverflow = BufferOverflow.DROP_OLDEST,
                onUndeliveredElement = { dropped.incrementAndGet() },
            )
            if (closed) {
                channel.close()
            } else {
                subscribers.add(Subscriber(channel, dropped))
            }
            return channel
        }

        @Synchronized
        fun unsubscribe(channel: Channel<CameraFrame>) {
            subscribers.removeAll { it.channel === channel }
            channel.cancel()
        }

        @Synchronized
        fun hasSubscribers(): Boolean = subscribers.isNotEmpty()

        @Synchronized
        fun send(frame: CameraFrame) {
            for (sub in subscribers) {
                sub.channel.trySend(frame.copy(droppedBefore = sub.dropped.getAndSet(0).toUInt()))
            }
        }

        @Synchronized
        fun close() {
            closed = true
            for (sub in subscribers) sub.channel.close()
            subscribers.clear()
        }
    }

    private suspend fun awaitProvider(): ProcessCameraProvider =
        suspendCancellableCoroutine { cont ->
            val future = ProcessCameraProvider.getInstance(appContext)
            future.addListener(
                {
                    try {
                        cont.resume(future.get())
                    } catch (e: Exception) {
                        cont.resumeWithException(
                            BackendException(CameraError.Backend(e.message ?: "camera provider unavailable"))
                        )
                    }
                },
                ContextCompat.getMainExecutor(appContext),
            )
        }

    private fun supportsFps(deviceId: String, fps: Int): Boolean {
        val ranges = runCatching {
            cameraManager.getCameraCharacteristics(deviceId)
                .get(CameraCharacteristics.CONTROL_AE_AVAILABLE_TARGET_FPS_RANGES)
        }.getOrNull() ?: return false
        return ranges.any { it.lower == fps && it.upper == fps }
    }

    private fun applyInitialControls(state: SessionState, controls: CameraControls) {
        controls.zoom?.let { state.camera.cameraControl.setZoomRatio(it) }
        controls.torch?.let { state.camera.cameraControl.enableTorch(it) }
    }

    private fun emitFrame(fanout: FrameFanout, seq: AtomicLong, image: ImageProxy, format: PixelFormat) {
        image.use {
            val frameSeq = seq.getAndIncrement()
            // Skip the copy entirely while nobody listens.
            if (!fanout.hasSubscribers()) return
            val (planes, bytes) = when (format) {
                is PixelFormat.Nv21 -> toNv21(it)
                is PixelFormat.Bgra8 -> extractPlanes(it).also { (_, data) -> swapRedBlue(data) }
                else -> extractPlanes(it)
            }
            fanout.send(
                CameraFrame(
                    seq = frameSeq.toULong(),
                    timestampNs = it.imageInfo.timestamp.toULong(),
                    resolution = Resolution(it.width.toUInt(), it.height.toUInt()),
                    rotationDegrees = it.imageInfo.rotationDegrees.toUShort(),
                    pixelFormat = format,
                    planes = planes,
                    bytes = bytes,
                    droppedBefore = 0u,
                )
            )
        }
    }

    /** Copy every plane as delivered (`YUV_420_888` or `RGBA_8888`). */
    private fun extractPlanes(image: ImageProxy): Pair<List<FramePlane>, ByteArray> {
        val src = image.planes
        val totalSize = src.sumOf { it.buffer.remaining() }
        val bytes = ByteArray(totalSize)
        val planes = ArrayList<FramePlane>(src.size)
        var offset = 0
        for ((index, p) in src.withIndex()) {
            val remaining = p.buffer.remaining()
            copyBuffer(p.buffer, bytes, offset)
            // YUV chroma planes are subsampled 2x2.
            val chroma = src.size == 3 && index > 0
            planes.add(
                FramePlane(
                    offset = offset.toUInt(),
                    rowStride = p.rowStride.toUInt(),
                    pixelStride = p.pixelStride.toUInt(),
                    width = (if (chroma) (image.width + 1) / 2 else image.width).toUInt(),
                    height = (if (chroma) (image.height + 1) / 2 else image.height).toUInt(),
                )
            )
            offset += remaining
        }
        return planes to bytes
    }

    /**
     * Repack a `YUV_420_888` image into tightly packed NV21: a full-size
     * Y plane followed by one interleaved V/U plane at half resolution.
     */
    private fun toNv21(image: ImageProxy): Pair<List<FramePlane>, ByteArray> {
        val width = image.width
        val height = image.height
        val chromaWidth = (width + 1) / 2
        val chromaHeight = (height + 1) / 2
        val ySize = width * height
        val out = ByteArray(ySize + chromaWidth * chromaHeight * 2)

        val y = image.planes[0]
        val yBuf = y.buffer
        for (row in 0 until height) {
            for (col in 0 until width) {
                out[row * width + col] = yBuf.get(row * y.rowStride + col * y.pixelStride)
            }
        }
        val u = image.planes[1]
        val v = image.planes[2]
        val uBuf = u.buffer
        val vBuf = v.buffer
        var dst = ySize
        for (row in 0 until chromaHeight) {
            for (col in 0 until chromaWidth) {
                out[dst++] = vBuf.get(row * v.rowStride + col * v.pixelStride)
                out[dst++] = uBuf.get(row * u.rowStride + col * u.pixelStride)
            }
        }
        val planes = listOf(
            FramePlane(0u, width.toUInt(), 1u, width.toUInt(), height.toUInt()),
            FramePlane(
                ySize.toUInt(),
                (chromaWidth * 2).toUInt(),
                2u,
                chromaWidth.toUInt(),
                chromaHeight.toUInt(),
            ),
        )
        return planes to out
    }

    /** `RGBA_8888` → BGRA in place. */
    private fun swapRedBlue(bytes: ByteArray) {
        var i = 0
        while (i + 3 < bytes.size) {
            val r = bytes[i]
            bytes[i] = bytes[i + 2]
            bytes[i + 2] = r
            i += 4
        }
    }

    private fun copyBuffer(src: ByteBuffer, dst: ByteArray, offset: Int) {
        val mark = src.position()
        src.get(dst, offset, src.remaining())
        src.position(mark)
    }

    private fun reencodeJpeg(jpeg: ByteArray, quality: Int): ByteArray {
        if (quality == CAPTURE_JPEG_QUALITY) return jpeg
        val bitmap = BitmapFactory.decodeByteArray(jpeg, 0, jpeg.size) ?: return jpeg
        return try {
            ByteArrayOutputStream().use { out ->
                bitmap.compress(Bitmap.CompressFormat.JPEG, quality, out)
                out.toByteArray()
            }
        } finally {
            bitmap.recycle()
        }
    }

    /**
     * Analyzer output format for a requested stream format, or `null`
     * when the stream cannot carry it.
     *
     * - `Nv21` is captured as `YUV_420_888` and repacked ([toNv21]).
     * - `Bgra8` is captured as `RGBA_8888` and swapped ([swapRedBlue]).
     * - `Jpeg` is a still-only format (see [capture_still]).
     */
    private fun outputFormat(format: PixelFormat): Int? = when (format) {
        is PixelFormat.Yuv420, is PixelFormat.Nv21 -> ImageAnalysis.OUTPUT_IMAGE_FORMAT_YUV_420_888
        is PixelFormat.Rgba8, is PixelFormat.Bgra8 -> ImageAnalysis.OUTPUT_IMAGE_FORMAT_RGBA_8888
        is PixelFormat.Jpeg -> null
    }
    private fun describeDevice(id: String): CameraDevice {
        val chars = cameraManager.getCameraCharacteristics(id)
        val facing = when (chars.get(CameraCharacteristics.LENS_FACING)) {
            CameraCharacteristics.LENS_FACING_FRONT -> CameraFacing.Front
            CameraCharacteristics.LENS_FACING_BACK -> CameraFacing.Back
            CameraCharacteristics.LENS_FACING_EXTERNAL -> CameraFacing.External
            else -> CameraFacing.Unknown
        }
        val focalLengths: List<Float> = chars.get(CameraCharacteristics.LENS_INFO_AVAILABLE_FOCAL_LENGTHS)
            ?.toList() ?: emptyList()
        val maxZoom: Float = chars.get(CameraCharacteristics.SCALER_AVAILABLE_MAX_DIGITAL_ZOOM) ?: 1f
        val caps: Set<Int> = chars.get(CameraCharacteristics.REQUEST_AVAILABLE_CAPABILITIES)
            ?.toSet() ?: emptySet()
        val flash: Boolean = chars.get(CameraCharacteristics.FLASH_INFO_AVAILABLE) ?: false
        val maxStillRect: Rect = chars.get(CameraCharacteristics.SENSOR_INFO_ACTIVE_ARRAY_SIZE) ?: Rect()

        // Logical multi-camera physical ids require API 28 +
        // reflection-safe constant resolution. Skip for the demo; a
        // full-fat backend reads them from
        // `CameraCharacteristics.LOGICAL_MULTI_CAMERA_PHYSICAL_IDS`
        // with an API gate.
        val physicalSensors: List<String> = emptyList()

        return CameraDevice(
            id = id,
            label = "Camera $id ($facing)",
            facing = facing,
            lens = inferLens(focalLengths, caps),
            physicalSensors = physicalSensors,
            capabilities = CameraCapabilities(
                minZoom = 1f,
                maxZoom = maxZoom,
                opticalZoomSteps = emptyList(),
                focalLengthsMm = focalLengths,
                supportsFlash = flash,
                supportsTorch = flash,
                supportsAutofocus = true,
                supportsManualFocus = CameraCharacteristics.REQUEST_AVAILABLE_CAPABILITIES_MANUAL_SENSOR in caps,
                supportsOis = (chars.get(CameraCharacteristics.LENS_INFO_AVAILABLE_OPTICAL_STABILIZATION)?.size ?: 0) > 1,
                supportsEis = (chars.get(CameraCharacteristics.CONTROL_AVAILABLE_VIDEO_STABILIZATION_MODES)?.size ?: 0) > 1,
                supportsHdr = CameraCharacteristics.REQUEST_AVAILABLE_CAPABILITIES_RAW in caps,
                supportsDepth = CameraCharacteristics.REQUEST_AVAILABLE_CAPABILITIES_DEPTH_OUTPUT in caps,
                supportsRaw = CameraCharacteristics.REQUEST_AVAILABLE_CAPABILITIES_RAW in caps,
                maxStillResolution = Resolution(
                    width = maxStillRect.width().toUInt(),
                    height = maxStillRect.height().toUInt(),
                ),
            ),
            supportedFormats = emptyList(),
        )
    }

    private fun inferLens(
        focalLengths: List<Float>,
        caps: Set<Int>,
    ): LensKind {
        if (CameraCharacteristics.REQUEST_AVAILABLE_CAPABILITIES_LOGICAL_MULTI_CAMERA in caps) {
            return LensKind.Logical
        }
        if (CameraCharacteristics.REQUEST_AVAILABLE_CAPABILITIES_DEPTH_OUTPUT in caps) {
            return LensKind.TrueDepth
        }
        val f = focalLengths.minOrNull() ?: return LensKind.Unknown
        return when {
            f < 2.5f -> LensKind.UltraWide
            f > 5f -> LensKind.Telephoto
            else -> LensKind.Wide
        }
    }

    private companion object {
        /** Quality the ImageCapture use case encodes at; other requests re-encode. */
        const val CAPTURE_JPEG_QUALITY = 95
    }
}
