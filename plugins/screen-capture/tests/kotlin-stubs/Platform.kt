// Stand-ins for the Android SDK surface the MediaProjection backend in
// `native/android` uses, so `scripts/check-plugin-native.sh` compiles it
// on a plain JVM against the generated Kotlin. Shapes follow the SDK;
// bodies do nothing. The shared SDK types (Context, Service, Image,
// HardwareBuffer, ...) live in `runtime/android/ci`.
@file:Suppress("UNUSED_PARAMETER", "unused")

package android

object Manifest {
    object permission {
        const val RECORD_AUDIO: String = "android.permission.RECORD_AUDIO"
        const val POST_NOTIFICATIONS: String = "android.permission.POST_NOTIFICATIONS"
    }
}

object R {
    object drawable {
        const val ic_menu_camera: Int = 17301559
    }
}
