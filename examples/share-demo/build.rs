fn main() {
    // `istmo-share`'s build.rs hands off its contract and manifest via
    // `DEP_*`; `emit()` generates the Kotlin / Swift dispatcher, types and
    // codecs and checks the plugin's `[min_versions]` against this app.
    istmo_build::emit();
}
