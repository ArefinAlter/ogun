package dev.verne.ogun.gateway

import android.content.Context
import org.json.JSONArray
import org.json.JSONObject
import java.io.File

/**
 * JNI bindings to the Rust gateway core (`apps/studio/plugins/gateway/src/android.rs`). Every call takes and
 * returns JSON text: `{"ok": ...}` or `{"error": <O-00 envelope>}`. The Rust side serialises calls behind one lock,
 * so the service, the sync job and the plugin may call from any thread.
 */
internal object NativeGateway {
    /** The Studio app library (`[lib] name` in `apps/studio/src-tauri/Cargo.toml`). */
    private const val LIBRARY = "ogun_studio_lib"

    @Volatile
    private var loaded = false

    fun ensureLoaded() {
        if (loaded) return
        synchronized(this) {
            if (!loaded) {
                System.loadLibrary(LIBRARY)
                loaded = true
            }
        }
    }

    @JvmStatic external fun open(dir: String, configJson: String): String?
    @JvmStatic external fun close(): String?
    @JvmStatic external fun append(batchJson: String): String?
    @JvmStatic external fun appendBody(origin: String, receivedMs: Long, body: String, ndjson: Boolean): String?
    @JvmStatic external fun seal(nowMs: Long): String?
    @JvmStatic external fun offer(): String?
    @JvmStatic external fun applyWant(wantJson: String): String?
    @JvmStatic external fun chunk(blake3: String, index: Int): String?
    @JvmStatic external fun recordSync(outcomeJson: String): String?
    @JvmStatic external fun status(): String?
}

/** An error envelope from the core. `code` is the O-00 code, for example `stream.full` or `edge.not_enrolled`. */
internal class GatewayException(val code: String, message: String) : Exception(message) {
    fun envelope(): JSONObject = JSONObject().put("code", code).put("message", message ?: code)
}

/** Typed wrappers over [NativeGateway]. */
internal object Core {
    private fun unwrap(text: String?): Any? {
        if (text == null) throw GatewayException("internal.bug", "the gateway core returned nothing")
        val o = JSONObject(text)
        if (o.has("error")) {
            val e = o.getJSONObject("error")
            throw GatewayException(e.optString("code", "internal.bug"), e.optString("message", "gateway error"))
        }
        return if (o.isNull("ok")) null else o.get("ok")
    }

    private fun obj(text: String?): JSONObject =
        unwrap(text) as? JSONObject ?: throw GatewayException("internal.bug", "unexpected answer from the gateway core")

    /** The gateway directory: excluded from backups, because it holds the edge key. */
    fun dir(context: Context): File = File(context.noBackupFilesDir, "gateway").apply { mkdirs() }

    /** Opens the core, or reopens it when the configuration changed. Returns the status. */
    fun open(context: Context, settings: GatewaySettings): JSONObject {
        NativeGateway.ensureLoaded()
        return obj(NativeGateway.open(dir(context).absolutePath, settings.coreConfigJson().toString()))
    }

    fun close() {
        NativeGateway.ensureLoaded()
        unwrap(NativeGateway.close())
    }

    /** Appends MQTT messages: each `{"received_ms", "origin", "payload"}`. */
    fun append(records: JSONArray): JSONObject =
        obj(NativeGateway.append(JSONObject().put("records", records).toString()))

    fun appendBody(origin: String, receivedMs: Long, body: String, ndjson: Boolean): JSONObject =
        obj(NativeGateway.appendBody(origin, receivedMs, body, ndjson))

    fun seal(nowMs: Long): JSONObject = obj(NativeGateway.seal(nowMs))

    /** The signed `sync.offer` body, or null when nothing waits. */
    fun offer(): JSONObject? = unwrap(NativeGateway.offer()) as? JSONObject

    fun applyWant(want: JSONObject): JSONObject = obj(NativeGateway.applyWant(want.toString()))

    fun chunk(blake3: String, index: Int): JSONObject = obj(NativeGateway.chunk(blake3, index))

    fun recordSync(ok: Boolean, reason: String) {
        val outcome = JSONObject().put("at_ms", System.currentTimeMillis()).put("ok", ok).put("reason", reason)
        unwrap(NativeGateway.recordSync(outcome.toString()))
    }

    fun status(): JSONObject = obj(NativeGateway.status())
}
