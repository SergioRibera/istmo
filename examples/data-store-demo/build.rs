fn main() {
    // `istmo-data-store`'s own build.rs hands off the contract via
    // `DEP_*_ISTMO_CONTRACT`; `emit()` picks it up and drives Kotlin/Swift
    // codegen out of the box.
    istmo_build::emit();
}
