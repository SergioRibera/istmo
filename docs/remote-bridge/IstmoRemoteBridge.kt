// REFERENCE — hand-drafted template for a `:remote`-process bridge.
// Copy into your Android module and adjust the package + service class
// names to match your app. Cargo does not build this file — istmo's
// Rust workspace has no Android SDK on the toolchain path, so it lives
// under `docs/remote-bridge/` as reference material.
//
// The bridge assumes the AIDL companion at
// `docs/remote-bridge/IIstmoBridge.aidl` was copied into
// `src/main/aidl/dev/istmo/remote/IIstmoBridge.aidl` on both processes.
//
// Wiring (per-process):
//
// 1. On the Rust side, `runtime.declare_remote_plugin(pluginId)` for every
//    trait that lives in the peer process. Every outbound frame that
//    references such a plugin id is then classified by the Rust runtime
//    and delivered as bytes to `IstmoRuntime.onRemoteEnvelope(bytes)`
//    instead of the typed `onCall` / `onRespond` / … callbacks.
// 2. Declare a `Service` that runs in the target process (main app on
//    the app-side stub, `:remote` on the remote-side stub).
// 3. On startup, each side binds to the OTHER side's service. Once the
//    Binder is available, each caches an `IIstmoBridge` proxy AND assigns
//    `IstmoRuntime.remoteEnvelopeSink = { bytes -> bridge.submit(bytes) }`.
// 4. `submitEnvelope` on the receiving side calls
//    `IstmoRuntime.nativeInjectEnvelope(bytes)` which forwards to
//    `Runtime::inject_wire_envelope`.
//
// The example below shows the app-side stub. The `:remote`-side stub is
// symmetric — same file, different `Intent` target.

package dev.istmo.remote

import android.app.Service
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.ServiceConnection
import android.os.IBinder
import java.util.concurrent.atomic.AtomicReference

/** Binder-side implementation the OTHER process talks to. */
class IstmoRemoteBridgeService : Service() {
    private val binder = object : IIstmoBridge.Stub() {
        override fun submitEnvelope(envelope: ByteArray) {
            // Hand the bytes to the local Rust runtime.
            IstmoRuntime.nativeInjectEnvelope(envelope)
        }
    }
    override fun onBind(intent: Intent?): IBinder = binder
}

/**
 * Client-side handle that binds to the peer process's
 * [IstmoRemoteBridgeService] and exposes a single `submit(bytes)` entry
 * point. Thread-safe; internally caches the resolved [IIstmoBridge] in
 * an [AtomicReference].
 *
 * Bind once at process start:
 *
 * ```
 * val bridge = IstmoRemoteBridgeClient(applicationContext, targetPackage,
 *     targetClassName = "dev.istmo.remote.IstmoRemoteBridgeService")
 * bridge.bind()
 * ```
 *
 * Rust's outbound pump then calls into `bridge.submit(envelopeBytes)`
 * for every envelope addressed to a remote-hosted plugin id.
 */
class IstmoRemoteBridgeClient(
    private val context: Context,
    private val targetPackage: String,
    private val targetClassName: String,
) {
    private val proxy = AtomicReference<IIstmoBridge?>()
    private val connection = object : ServiceConnection {
        override fun onServiceConnected(name: ComponentName?, service: IBinder?) {
            proxy.set(IIstmoBridge.Stub.asInterface(service))
        }
        override fun onServiceDisconnected(name: ComponentName?) {
            proxy.set(null)
        }
    }

    fun bind() {
        val intent = Intent().apply {
            component = ComponentName(targetPackage, targetClassName)
        }
        // `Context.BIND_AUTO_CREATE` starts the target service in its
        // declared process; `:remote` in manifest is the whole point.
        context.bindService(intent, connection, Context.BIND_AUTO_CREATE)
    }

    fun unbind() {
        context.unbindService(connection)
    }

    /**
     * Ship an envelope. Returns `false` when the Binder is not yet
     * connected — callers can requeue or drop depending on the wire
     * frame's tolerance for latency.
     */
    fun submit(envelope: ByteArray): Boolean {
        val bridge = proxy.get() ?: return false
        bridge.submitEnvelope(envelope)
        return true
    }
}

/**
 * Wires the bridge into `IstmoRuntime.remoteEnvelopeSink`. Call once on
 * startup, after the peer service has bound. Rust classifies every
 * outbound frame — no Kotlin-side plugin-id set is needed here.
 */
fun installRemoteBridge(bridge: IstmoRemoteBridgeClient) {
    IstmoRuntime.remoteEnvelopeSink = { bytes -> bridge.submit(bytes) }
}
