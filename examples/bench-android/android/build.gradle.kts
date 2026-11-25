plugins {
    // Same toolchain as the demos and the runtime itself (Kotlin 1.9.24),
    // so the generated Kotlin is compiled the way apps compile it.
    id("com.android.application") version "8.2.2" apply false
    id("org.jetbrains.kotlin.android") version "1.9.24" apply false
}
