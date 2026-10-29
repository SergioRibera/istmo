package dev.istmo.plugins.camera

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.graphics.Rect
import android.hardware.camera2.CameraCharacteristics
import android.hardware.camera2.CameraManager
import android.os.Build
import android.util.Range
import android.util.Size
import androidx.activity.ComponentActivity
import androidx.activity.result.contract.ActivityResultContracts
import androidx.camera.camera2.interop.Camera2CameraInfo
import androidx.camera.camera2.interop.Camera2Interop
import androidx.camera.core.CameraSelector
import androidx.camera.core.ImageAnalysis
import androidx.camera.core.ImageProxy
import androidx.camera.core.Preview
import androidx.camera.core.resolutionselector.ResolutionSelector
import androidx.camera.core.resolutionselector.ResolutionStrategy
import androidx.camera.lifecycle.ProcessCameraProvider
import androidx.core.content.ContextCompat
import dev.istmo.runtime.*
import java.nio.ByteBuffer
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicLong
import kotlin.coroutines.resume
import kotlin.coroutines.suspendCoroutine
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.asSharedFlow
import kotlinx.coroutines.flow.emptyFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/**
 * CameraX-backed implementation of [CameraBackend]. Enumerates every
 * camera the OS exposes, drives one or more concurrent sessions, and
 * fans analyzer frames out through a per-session [MutableSharedFlow]
 * sized by [CameraInitConfig.framePoolDepth].
 */
