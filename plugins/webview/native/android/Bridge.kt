package dev.istmo.plugins.webview

import android.util.Base64
import org.json.JSONObject

/**
 * The page side of the bridge: a document-start shim turning the native
 * message port (`__istmoNative`, only injected into allowed origins)
 * into `window.istmo`, and the JSON frames both sides exchange.
 *
 * Page → app: `{t:"h"}` hello, `{t:"m",c,b}` post, `{t:"i",id,c,b}`
 * invoke. App → page: `{t:"m",c,b}` message, `{t:"r",id,ok,b|e}` reply.
 * `b` is base64; strings from the page travel as their UTF-8 bytes.
 */
internal object Bridge {
    const val NATIVE_OBJECT = "__istmoNative"

    sealed class Incoming {
        object Hello : Incoming()
        class Post(val channel: String, val data: ByteArray) : Incoming()
        class Invoke(val callId: Long, val channel: String, val data: ByteArray) : Incoming()
    }

    fun parse(raw: String): Incoming? = try {
        val json = JSONObject(raw)
        when (json.getString("t")) {
            "h" -> Incoming.Hello
            "m" -> Incoming.Post(json.getString("c"), decode(json.getString("b")))
            "i" -> Incoming.Invoke(json.getLong("id"), json.getString("c"), decode(json.getString("b")))
            else -> null
        }
    } catch (_: Exception) {
        null
    }

    fun message(channel: String, data: ByteArray): String =
        JSONObject().put("t", "m").put("c", channel).put("b", encode(data)).toString()

    fun reply(callId: Long, result: ReplyResult): String {
        val json = JSONObject().put("t", "r").put("id", callId)
        when (result) {
            is ReplyResult.Ok -> json.put("ok", true).put("b", encode(result.value))
            is ReplyResult.Err -> json.put("ok", false).put("e", result.value)
        }
        return json.toString()
    }

    private fun encode(data: ByteArray) = Base64.encodeToString(data, Base64.NO_WRAP)

    private fun decode(data: String) = Base64.decode(data, Base64.NO_WRAP)

    /** Document-start script for allowed origins. */
    fun shim(allowSubframes: Boolean): String = """
        (() => {
          const native = window.$NATIVE_OBJECT;
          if (!native || window.istmo ${if (allowSubframes) "" else "|| window !== window.top"}) return;
          const waiting = new Map();
          const listeners = new Set();
          let next = 1;
          const toB64 = (d) => {
            const bytes = typeof d === "string" ? new TextEncoder().encode(d)
              : d instanceof Uint8Array ? d : new Uint8Array(d);
            let bin = "";
            for (let i = 0; i < bytes.length; i += 0x8000) bin += String.fromCharCode.apply(null, bytes.subarray(i, i + 0x8000));
            return btoa(bin);
          };
          const fromB64 = (b) => Uint8Array.from(atob(b), (c) => c.charCodeAt(0));
          const send = (o) => native.postMessage(JSON.stringify(o));
          native.onmessage = (e) => {
            let m;
            try { m = JSON.parse(e.data); } catch (_) { return; }
            if (m.t === "r") {
              const w = waiting.get(m.id);
              if (!w) return;
              waiting.delete(m.id);
              if (m.ok) w.resolve(fromB64(m.b)); else w.reject(new Error(m.e));
            } else if (m.t === "m") {
              const event = { channel: m.c, data: fromB64(m.b) };
              listeners.forEach((f) => { try { f(event); } catch (err) { console.error(err); } });
            }
          };
          Object.defineProperty(window, "istmo", {
            value: Object.freeze({
              postMessage(channel, data) { send({ t: "m", c: String(channel), b: toB64(data ?? "") }); },
              invoke(channel, data) {
                const id = next++;
                return new Promise((resolve, reject) => {
                  waiting.set(id, { resolve, reject });
                  send({ t: "i", id, c: String(channel), b: toB64(data ?? "") });
                });
              },
              addEventListener(type, f) { if (type === "message") listeners.add(f); },
              removeEventListener(type, f) { if (type === "message") listeners.delete(f); },
            }),
            configurable: false,
            writable: false,
          });
          send({ t: "h" });
        })();
    """.trimIndent()
}
