@file:Suppress("UNUSED_PARAMETER", "unused")

package androidx.activity.result

class ActivityResult(val resultCode: Int, val data: android.content.Intent?)

fun interface ActivityResultCallback<O> {
    fun onActivityResult(result: O)
}

abstract class ActivityResultLauncher<I> {
    abstract fun launch(input: I)
    abstract fun unregister()
}

open class ActivityResultRegistry {
    fun <I, O> register(
        key: String,
        contract: androidx.activity.result.contract.ActivityResultContract<I, O>,
        callback: ActivityResultCallback<O>,
    ): ActivityResultLauncher<I> = object : ActivityResultLauncher<I>() {
        override fun launch(input: I) {}
        override fun unregister() {}
    }
}
