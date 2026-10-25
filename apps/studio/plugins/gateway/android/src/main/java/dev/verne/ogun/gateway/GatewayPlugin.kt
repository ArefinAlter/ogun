package dev.verne.ogun.gateway

import android.Manifest
import android.app.Activity
import android.content.pm.PackageManager
import android.os.Build
import app.tauri.annotation.Command
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import org.json.JSONObject
import java.util.concurrent.Executors

/**
 * The Studio side of the field gateway: `plugin:ogun-gateway|status`, `configure`, `start`, `stop`, `sync_now`.
 * Tauri forwards these commands here because the Rust plugin does not handle them on Android, so a broker password
 * or a device token goes from the webview straight to the Android Keystore. Results never contain a secret.
 */
@TauriPlugin
class GatewayPlugin(private val activity: Activity) : Plugin(activity) {
    private val work = Executors.newSingleThreadExecutor()

    private fun background(invoke: Invoke, block: () -> JSONObject) {
        work.execute {
            try {
                invoke.resolve(JSObject(block().toString()))
            } catch (e: IllegalArgumentException) {
                invoke.reject(e.message ?: "invalid gateway settings", "api.invalid_params")
            } catch (e: GatewayException) {
                invoke.reject(e.message ?: e.code, e.code)
            } catch (e: Exception) {
                invoke.reject("the gateway failed: ${e.javaClass.simpleName}", "internal.bug")
            }
        }
    }

    private fun statusJson(): JSONObject {
        val ctx = activity.applicationContext
        val settings = GatewaySettings.load(ctx)
        val o = JSONObject()
            .put("running", GatewayState.running)
            .put("settings", settings.publicJson(ctx))
        GatewayState.stoppedReason?.let { o.put("stopped_reason", it) }
        if (settings.mqttUrl != null) {
            val m = JSONObject()
                .put("connected", GatewayState.mqttConnected && GatewayState.running)
                .put("received", GatewayState.mqttReceived.get())
            GatewayState.mqttError?.let { m.put("last_error", it) }
            o.put("mqtt", m)
        }
        if (settings.httpPushEnabled) {
            val h = JSONObject()
                .put("listening", GatewayState.pushListening && GatewayState.running)
                .put("received", GatewayState.pushReceived.get())
            GatewayState.pushAddress?.let { h.put("address", it) }
            o.put("http_push", h)
        }
        try {
            Core.open(ctx, settings)
            o.put("core", Core.status())
        } catch (e: GatewayException) {
            o.put("core_error", e.envelope())
        }
        return o
    }

    @Command
    fun status(invoke: Invoke) = background(invoke) { statusJson() }

    @Command
    fun configure(invoke: Invoke) = background(invoke) {
        val args = invoke.getArgs()
        val raw = args.optJSONObject("settings") ?: throw IllegalArgumentException("settings are missing")
        val settings = GatewaySettings.fromJson(raw)
        val ctx = activity.applicationContext
        GatewaySettings.putSecrets(ctx, raw.optString("mqtt_password", ""), raw.optString("http_push_token", ""))
        if (settings.mqttUrl != null && !settings.mqttTls() && GatewaySettings.mqttPassword(ctx) != null) {
            throw IllegalArgumentException("a broker password needs mqtts://")
        }
        settings.save(ctx)
        // Reopen with the new configuration (log limit, policy, edge id); listeners pick it up on restart.
        Core.open(ctx, settings)
        if (GatewayState.running) SyncScheduler.schedulePeriodic(ctx, settings.syncRequiresCharging)
        statusJson()
    }

    @Command
    fun start(invoke: Invoke) {
        if (Build.VERSION.SDK_INT >= 33 &&
            activity.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED
        ) {
            // The service runs without it, but the person should see the notification that says it runs.
            activity.requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), 0x6f67)
        }
        val ctx = activity.applicationContext
        try {
            GatewaySettings.load(ctx).validate()
            GatewayService.start(ctx)
        } catch (e: IllegalArgumentException) {
            invoke.reject(e.message ?: "invalid gateway settings", "api.invalid_params")
            return
        } catch (e: Exception) {
            invoke.reject("Android did not start the service: ${e.javaClass.simpleName}", "api.unavailable")
            return
        }
        background(invoke) { statusJson() }
    }

    @Command
    fun stop(invoke: Invoke) {
        // Collection stops; the periodic sync job stays scheduled so sealed segments still reach the hub.
        GatewayService.stop(activity.applicationContext)
        background(invoke) { statusJson() }
    }

    @Command
    fun syncNow(invoke: Invoke) {
        SyncScheduler.runSoon(activity.applicationContext)
        background(invoke) { statusJson() }
    }
}