class CameraBackendImpl(
    private val activity: ComponentActivity,
    private val config: CameraInitConfig,
) : CameraBackend, HandleReleaser {

    private val sessions = ConcurrentHashMap<Long, SessionState>()
    private val frames = ConcurrentHashMap<Long, MutableSharedFlow<CameraFrame>>()
    private val droppedCounter = ConcurrentHashMap<Long, AtomicLong>()
    private val sequence = ConcurrentHashMap<Long, AtomicLong>()
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
    private val cameraManager by lazy {
        activity.getSystemService(Context.CAMERA_SERVICE) as CameraManager
    }
    private val poolDepth: Int =
        if (config.framePoolDepth > 0u) config.framePoolDepth.toInt() else 3

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
        ContextCompat.checkSelfPermission(activity, Manifest.permission.CAMERA)
    ) {
        PackageManager.PERMISSION_GRANTED -> AccessStatus.Granted
        else -> AccessStatus.NotDetermined
    }

    override suspend fun request_access(): AccessStatus = withContext(Dispatchers.Main) {
        suspendCoroutine { cont ->
            val launcher = activity.activityResultRegistry.register(
                "istmo.camera.permission.${System.nanoTime()}",
                ActivityResultContracts.RequestPermission(),
            ) { granted ->
                cont.resume(if (granted) AccessStatus.Granted else AccessStatus.Denied)
            }
            launcher.launch(Manifest.permission.CAMERA)
        }
    }

    // ---- Session lifecycle ----------------------------------------

    override suspend fun open(config: SessionConfig): CameraSession = withContext(Dispatchers.Main) {
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

        val preview = Preview.Builder().apply {
            resolutionSelector?.let { setResolutionSelector(it) }
        }.build()
        val analysisBuilder = ImageAnalysis.Builder()
            .setBackpressureStrategy(ImageAnalysis.STRATEGY_KEEP_ONLY_LATEST)
            .setOutputImageFormat(outputFormat(config.pixelFormat))
            .apply {
                resolutionSelector?.let { setResolutionSelector(it) }
                config.targetFps?.let { fps ->
                    Camera2Interop.Extender(this).setCaptureRequestOption(
                        android.hardware.camera2.CaptureRequest.CONTROL_AE_TARGET_FPS_RANGE,
                        Range(fps.toInt(), fps.toInt()),
                    )
                }
            }
        val analysis = analysisBuilder.build()

        val handleId: Long = IstmoRuntime.allocHandleId("istmo.camera")
        val flow = MutableSharedFlow<CameraFrame>(
            replay = 0,
            extraBufferCapacity = poolDepth,
            onBufferOverflow = kotlinx.coroutines.channels.BufferOverflow.DROP_OLDEST,
        )
        frames[handleId] = flow
        droppedCounter[handleId] = AtomicLong(0)
        sequence[handleId] = AtomicLong(0)

        val analyzer = Executors.newSingleThreadExecutor()
        if (config.emitFrames) {
            analysis.setAnalyzer(analyzer) { image ->
                emitFrame(handleId, image, config.pixelFormat)
            }
        }

        val camera = provider.bindToLifecycle(activity, selector, preview, analysis)
        val state = SessionState(camera, preview, analysis, analyzer, provider)
        sessions[handleId] = state

        applyInitialControls(state, config.initialControls)

        val negotiatedSize = analysis.resolutionInfo?.resolution ?: Size(0, 0)
        val negotiatedFps = config.targetFps ?: 30u
        CameraSession(
            session = handleId,
            negotiated = NegotiatedFormat(
                resolution = Resolution(negotiatedSize.width.toUInt(), negotiatedSize.height.toUInt()),
                pixelFormat = config.pixelFormat,
                fps = negotiatedFps,
                rotationDegrees = analysis.resolutionInfo?.rotationDegrees?.toUShort() ?: 0u,
            ),
        )
    }

    override suspend fun close(session: Long) = withContext(Dispatchers.Main) {
        val state = sessions.remove(session) ?: return@withContext
        state.provider.unbind(state.preview, state.analysis)
        state.analysis.clearAnalyzer()
        state.analyzer.shutdown()
        frames.remove(session)
        droppedCounter.remove(session)
        sequence.remove(session)
        IstmoRuntime.forgetHandle(session)
    }

    override fun frames(session: Long): Flow<CameraFrame> =
        frames[session]?.asSharedFlow() ?: emptyFlow()

    override fun releaseNativeHandle(handleId: Long) {
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
            torch = info.torchState.value == androidx.camera.core.TorchState.ON,
            focus = controls.focus ?: FocusMode.ContinuousVideo,
            exposureCompensationEv = info.exposureState.exposureCompensationIndex.toFloat()
                * info.exposureState.exposureCompensationStep.toFloat(),
            whiteBalance = controls.whiteBalance ?: WhiteBalanceMode.Auto,
            flash = controls.flash ?: FlashMode.Off,
        )
    }

    override suspend fun capture_still(
        session: Long,
        request: StillCaptureRequest,
    ): StillPhoto {
        throw BackendException(CameraError.Backend("capture_still: wire ImageCapture use-case"))
    }

    // ---- Internals -------------------------------------------------

    private data class SessionState(
        val camera: androidx.camera.core.Camera,
        val preview: Preview,
        val analysis: ImageAnalysis,
        val analyzer: java.util.concurrent.ExecutorService,
        val provider: ProcessCameraProvider,
    )

    private suspend fun awaitProvider(): ProcessCameraProvider = suspendCoroutine { cont ->
        val future = ProcessCameraProvider.getInstance(activity)
        future.addListener({ cont.resume(future.get()) }, ContextCompat.getMainExecutor(activity))
    }

    private fun applyInitialControls(state: SessionState, controls: CameraControls) {
        controls.zoom?.let { state.camera.cameraControl.setZoomRatio(it) }
        controls.torch?.let { state.camera.cameraControl.enableTorch(it) }
    }

    private fun emitFrame(session: Long, image: ImageProxy, format: PixelFormat) {
        try {
            val seq = sequence[session]?.getAndIncrement() ?: return
            val flow = frames[session] ?: return
            val dropped = droppedCounter[session]?.getAndSet(0)?.toInt() ?: 0

            val (planes, bytes) = extractPlanes(image)
            val frame = CameraFrame(
                seq = seq.toULong(),
                timestampNs = image.imageInfo.timestamp.toULong(),
                resolution = Resolution(image.width.toUInt(), image.height.toUInt()),
                rotationDegrees = image.imageInfo.rotationDegrees.toUShort(),
                pixelFormat = format,
                planes = planes,
                bytes = bytes,
                droppedBefore = dropped.toUInt(),
            )
            if (!flow.tryEmit(frame)) {
                droppedCounter[session]?.incrementAndGet()
            }
        } finally {
            image.close()
        }
    }

    private fun extractPlanes(image: ImageProxy): Pair<List<FramePlane>, ByteArray> {
        val src = image.planes
        val totalSize = src.sumOf { it.buffer.remaining() }
        val bytes = ByteArray(totalSize)
        val planes = ArrayList<FramePlane>(src.size)
        var offset = 0
        for (p in src) {
            val remaining = p.buffer.remaining()
            copyBuffer(p.buffer, bytes, offset)
            planes.add(
                FramePlane(
                    offset = offset.toUInt(),
                    rowStride = p.rowStride.toUInt(),
                    pixelStride = p.pixelStride.toUInt(),
                    width = image.width.toUInt(),
                    height = image.height.toUInt(),
                )
            )
            offset += remaining
        }
        return planes to bytes
    }

    private fun copyBuffer(src: ByteBuffer, dst: ByteArray, offset: Int) {
        val mark = src.position()
        src.get(dst, offset, src.remaining())
        src.position(mark)
    }

    private fun outputFormat(format: PixelFormat): Int = when (format) {
        is PixelFormat.Yuv420, is PixelFormat.Nv21 -> ImageAnalysis.OUTPUT_IMAGE_FORMAT_YUV_420_888
        is PixelFormat.Rgba8, is PixelFormat.Bgra8 -> ImageAnalysis.OUTPUT_IMAGE_FORMAT_RGBA_8888
        is PixelFormat.Jpeg -> ImageAnalysis.OUTPUT_IMAGE_FORMAT_YUV_420_888
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

    protected fun finalize() {
        scope.cancel()
    }
}
