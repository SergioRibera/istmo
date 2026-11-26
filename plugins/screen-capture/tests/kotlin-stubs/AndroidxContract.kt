@file:Suppress("UNUSED_PARAMETER", "unused")

package androidx.activity.result.contract

abstract class ActivityResultContract<I, O>

class ActivityResultContracts {
    class StartActivityForResult : ActivityResultContract<android.content.Intent, androidx.activity.result.ActivityResult>()

    class RequestPermission : ActivityResultContract<String, Boolean>()
}
