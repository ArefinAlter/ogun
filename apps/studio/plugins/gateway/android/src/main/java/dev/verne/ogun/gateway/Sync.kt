package dev.verne.ogun.gateway

import android.app.job.JobInfo
import android.app.job.JobParameters
import android.app.job.JobScheduler
import android.app.job.JobService
import android.content.ComponentName
import android.content.Context
import org.json.JSONObject
import java.io.IOException
import java.io.InputStream
import java.net.URL
import javax.net.ssl.HttpsURLConnection

/** A hub's HTTP error, by status and O-00 code; never the response body or the URL. */
internal class HubException(val status: Int, val code: String) : IOException("hub answered HTTP $status ($code)")

/** POSTs JSON to the hub's REST routes (`/api/v1/sync/offer`, `/api/v1/sync/chunk`) over HTTPS. */
internal class HubClient(base: String, caPem: String?) {
    private val base = base.trimEnd('/') + "/api/v1/"
    private val factory = Tls.socketFactory(caPem)

    private fun readBounded(stream: InputStream?, max: Int): String {
        if (stream == null) return ""
        stream.use {
            val buf = java.io.ByteArrayOutputStream()
            val chunk = ByteArray(16 * 1024)
            while (true) {
                val n = it.read(chunk)
                if (n < 0) break
                buf.write(chunk, 0, n)
                if (buf.size() > max) throw IOException("the hub's answer is too large")
            }
            return buf.toString("UTF-8")
        }
    }

    fun post(path: String, body: JSONObject): JSONObject {
        val url = URL(base + path)
        if (url.protocol != "https") throw IOException("the hub must be https")
        val conn = url.openConnection() as HttpsURLConnection
        try {
            conn.sslSocketFactory = factory
            conn.instanceFollowRedirects = false
            conn.connectTimeout = 15_000
            conn.readTimeout = 60_000
            conn.requestMethod = "POST"
            conn.doOutput = true
            conn.setRequestProperty("Content-Type", "application/json")
            conn.setRequestProperty("Accept", "application/json")
            val bytes = body.toString().toByteArray(Charsets.UTF_8)
            conn.setFixedLengthStreamingMode(bytes.size)
            conn.outputStream.use { it.write(bytes) }
            val status = conn.responseCode
            if (status !in 200..299) {
                val code = try {
                    JSONObject(readBounded(conn.errorStream, 64 * 1024)).optJSONObject("error")?.optString("code") ?: "http"
                } catch (e: Exception) {
                    "http"
                }
                throw HubException(status, code)
            }
            return JSONObject(readBounded(conn.inputStream, 4 * 1024 * 1024))
        } finally {
            conn.disconnect()
        }
    }
}

/**
 * One sync round (O-11): seal, offer the oldest pending segments, upload the chunks the hub wants, offer again to
 * collect acknowledgements, and repeat until nothing is pending or no progress is made. Acknowledged segments are
 * deleted and their ingest-log records released by the core.
 */
internal object SyncRunner {
    private const val MAX_ROUNDS = 8

    private fun note(ok: Boolean, reason: String) {
        try {
            Core.recordSync(ok, reason)
        } catch (_: Exception) {
            // The outcome is informational; a failure to record it must not fail the job.
        }
    }

    /** Returns true when the job should be retried later (a network or hub failure). */
    @Synchronized
    fun run(context: Context): Boolean {
        val settings = GatewaySettings.load(context)
        try {
            Core.open(context, settings)
            Core.seal(System.currentTimeMillis())
        } catch (e: GatewayException) {
            return false
        }
        if (settings.edgeId == null) {
            note(false, "not enrolled on a hub")
            return false
        }
        val hubUrl = settings.hubUrl ?: run {
            note(false, "no hub configured")
            return false
        }
        return try {
            val hub = HubClient(hubUrl, settings.caPem)
            var uploaded = 0
            var acked = 0L
            var rounds = 0
            while (rounds++ < MAX_ROUNDS) {
                if (Thread.currentThread().isInterrupted) throw InterruptedException()
                val offer = Core.offer() ?: break
                val want = hub.post("sync/offer", offer).optJSONObject("want") ?: JSONObject()
                val plan = Core.applyWant(want)
                acked += plan.optLong("acked")
                val uploads = plan.optJSONArray("uploads")
                if ((uploads == null || uploads.length() == 0) && plan.optLong("acked") == 0L) break
                if (uploads != null) {
                    for (i in 0 until uploads.length()) {
                        val u = uploads.getJSONObject(i)
                        val digest = u.getString("blake3")
                        for (index in u.getInt("from_index") until u.getInt("total")) {
                            if (Thread.currentThread().isInterrupted) throw InterruptedException()
                            hub.post("sync/chunk", Core.chunk(digest, index))
                            uploaded++
                        }
                    }
                }
            }
            note(true, "acknowledged $acked segments, sent $uploaded chunks")
            false
        } catch (e: InterruptedException) {
            note(false, "stopped by Android")
            true
        } catch (e: HubException) {
            note(false, "hub: HTTP ${e.status} ${e.code}")
            e.status >= 500 || e.status == 429
        } catch (e: GatewayException) {
            note(false, e.code)
            false
        } catch (e: Exception) {
            note(false, "network: ${e.javaClass.simpleName}")
            true
        }
    }
}

/** Schedules sync with JobScheduler: unmetered network only, and while charging unless the person turned that off. */
internal object SyncScheduler {
    private const val PERIODIC = 0x6f67
    private const val NOW = 0x6f68

    private fun component(context: Context) = ComponentName(context, SyncJobService::class.java)

    fun schedulePeriodic(context: Context, requiresCharging: Boolean) {
        val job = JobInfo.Builder(PERIODIC, component(context))
            .setRequiredNetworkType(JobInfo.NETWORK_TYPE_UNMETERED)
            .setRequiresCharging(requiresCharging)
            .setPeriodic(15 * 60 * 1000L)
            .build()
        context.getSystemService(JobScheduler::class.java)?.schedule(job)
    }

    /** A one-off sync as soon as an unmetered network is available (charging not required). */
    fun runSoon(context: Context) {
        val job = JobInfo.Builder(NOW, component(context))
            .setRequiredNetworkType(JobInfo.NETWORK_TYPE_UNMETERED)
            .build()
        context.getSystemService(JobScheduler::class.java)?.schedule(job)
    }
}

/** Runs [SyncRunner] off the main thread; Android may stop it at any time, and it resumes from the outbox. */
class SyncJobService : JobService() {
    @Volatile
    private var worker: Thread? = null

    override fun onStartJob(params: JobParameters): Boolean {
        val t = Thread({
            val retry = try {
                SyncRunner.run(applicationContext)
            } catch (e: Exception) {
                true
            }
            jobFinished(params, retry)
        }, "ogun-sync")
        worker = t
        t.start()
        return true
    }

    override fun onStopJob(params: JobParameters): Boolean {
        worker?.interrupt()
        return true
    }
}
