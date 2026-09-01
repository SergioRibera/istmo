package dev.istmo.demo

/**
 * Native bridge into the Rust demo cdylib. `callEcho` is a synchronous call
 * that blocks the invoking thread until the Kotlin `EchoImpl` responds. The
 * caller should always dispatch it off the main thread.
 */
object DemoBridge {
    external fun callEcho(text: String): String
}
