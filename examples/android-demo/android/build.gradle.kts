plugins {
    // Pinned to versions that work with the Gradle 8.2 shipped inside the
    // sergioribera/rust-android:1.96-sdk-37.0 image. Bumping requires
    // either upgrading the image or introducing a Gradle wrapper.
    id("com.android.application") version "8.2.2" apply false
    id("org.jetbrains.kotlin.android") version "1.9.24" apply false
    // Cross-compiles the Rust cdylib and drops the .so into the standard
    // AGP JNI merge — no manual `cargo build` / `cp` in the justfile.
    id("org.mozilla.rust-android-gradle.rust-android") version "0.9.4" apply false
}
