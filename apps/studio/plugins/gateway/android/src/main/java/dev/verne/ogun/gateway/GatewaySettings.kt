package dev.verne.ogun.gateway

import android.content.Context
import org.json.JSONArray
import org.json.JSONObject
import java.net.URI

/**
 * The gateway configuration, kept in app-private preferences. Secrets (the MQTT password and the HTTP push device
 * token) are stored separately, encrypted by [SecretBox]; [publicJson] reports only whether they are set.
 */
internal data class GatewaySettings(
    val name: String = "phone",
    val edgeId: String? = null,
    val hubUrl: String? = null,
    /** A private CA (PEM) for the hub and the broker; the system trust store when absent. */
    val caPem: String? = null,
    val mqttUrl: String? = null,
    val mqttTopics: List<String> = emptyList(),
    val mqttUsername: String? = null,
    val httpPushEnabled: Boolean = false,
    val httpPushPort: Int = 8787,
    val syncRequiresCharging: Boolean = true,
    val segmentMs: Long = 300_000,
    val maxLogBytes: Long = 512L * 1024 * 1024,
    val whenFull: String = "block",
) {
    companion object {
        private const val PREFS = "ogun_gateway"
        private const val SECRETS = "ogun_gateway_secrets"
        private const val KEY_SETTINGS = "settings"
        private const val KEY_MQTT_PASSWORD = "mqtt_password"
        private const val KEY_PUSH_TOKEN = "http_push_token"
        private val NAME = Regex("^[a-z0-9_-]{1,64}$")
        private val EDGE_ID = Regex("^edg_[0-9A-HJKMNP-TV-Z]{26}$")
        private val WHEN_FULL = setOf("block", "drop_oldest", "drop_newest")

        fun load(context: Context): GatewaySettings {
            val text = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).getString(KEY_SETTINGS, null)
                ?: return GatewaySettings()
            return try {
                fromJson(JSONObject(text))
            } catch (e: Exception) {
                GatewaySettings()
            }
        }

        private fun str(o: JSONObject, k: String): String? =
            if (o.isNull(k)) null else o.optString(k, "").trim().ifEmpty { null }

        /**
         * Parses and checks settings from the webview.
         *
         * @throws IllegalArgumentException naming the member, never echoing a value.
         */
        fun fromJson(o: JSONObject): GatewaySettings {
            val topics = o.optJSONArray("mqtt_topics")
            val list = mutableListOf<String>()
            if (topics != null) for (i in 0 until topics.length()) topics.optString(i).trim().takeIf { it.isNotEmpty() }?.let(list::add)
            val s = GatewaySettings(
                name = str(o, "name") ?: "phone",
                edgeId = str(o, "edge_id"),
                hubUrl = str(o, "hub_url"),
                caPem = str(o, "hub_ca_pem"),
                mqttUrl = str(o, "mqtt_url"),
                mqttTopics = list,
                mqttUsername = str(o, "mqtt_username"),
                httpPushEnabled = o.optBoolean("http_push_enabled", false),
                httpPushPort = o.optInt("http_push_port", 8787),
                syncRequiresCharging = o.optBoolean("sync_requires_charging", true),
                segmentMs = o.optLong("segment_ms", 300_000),
                maxLogBytes = o.optLong("max_log_bytes", 512L * 1024 * 1024),
                whenFull = str(o, "when_full") ?: "block",
            )
            s.validate()
            return s
        }

        fun mqttPassword(context: Context): String? =
            SecretBox.open(context.getSharedPreferences(SECRETS, Context.MODE_PRIVATE).getString(KEY_MQTT_PASSWORD, null))

        fun httpPushToken(context: Context): String? =
            SecretBox.open(context.getSharedPreferences(SECRETS, Context.MODE_PRIVATE).getString(KEY_PUSH_TOKEN, null))

        /** Stores a new secret; an empty value keeps the stored one. */
        fun putSecrets(context: Context, mqttPassword: String?, httpPushToken: String?) {
            val prefs = context.getSharedPreferences(SECRETS, Context.MODE_PRIVATE).edit()
            if (!mqttPassword.isNullOrEmpty()) prefs.putString(KEY_MQTT_PASSWORD, SecretBox.seal(mqttPassword))
            if (!httpPushToken.isNullOrEmpty()) prefs.putString(KEY_PUSH_TOKEN, SecretBox.seal(httpPushToken))
            prefs.apply()
        }
    }

    fun validate() {
        require(NAME.matches(name)) { "name must match [a-z0-9_-]{1,64}" }
        edgeId?.let { require(EDGE_ID.matches(it)) { "edge_id must be an edg_ id" } }
        hubUrl?.let {
            val u = URI(it)
            require(u.scheme == "https" && !u.host.isNullOrEmpty()) { "the hub URL must be https://host" }
            require(u.userInfo == null && u.rawQuery == null && u.rawFragment == null) {
                "the hub URL must not carry credentials, a query or a fragment"
            }
        }
        mqttUrl?.let {
            val u = URI(it)
            require(u.scheme in setOf("mqtt", "mqtts", "tcp", "ssl") && !u.host.isNullOrEmpty()) {
                "the broker must be mqtt://host[:port] or mqtts://host[:port]"
            }
            require(u.userInfo == null) { "the broker URL must not carry credentials; use the username and password fields" }
            require(mqttTopics.isNotEmpty() && mqttTopics.size <= 32) { "give 1 to 32 MQTT topics" }
            require(mqttTopics.all { t -> t.length <= 256 }) { "an MQTT topic filter is too long" }
        }
        require(httpPushPort in 1024..65535) { "the HTTP push port must be between 1024 and 65535" }
        require(segmentMs in 10_000..3_600_000) { "the segment length must be between 10 s and 1 h" }
        require(maxLogBytes >= 1024L * 1024) { "the log limit must be at least 1 MiB" }
        require(whenFull in WHEN_FULL) { "when_full must be block, drop_oldest or drop_newest" }
    }

    fun mqttTls(): Boolean = mqttUrl?.let { URI(it).scheme in setOf("mqtts", "ssl") } ?: false

    fun save(context: Context) {
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).edit()
            .putString(KEY_SETTINGS, toJson().toString())
            .apply()
    }

    /** Everything except secrets, in the webview's member names. */
    fun toJson(): JSONObject = JSONObject()
        .put("name", name)
        .put("edge_id", edgeId ?: JSONObject.NULL)
        .put("hub_url", hubUrl ?: JSONObject.NULL)
        .put("hub_ca_pem", caPem ?: JSONObject.NULL)
        .put("mqtt_url", mqttUrl ?: JSONObject.NULL)
        .put("mqtt_topics", JSONArray(mqttTopics))
        .put("mqtt_username", mqttUsername ?: JSONObject.NULL)
        .put("http_push_enabled", httpPushEnabled)
        .put("http_push_port", httpPushPort)
        .put("sync_requires_charging", syncRequiresCharging)
        .put("segment_ms", segmentMs)
        .put("max_log_bytes", maxLogBytes)
        .put("when_full", whenFull)

    fun publicJson(context: Context): JSONObject = toJson()
        .put("mqtt_password_set", mqttPassword(context) != null)
        .put("http_push_token_set", httpPushToken(context) != null)

    /** The Rust `CoreConfig` (`apps/studio/plugins/gateway/src/config.rs`); it refuses unknown members. */
    fun coreConfigJson(): JSONObject {
        val o = JSONObject()
            .put("name", name)
            .put("segment_ms", segmentMs)
            .put("grace_ms", minOf(30_000L, segmentMs))
            .put("max_log_bytes", maxLogBytes)
            .put("when_full", whenFull)
        edgeId?.let { o.put("edge_id", it) }
        return o
    }
}
