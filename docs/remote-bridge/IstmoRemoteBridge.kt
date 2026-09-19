package dev.istmo.remote

import android.app.Service
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.ServiceConnection
import android.os.IBinder
import java.util.concurrent.atomic.AtomicReference

class IstmoRemoteBridgeService : Service() {
    private val binder = object : IIstmoBridge.Stub() {
        override fun submitEnvelope(envelope: ByteArray) {

            IstmoRuntime.nativeInjectEnvelope(envelope)
        }
    }
    override fun onBind(intent: Intent?): IBinder = binder
}

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

        context.bindService(intent, connection, Context.BIND_AUTO_CREATE)
    }

    fun unbind() {
        context.unbindService(connection)
    }

    fun submit(envelope: ByteArray): Boolean {
        val bridge = proxy.get() ?: return false
        bridge.submitEnvelope(envelope)
        return true
    }
}

fun installRemoteBridge(bridge: IstmoRemoteBridgeClient) {
    IstmoRuntime.remoteEnvelopeSink = { bytes -> bridge.submit(bytes) }
}

