//! Widget cdylib for the multi-cdylib packaging demo.
//!
//! Real widget code lives in a separate Rust process on Android's widget
//! host; that requires a distinct JNI class + runtime, which is deferred.
//! For now the artifact is a scaffold whose `.so` proves that
//! `istmoCargoLib` in Gradle handles multiple crates per module.

istmo::runtime!();
