plugins {
    // Pinned to the versions that work with the Gradle 8.2 shipped inside
    // the sergioribera/rust-android:1.96-sdk-37.0 image. Bumping requires
    // either upgrading the image or introducing a Gradle wrapper.
    id("com.android.application") version "8.2.2" apply false
    id("org.jetbrains.kotlin.android") version "1.9.24" apply false
}
