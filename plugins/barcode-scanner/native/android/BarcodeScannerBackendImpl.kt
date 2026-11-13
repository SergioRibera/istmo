package dev.istmo.plugins.barcodescanner

import androidx.activity.ComponentActivity
import com.google.android.gms.common.ConnectionResult
import com.google.android.gms.common.GoogleApiAvailability
import com.google.mlkit.common.MlKitException
import com.google.mlkit.vision.barcode.common.Barcode
import com.google.mlkit.vision.codescanner.GmsBarcodeScannerOptions
import com.google.mlkit.vision.codescanner.GmsBarcodeScanning
import dev.istmo.runtime.BackendException
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
        val formats = config.formats.mapNotNull(::toMlKit)
        // An empty list means "every format"; one that only became
        // empty by dropping `Other` must not widen the scan.
        if (formats.isEmpty() && config.formats.isNotEmpty()) {
            throw BackendException(ScanError.Backend("none of the requested formats can be scanned"))
        }
        val options = GmsBarcodeScannerOptions.Builder()
            .apply {
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
                    // `rawValue` is null for payloads that are not text
                    // (binary QR codes); that is still a successful read.
                    val text = barcode.rawValue
                    val raw = barcode.rawBytes
                    if (text == null && raw == null) {
                        cont.resumeWithException(
                            BackendException(ScanError.Backend("scanner returned an empty code")),
                        )
                    } else {
                        cont.resume(ScannedCode(text.orEmpty(), fromMlKit(barcode.format), raw))
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

    /** `null` for `Other`, which names no ML Kit format to ask for. */
    private fun toMlKit(format: BarcodeFormat): Int? = when (format) {
        BarcodeFormat.Qr -> Barcode.FORMAT_QR_CODE
        BarcodeFormat.Aztec -> Barcode.FORMAT_AZTEC
        BarcodeFormat.DataMatrix -> Barcode.FORMAT_DATA_MATRIX
        BarcodeFormat.Pdf417 -> Barcode.FORMAT_PDF417
        BarcodeFormat.Code128 -> Barcode.FORMAT_CODE_128
        BarcodeFormat.Ean13 -> Barcode.FORMAT_EAN_13
        BarcodeFormat.Ean8 -> Barcode.FORMAT_EAN_8
        BarcodeFormat.UpcA -> Barcode.FORMAT_UPC_A
        BarcodeFormat.UpcE -> Barcode.FORMAT_UPC_E
        BarcodeFormat.Code39 -> Barcode.FORMAT_CODE_39
        BarcodeFormat.Code93 -> Barcode.FORMAT_CODE_93
        BarcodeFormat.Itf -> Barcode.FORMAT_ITF
        BarcodeFormat.Codabar -> Barcode.FORMAT_CODABAR
        BarcodeFormat.Other -> null
    }

    private fun fromMlKit(format: Int): BarcodeFormat = when (format) {
        Barcode.FORMAT_QR_CODE -> BarcodeFormat.Qr
        Barcode.FORMAT_AZTEC -> BarcodeFormat.Aztec
        Barcode.FORMAT_DATA_MATRIX -> BarcodeFormat.DataMatrix
        Barcode.FORMAT_PDF417 -> BarcodeFormat.Pdf417
        Barcode.FORMAT_CODE_128 -> BarcodeFormat.Code128
        Barcode.FORMAT_EAN_13 -> BarcodeFormat.Ean13
        Barcode.FORMAT_EAN_8 -> BarcodeFormat.Ean8
        Barcode.FORMAT_UPC_A -> BarcodeFormat.UpcA
        Barcode.FORMAT_UPC_E -> BarcodeFormat.UpcE
        Barcode.FORMAT_CODE_39 -> BarcodeFormat.Code39
        Barcode.FORMAT_CODE_93 -> BarcodeFormat.Code93
        Barcode.FORMAT_ITF -> BarcodeFormat.Itf
        Barcode.FORMAT_CODABAR -> BarcodeFormat.Codabar
        else -> BarcodeFormat.Other
    }
}
