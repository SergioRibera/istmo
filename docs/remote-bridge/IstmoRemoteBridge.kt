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
// 1. Declare a `Service` that runs in the target process (main app on
//    the app-side stub, `:remote` on the remote-side stub).
// 2. On startup, each side binds to the OTHER side's service. Once the
//    Binder is available, each caches a `IIstmoBridge` proxy.
// 3. When Rust submits an outbound envelope destined for a remote-hosted
//    plugin, the pump serialises the Envelope via
//    `Envelope::to_wire_bytes` (Rust-side) and hands the bytes to
//    `remoteBridge.submitEnvelope(bytes)`.
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
 * Router installed into `IstmoRuntime.onCall` on the app-process side.
 * When the plugin id is in `remotePlugins`, the router rebuilds the
 * Frame::Call as an Envelope, bincode-encodes it via
 * `nativeEncodeCall(...)`, and ships the bytes over the bridge instead
 * of dispatching locally.
 *
 * The `nativeEncode*` helpers must be added to `istmo-android` — see
 * CLAUDE.md "`:remote` process bridge" section for the JNI shape.
 */
object RemotePluginRouter {
    private val remotePlugins = mutableSetOf<String>()
    private var bridge: IstmoRemoteBridgeClient? = null

    fun install(bridgeClient: IstmoRemoteBridgeClient, pluginIds: Collection<String>) {
        bridge = bridgeClient
        remotePlugins.clear()
        remotePlugins.addAll(pluginIds)
    }

    fun isRemote(pluginId: String): Boolean = remotePlugins.contains(pluginId)

    /**
     * Called by `IstmoRuntime.onCall` before local dispatch. Returns
     * `true` when the call was forwarded, `false` when the caller must
     * fall through to local handling.
     */
    fun tryForwardCall(
        callId: Long,
        pluginId: String,
        instanceId: Long,
        method: String,
        payload: ByteArray,
    ): Boolean {
        if (!isRemote(pluginId)) return false
        val bridge = bridge ?: return false
        val envelope = IstmoRuntime.nativeEncodeCall(callId, pluginId, instanceId, method, payload)
        return bridge.submit(envelope)
    }
}
