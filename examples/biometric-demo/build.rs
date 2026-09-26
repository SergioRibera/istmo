fn main() {
    // `istmo-biometric`'s build.rs hands off its contract and manifest via
    // `DEP_*`; `emit()` generates the Kotlin / Swift dispatcher, types and
    // codecs and patches `NSFaceIDUsageDescription` into Info.plist.
    istmo_build::emit();
}
