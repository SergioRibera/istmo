package dev.istmo.bench_flutter

import android.content.Intent
import android.util.Log
import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.plugin.common.MethodChannel

/**
 * Dart ↔ Kotlin bridge. Three MethodChannel handlers for the three
 * measured paths plus a side channel that lets Dart report finished
 * results into [Log.i] so a host shell can scrape them out of logcat.
 *
 * The Pigeon-FFI/JNIgen handler lives in `BenchPigeon.kt` after
 * `dart run pigeon --input lib/bench_api.dart` and gets registered
 * from [configureFlutterEngine] once the generated file lands.
 */
class MainActivity : FlutterActivity() {
    override fun configureFlutterEngine(flutterEngine: FlutterEngine) {
        super.configureFlutterEngine(flutterEngine)

        MethodChannel(flutterEngine.dartExecutor.binaryMessenger, CHANNEL)
            .setMethodCallHandler { call, result ->
                when (call.method) {
                    // Current MethodChannel echo baseline.
                    "echo" -> result.success(call.arguments as ByteArray)
                    // Pure MethodChannel roundtrip; the arg carries no
                    // payload. The gap between this and the two above
                    // isolates how much the codec adds for this call
                    // shape.
                    "noop" -> result.success(null)
                    // MethodChannel + ByteArray decode; bytes travel but
                    // are not returned, so this isolates the inbound
                    // codec cost.
                    "noopBytes" -> {
                        val _bytes = call.arguments as ByteArray
                        result.success(null)
                    }
                    else -> result.notImplemented()
                }
            }

        // Side channel Dart calls once the bench is done. Messages land
        // in `adb logcat` with tag [LOG_TAG] between [LOG_BEGIN] /
        // [LOG_END] markers so the host script knows where to cut.
        MethodChannel(flutterEngine.dartExecutor.binaryMessenger, REPORT_CHANNEL)
            .setMethodCallHandler { call, result ->
                when (call.method) {
                    "begin" -> {
                        Log.i(LOG_TAG, LOG_BEGIN)
                        result.success(null)
                    }
                    "line" -> {
                        val line = call.arguments as? String ?: ""
                        Log.i(LOG_TAG, line)
                        result.success(null)
                    }
                    "end" -> {
                        Log.i(LOG_TAG, LOG_END)
                        result.success(null)
                    }
                    "shouldAutoRun" -> {
                        result.success(intent?.getBooleanExtra("auto", false) ?: false)
                    }
                    else -> result.notImplemented()
                }
            }

        BenchHost.setUp(flutterEngine.dartExecutor.binaryMessenger, BenchHostImpl())
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
    }

    companion object {
        private const val CHANNEL = "dev.istmo.bench/echo"
        private const val REPORT_CHANNEL = "dev.istmo.bench/report"
        private const val LOG_TAG = "istmo-flutter-bench"
        private const val LOG_BEGIN = "BENCH_BEGIN"
        private const val LOG_END = "BENCH_END"
    }
}

class BenchHostImpl : BenchHost {
    override fun echo(payload: ByteArray): ByteArray = payload
}
