fn main() {
    // `istmo-pen`'s build.rs emits the contract; `emit()` here picks it
    // up plus the `[app]` section of `istmo.toml` to drive Kotlin /
    // Swift dispatcher codegen into the Android app tree.
    istmo_build::emit();
}
