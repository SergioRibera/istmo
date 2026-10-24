package dev.istmo.plugins.barcodescanner

import androidx.activity.ComponentActivity
import com.google.android.gms.common.ConnectionResult
import com.google.android.gms.common.GoogleApiAvailability
import com.google.mlkit.common.MlKitException
import com.google.mlkit.vision.barcode.common.Barcode
import com.google.mlkit.vision.codescanner.GmsBarcodeScannerOptions
import com.google.mlkit.vision.codescanner.GmsBarcodeScanning
import dev.istmo.runtime.BackendException
import dev.istmo.runtime.BarcodeFormat
import dev.istmo.runtime.BarcodeScannerBackend
import dev.istmo.runtime.ScanConfig
import dev.istmo.runtime.ScanError
import dev.istmo.runtime.ScannedCode
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock

/**
 * Reference backend for `istmo.barcode_scanner` on top of the Google
 * code scanner. Play services draws the camera UI and decodes, so the
 * app needs no `CAMERA` permission. Devices without Play services get
 * [ScanError.Unavailable].
 */
class BarcodeScannerBackendImpl(
    private val activity: ComponentActivity,
) : BarcodeScannerBackend {

    /** One scanner on screen at a time. */
    private val scanMutex = Mutex()

    override suspend fun scan(config: ScanConfig): ScannedCode? = scanMutex.withLock {
        val availability = GoogleApiAvailability.getInstance()
            .isGooglePlayServicesAvailable(activity)
        if (availability != ConnectionResult.SUCCESS) {
            throw BackendException(ScanError.Unavailable)
        }
        val options = GmsBarcodeScannerOptions.Builder()
            .apply {
                val formats = config.formats.map(::toMlKit)
                if (formats.isNotEmpty()) {
                    setBarcodeFormats(formats.first(), *formats.drop(1).toIntArray())
                }
            }
            .enableAutoZoom()
            .build()
        val scanner = GmsBarcodeScanning.getClient(activity, options)
        suspendCancellableCoroutine { cont ->
            scanner.startScan()
                .addOnSuccessListener { barcode ->
                    val text = barcode.rawValue
                    if (text == null) {
                        cont.resume(null)
                    } else {
                        cont.resume(ScannedCode(text, fromMlKit(barcode.format)))
                    }
                }
                .addOnCanceledListener { cont.resume(null) }
                .addOnFailureListener { error ->
                    val mapped = when ((error as? MlKitException)?.errorCode) {
                        MlKitException.CODE_SCANNER_CANCELLED -> null
                        MlKitException.CODE_SCANNER_CAMERA_PERMISSION_NOT_GRANTED ->
                            ScanError.PermissionDenied
                        MlKitException.CODE_SCANNER_UNAVAILABLE,
                        MlKitException.CODE_SCANNER_APP_NAME_UNAVAILABLE -> ScanError.Unavailable
                        else -> ScanError.Backend(error.message ?: "scan failed")
                    }
                    if (mapped == null) {
                        cont.resume(null)
                    } else {
                        cont.resumeWithException(BackendException(mapped))
                    }
                }
        }
    }

    private fun toMlKit(format: BarcodeFormat): Int = when (format) {
        BarcodeFormat.Qr -> Barcode.FORMAT_QR_CODE
        BarcodeFormat.Aztec -> Barcode.FORMAT_AZTEC
        BarcodeFormat.DataMatrix -> Barcode.FORMAT_DATA_MATRIX
        BarcodeFormat.Pdf417 -> Barcode.FORMAT_PDF417
        BarcodeFormat.Code128 -> Barcode.FORMAT_CODE_128
        BarcodeFormat.Ean13 -> Barcode.FORMAT_EAN_13
    }

    private fun fromMlKit(format: Int): BarcodeFormat = when (format) {
        Barcode.FORMAT_AZTEC -> BarcodeFormat.Aztec
        Barcode.FORMAT_DATA_MATRIX -> BarcodeFormat.DataMatrix
        Barcode.FORMAT_PDF417 -> BarcodeFormat.Pdf417
        Barcode.FORMAT_CODE_128 -> BarcodeFormat.Code128
        Barcode.FORMAT_EAN_13 -> BarcodeFormat.Ean13
        else -> BarcodeFormat.Qr
    }
}
