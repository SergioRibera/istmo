fn main() {
    // `istmo-camera`'s build.rs emits the contract; `emit()` here picks
    // it up plus this crate's `istmo.toml` `[app]` section to drive
    // Kotlin / Swift dispatcher codegen into the Android app tree.
    istmo_build::emit();
}
