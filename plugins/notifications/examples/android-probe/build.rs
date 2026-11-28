fn main() {
    // Picks up istmo-notifications' contract and this crate's
    // `istmo.toml` `[app]`, and writes the Android plugin registry.
    istmo_build::emit();
}
