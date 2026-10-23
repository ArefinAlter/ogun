package dev.verne.ogun.gateway

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.net.ConnectivityManager
import android.os.Build
import android.os.IBinder
import java.net.Inet4Address
import java.util.concurrent.Executors
import java.util.concurrent.ScheduledExecutorService
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicLong

/** What the service is doing, for the Studio page. In-process only. */
internal object GatewayState {
    @Volatile var running = false
    @Volatile var stoppedReason: String? = null
    @Volatile var mqttConfigured = false
    @Volatile var mqttConnected = false
    @Volatile var mqttError: String? = null
    val mqttReceived = AtomicLong()
    @Volatile var pushEnabled = false
    @Volatile var pushListening = false
    @Volatile var pushAddress: String? = null
    @Volatile var pushError: String? = null
    val pushReceived = AtomicLong()
}

/**
 * The field gateway's foreground service (`dataSync`). It runs only while its notification shows: it subscribes to
 * the configured broker, serves HTTP push on the local network when enabled, seals the ingest log every 30 s, and
 * schedules the sync job. It stops itself when Android 15's daily limit for data-sync services is reached
 * ([onTimeout]); Android may also stop it at any time, and nothing is lost: unacknowledged MQTT messages stay at the
 * broker, and committed records stay in the log.
 */
class GatewayService : Service() {
    companion object {
        private const val CHANNEL = "ogun-gateway"
        private const val NOTIFICATION_ID = 0x6f67
        private const val ACTION_START = "dev.verne.ogun.gateway.START"
        private const val ACTION_STOP = "dev.verne.ogun.gateway.STOP"

        fun start(context: Context) {
            val i = Intent(context, GatewayService::class.java).setAction(ACTION_START)
            context.startForegroundService(i)
        }

        fun stop(context: Context) {
            val i = Intent(context, GatewayService::class.java).setAction(ACTION_STOP)
            context.startService(i)
        }
    }

    private var mqtt: MqttSubscriber? = null
    private var push: HttpPushServer? = null
    private var timer: ScheduledExecutorService? = null

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == ACTION_STOP) {
            halt("stopped in Ogun Studio")
            return START_NOT_STICKY
        }
        try {
            enterForeground()
        } catch (e: Exception) {
            // Android 12+ refuses to start a foreground service from the background (for example a sticky restart).
            GatewayState.stoppedReason = "Android did not allow the service to start; open Ogun Studio to start it"
            stopSelf()
            return START_NOT_STICKY
        }
        if (!GatewayState.running) begin()
        return START_STICKY
    }

    private fun channel() {
        val nm = getSystemService(NotificationManager::class.java)
        if (nm.getNotificationChannel(CHANNEL) == null) {
            val ch = NotificationChannel(CHANNEL, "Field gateway", NotificationManager.IMPORTANCE_LOW)
            ch.description = "Shown while Ogun Studio collects from sensors on the local network"
            nm.createNotificationChannel(ch)
        }
    }

    private fun notification(text: String): Notification {
        val launch = packageManager.getLaunchIntentForPackage(packageName)
        val content = launch?.let {
            PendingIntent.getActivity(this, 0, it, PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT)
        }
        return Notification.Builder(this, CHANNEL)
            .setSmallIcon(android.R.drawable.stat_notify_sync)
            .setContentTitle("Ogun field gateway")
            .setContentText(text)
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .apply { if (content != null) setContentIntent(content) }
            .build()
    }

    private fun enterForeground() {
        channel()
        val n = notification("Collecting from sensors on the local network")
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            startForeground(NOTIFICATION_ID, n, ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC)
        } else {
            startForeground(NOTIFICATION_ID, n)
        }
    }

    private fun updateNotification() {
        val text = buildString {
            append(if (GatewayState.mqttConnected) "MQTT connected" else if (GatewayState.mqttConfigured) "MQTT reconnecting" else "MQTT off")
            if (GatewayState.pushEnabled) append(if (GatewayState.pushListening) " · HTTP push on" else " · HTTP push off")
            append(" · ${GatewayState.mqttReceived.get() + GatewayState.pushReceived.get()} received")
        }
        getSystemService(NotificationManager::class.java).notify(NOTIFICATION_ID, notification(text))
    }

    private fun wifiAddress(): String? {
        val cm = getSystemService(ConnectivityManager::class.java) ?: return null
        val props = cm.getLinkProperties(cm.activeNetwork) ?: return null
        return props.linkAddresses.map { it.address }.firstOrNull { it is Inet4Address && it.isSiteLocalAddress }?.hostAddress
    }

    private fun begin() {
        val settings = GatewaySettings.load(this)
        try {
            Core.open(this, settings)
        } catch (e: GatewayException) {
            halt("the gateway core did not open: ${e.code}")
            return
        }
        GatewayState.running = true
        GatewayState.stoppedReason = null
        GatewayState.mqttConfigured = settings.mqttUrl != null
        GatewayState.pushEnabled = settings.httpPushEnabled
        if (settings.mqttUrl != null) {
            mqtt = MqttSubscriber(settings, GatewaySettings.mqttPassword(this), mqttListener).also { it.start() }
        }
        if (settings.httpPushEnabled) {
            push = HttpPushServer(settings.httpPushPort, GatewaySettings.httpPushToken(this), pushListener).also { it.start() }
            GatewayState.pushAddress = wifiAddress()?.let { "$it:${settings.httpPushPort}" }
        }
        timer = Executors.newSingleThreadScheduledExecutor().also {
            it.scheduleWithFixedDelay({
                try {
                    Core.seal(System.currentTimeMillis())
                } catch (_: Exception) {
                    // Sealing retries on the next tick; the status page shows the core's counters.
                }
                updateNotification()
            }, 30, 30, TimeUnit.SECONDS)
        }
        SyncScheduler.schedulePeriodic(this, settings.syncRequiresCharging)
    }

    /** Android 15: the daily time limit for `dataSync` foreground services was reached. */
    override fun onTimeout(startId: Int, fgsType: Int) {
        halt("Android's daily limit for data-sync services was reached (6 hours in 24)")
    }

    override fun onDestroy() {
        shutdown()
        super.onDestroy()
    }

    private fun halt(reason: String) {
        GatewayState.stoppedReason = reason
        shutdown()
        stopForeground(STOP_FOREGROUND_REMOVE)
        stopSelf()
    }

    private fun shutdown() {
        if (!GatewayState.running && mqtt == null && push == null) return
        GatewayState.running = false
        mqtt?.shutdown()
        mqtt = null
        push?.shutdown()
        push = null
        timer?.shutdownNow()
        timer = null
        try {
            Core.seal(System.currentTimeMillis())
        } catch (_: Exception) {
        }
        GatewayState.mqttConnected = false
        GatewayState.pushListening = false
    }

    private val mqttListener = object : MqttSubscriber.Listener {
        override fun onMqttState(connected: Boolean, error: String?) {
            GatewayState.mqttConnected = connected
            if (error != null || connected) GatewayState.mqttError = error
        }

        override fun onMqttReceived(count: Int) {
            GatewayState.mqttReceived.addAndGet(count.toLong())
        }
    }

    private val pushListener = object : HttpPushServer.Listener {
        override fun onPushState(listening: Boolean, error: String?) {
            GatewayState.pushListening = listening
            if (error != null || listening) GatewayState.pushError = error
        }

        override fun onPushReceived(count: Long) {
            GatewayState.pushReceived.addAndGet(count)
        }
    }
}
