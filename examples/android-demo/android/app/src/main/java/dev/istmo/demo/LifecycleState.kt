package dev.istmo.demo

/**
 * Mirror of the Rust `istmo::plugins::LifecycleState` enum. The ordinal is the
 * wire discriminant used by `DemoBridge.pushLifecycle` / `pollLifecycle`.
 *
 * KEEP THIS ORDER IN SYNC with the Rust enum declaration order.
 */
enum class LifecycleState {
    Created,
    Started,
    Resumed,
    Paused,
    Stopped,
    Destroyed,
    LowMemory,
    ConfigurationChanged;

    companion object {
        fun fromOrdinal(value: Int): LifecycleState? =
            values().getOrNull(value)
    }
}
