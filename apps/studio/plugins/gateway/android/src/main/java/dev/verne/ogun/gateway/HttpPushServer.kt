package dev.verne.ogun.gateway

import java.io.BufferedInputStream
import java.io.IOException
import java.io.InputStream
import java.io.OutputStream
import java.net.Inet4Address
import java.net.Inet6Address
import java.net.InetAddress
import java.net.InetSocketAddress
import java.net.ServerSocket
import java.net.Socket
import java.security.MessageDigest
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors

/**
 * HTTP push from devices on the local network (O-10 section 2, `stream.http`): `POST /ingest` with a JSON or NDJSON
 * body. The reply is `202` only after the records committed to the ingest log, or `503` with `Retry-After` while a
 * `block` log is full. Only private, link-local and loopback peers are served, and when a device token is set the
 * request must carry `Authorization: Bearer <token>`. Plain HTTP: the device token protects against other devices
 * on the LAN, not against someone who can read LAN traffic.
 */
internal class HttpPushServer(
    private val port: Int,
    private val token: String?,
    private val listener: Listener,
) : Thread("ogun-http-push") {
    interface Listener {
        fun onPushState(listening: Boolean, error: String?)
        fun onPushReceived(count: Long)
    }

    companion object {
        const val PATH = "/ingest"
        private const val MAX_BODY = 1024 * 1024
        private const val MAX_LINE = 8192
        private const val MAX_HEADERS = 64
    }

    @Volatile
    private var running = true
    private var server: ServerSocket? = null
    private val pool: ExecutorService = Executors.newFixedThreadPool(4)

    fun shutdown() {
        running = false
        try {
            server?.close()
        } catch (_: IOException) {
        }
        pool.shutdownNow()
    }

    override fun run() {
        try {
            val s = ServerSocket()
            s.reuseAddress = true
            s.bind(InetSocketAddress(port), 32)
            server = s
            listener.onPushState(true, null)
            while (running) {
                val c = s.accept()
                pool.execute { handle(c) }
            }
        } catch (e: IOException) {
            if (running) listener.onPushState(false, "cannot listen on port $port: ${e.javaClass.simpleName}")
        } finally {
            listener.onPushState(false, null)
        }
    }

    private fun lanPeer(a: InetAddress): Boolean = when (a) {
        is Inet4Address, is Inet6Address ->
            a.isSiteLocalAddress || a.isLinkLocalAddress || a.isLoopbackAddress ||
                (a is Inet6Address && (a.address[0].toInt() and 0xfe) == 0xfc)
        else -> false
    }

    private fun handle(c: Socket) {
        c.use { sock ->
            try {
                sock.soTimeout = 10_000
                val out = sock.getOutputStream()
                if (!lanPeer(sock.inetAddress)) return respond(out, 403, "{\"error\":\"local network only\"}")
                val input = BufferedInputStream(sock.getInputStream())
                val requestLine = readLine(input) ?: return
                val parts = requestLine.split(' ')
                if (parts.size != 3) return respond(out, 400, "{\"error\":\"bad request line\"}")
                val headers = HashMap<String, String>()
                for (i in 0..MAX_HEADERS) {
                    val line = readLine(input) ?: return
                    if (line.isEmpty()) break
                    if (i == MAX_HEADERS) return respond(out, 431, "{\"error\":\"too many headers\"}")
                    val colon = line.indexOf(':')
                    if (colon <= 0) return respond(out, 400, "{\"error\":\"bad header\"}")
                    headers[line.substring(0, colon).trim().lowercase()] = line.substring(colon + 1).trim()
                }
                val path = parts[1].substringBefore('?')
                if (path != PATH) return respond(out, 404, "{\"error\":\"POST $PATH\"}")
                if (parts[0] != "POST") return respond(out, 405, "{\"error\":\"POST only\"}", "Allow: POST\r\n")
                if (token != null) {
                    val given = headers["authorization"]?.removePrefix("Bearer ")?.trim() ?: ""
                    val ok = MessageDigest.isEqual(given.toByteArray(Charsets.UTF_8), token.toByteArray(Charsets.UTF_8))
                    if (!ok) return respond(out, 401, "{\"error\":\"device token required\"}")
                }
                if (headers.containsKey("transfer-encoding")) return respond(out, 411, "{\"error\":\"send Content-Length\"}")
                val length = headers["content-length"]?.toIntOrNull()
                    ?: return respond(out, 411, "{\"error\":\"send Content-Length\"}")
                if (length < 0 || length > MAX_BODY) return respond(out, 413, "{\"error\":\"at most 1 MiB per request\"}")
                val type = headers["content-type"]?.substringBefore(';')?.trim()?.lowercase() ?: ""
                val ndjson = when (type) {
                    "application/json" -> false
                    "application/x-ndjson", "application/ndjson" -> true
                    else -> return respond(out, 415, "{\"error\":\"application/json or application/x-ndjson\"}")
                }
                val body = ByteArray(length)
                var read = 0
                while (read < length) {
                    val n = input.read(body, read, length - read)
                    if (n < 0) return
                    read += n
                }
                val origin = "http:" + (sock.inetAddress.hostAddress ?: "lan")
                try {
                    val r = Core.appendBody(origin, System.currentTimeMillis(), String(body, Charsets.UTF_8), ndjson)
                    val written = r.optLong("written") + r.optLong("dropped_newest")
                    listener.onPushReceived(written + r.optLong("invalid"))
                    respond(out, 202, "{\"accepted\":${r.optLong("written")},\"invalid\":${r.optLong("invalid")}}")
                } catch (e: GatewayException) {
                    if (e.code == "stream.full") {
                        respond(out, 503, "{\"error\":\"ingest log full\"}", "Retry-After: 5\r\n")
                    } else {
                        respond(out, 500, "{\"error\":\"${e.code}\"}")
                    }
                }
            } catch (_: IOException) {
                // The device went away; nothing was acknowledged.
            }
        }
    }

    private fun readLine(input: InputStream): String? {
        val buf = StringBuilder()
        while (true) {
            val b = input.read()
            if (b < 0) return null
            if (b == '\n'.code) return buf.toString().trimEnd('\r')
            if (buf.length >= MAX_LINE) throw IOException("line too long")
            buf.append(b.toChar())
        }
    }

    private fun respond(out: OutputStream, status: Int, json: String, extra: String = "") {
        val reason = when (status) {
            202 -> "Accepted"; 400 -> "Bad Request"; 401 -> "Unauthorized"; 403 -> "Forbidden"; 404 -> "Not Found"
            405 -> "Method Not Allowed"; 411 -> "Length Required"; 413 -> "Payload Too Large"
            415 -> "Unsupported Media Type"; 431 -> "Request Header Fields Too Large"; 503 -> "Service Unavailable"
            else -> "Error"
        }
        val bytes = json.toByteArray(Charsets.UTF_8)
        val head = "HTTP/1.1 $status $reason\r\nContent-Type: application/json\r\nContent-Length: ${bytes.size}\r\n" +
            "Connection: close\r\n$extra\r\n"
        out.write(head.toByteArray(Charsets.US_ASCII))
        out.write(bytes)
        out.flush()
    }
}
