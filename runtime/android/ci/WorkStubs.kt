@file:Suppress("UNUSED_PARAMETER")

package androidx.work

class WorkerParameters

class Data {
    fun getString(key: String): String? = null
    fun getByteArray(key: String): ByteArray? = null
}

abstract class ListenableWorker {
    abstract class Result {
        companion object {
            fun success(): Result = throw UnsupportedOperationException()
            fun retry(): Result = throw UnsupportedOperationException()
            fun failure(): Result = throw UnsupportedOperationException()
        }
    }
}

abstract class CoroutineWorker(context: android.content.Context, params: WorkerParameters) :
    ListenableWorker() {
    val inputData: Data = Data()
    abstract suspend fun doWork(): Result
}
