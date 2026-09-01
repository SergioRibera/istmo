package dev.istmo.demo

/**
 * Native bridge into the Rust demo cdylib.
 *
 * `call*` methods block the caller (they synchronously wait for the Rust
 * runtime to reply); dispatch them off the main thread.
 *
 * `push*` methods forward platform events into the Rust runtime's
 * early-event store; they are cheap and safe on the main thread.
 *
 * `poll*` methods read Rust-side plugin state without blocking; safe on the
 * main thread.
 */
object DemoBridge {
    external fun callEcho(text: String): String

    external fun pushLifecycle(state: Int)
    external fun pollLifecycle(): Int
    external fun awaitLifecycleTransition(): Int

    external fun pushDeepLink(uri: String, source: String?)
    external fun pollDeepLink(): String?

    external fun callCheckPermission(permission: String): Int
    external fun callRequestPermission(permission: String): Int
}
