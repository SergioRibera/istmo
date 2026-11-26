fn main() {
    // Picks up istmo-screen-capture's contract and this crate's
    // `istmo.toml` `[app]`, and writes the Android plugin registry.
    istmo_build::emit();
}
