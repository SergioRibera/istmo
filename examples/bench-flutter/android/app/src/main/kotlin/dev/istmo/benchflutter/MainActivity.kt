package dev.istmo.benchflutter

import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.plugin.common.MethodChannel

/**
 * Dart ↔ Kotlin echo handler for the MethodChannel baseline. The
 * Pigeon-FFI/JNIgen handler is generated into `BenchPigeon.kt` after
 * `dart run pigeon --input lib/bench_api.dart` and registered from
 * [configureFlutterEngine] once the generated file lands.
 */
class MainActivity : FlutterActivity() {
    override fun configureFlutterEngine(flutterEngine: FlutterEngine) {
        super.configureFlutterEngine(flutterEngine)
        MethodChannel(flutterEngine.dartExecutor.binaryMessenger, CHANNEL)
            .setMethodCallHandler { call, result ->
                when (call.method) {
                    "echo" -> result.success(call.arguments as ByteArray)
                    else -> result.notImplemented()
                }
            }

        // Enable after running the Pigeon generator:
        // BenchHost.setUp(flutterEngine.dartExecutor.binaryMessenger, BenchHostImpl())
    }

    companion object {
        private const val CHANNEL = "dev.istmo.bench/echo"
    }
}

/**
 * Stub for the Pigeon-generated `BenchHost` interface. Uncomment and
 * wire into [MainActivity.configureFlutterEngine] once the generator
 * has emitted `BenchPigeon.kt`.
 */
// class BenchHostImpl : BenchHost {
//     override fun echo(payload: ByteArray): ByteArray = payload
// }
