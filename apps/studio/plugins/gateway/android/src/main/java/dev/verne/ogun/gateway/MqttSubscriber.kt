package dev.verne.ogun.gateway

import org.json.JSONArray
import org.json.JSONObject
import java.io.BufferedInputStream
import java.io.BufferedOutputStream
import java.io.ByteArrayOutputStream
import java.io.DataInputStream
import java.io.EOFException
import java.io.IOException
import java.io.InputStream
import java.io.OutputStream
import java.net.InetSocketAddress
import java.net.Socket
import java.net.SocketTimeoutException
import java.net.URI

/**
 * A minimal MQTT 3.1.1 subscriber (O-10 section 2, `stream.mqtt`): QoS 1, a persistent session (clean session off,
 * a stable client id) so the broker queues messages while the phone is dozing or the log is full, and the PUBACK
 * sent only after the batch holding the message has committed to the ingest log. Messages are batched for up to
 * 200 ms or 1 000 records, so the log is not written once per message.
 */
internal class MqttSubscriber(
    private val settings: GatewaySettings,
    private val password: String?,
    private val listener: Listener,
) : Thread("ogun-mqtt") {
    interface Listener {
        fun onMqttState(connected: Boolean, error: String?)
        fun onMqttReceived(count: Int)
    }

    private class Packet(val type: Int, val flags: Int, val body: ByteArray)

    private class Pending(val packetId: Int?, val record: JSONObject)

    companion object {
        private const val KEEPALIVE_S = 60
        private const val BATCH_MS = 200L
        private const val BATCH_MAX = 1000
        private const val MAX_PACKET = 1024 * 1024
        private const val CONNECT_TIMEOUT_MS = 15_000
    }

    @Volatile
    private var running = true

    @Volatile
    private var socket: Socket? = null
    private var nextId = 1

    fun shutdown() {
        running = false
        try {
            socket?.close()
        } catch (_: IOException) {
        }
        interrupt()
    }

    override fun run() {
        var backoff = 1_000L
        while (running) {
            try {
                session()
                backoff = 1_000L
            } catch (e: GatewayException) {
                listener.onMqttState(false, if (e.code == "stream.full") "ingest log full; not acknowledging" else e.code)
            } catch (e: Exception) {
                if (running) listener.onMqttState(false, describe(e))
            } finally {
                try {
                    socket?.close()
                } catch (_: IOException) {
                }
                socket = null
            }
            if (!running) break
            try {
                sleep(backoff)
            } catch (_: InterruptedException) {
                break
            }
            backoff = (backoff * 2).coerceAtMost(60_000L)
        }
        listener.onMqttState(false, null)
    }

    /** A short reason without the broker address or credentials. */
    private fun describe(e: Exception): String = when (e) {
        is SocketTimeoutException -> "the broker did not answer"
        is java.net.ConnectException -> "cannot connect to the broker"
        is java.net.UnknownHostException -> "unknown broker host"
        is javax.net.ssl.SSLException -> "TLS failed: ${e.javaClass.simpleName}"
        is BrokerRefused -> e.message ?: "the broker refused the connection"
        else -> e.javaClass.simpleName
    }

    private class BrokerRefused(message: String) : IOException(message)

    private fun session() {
        val uri = URI(settings.mqttUrl ?: throw IOException("no broker configured"))
        val host = uri.host ?: throw IOException("no broker host")
        val tls = settings.mqttTls()
        val port = if (uri.port > 0) uri.port else if (tls) 8883 else 1883
        if (!tls && password != null) throw BrokerRefused("a broker password needs mqtts://")
        val s: Socket = if (tls) {
            Tls.connect(host, port, settings.caPem, CONNECT_TIMEOUT_MS)
        } else {
            Socket().apply { connect(InetSocketAddress(host, port), CONNECT_TIMEOUT_MS) }
        }
        socket = s
        s.tcpNoDelay = true
        s.soTimeout = 250
        val input = DataInputStream(BufferedInputStream(s.getInputStream()))
        val out = BufferedOutputStream(s.getOutputStream())

        sendConnect(out)
        val connack = readBlocking(input, 10_000)
        if (connack.type != 2 || connack.body.size < 2) throw BrokerRefused("unexpected answer to CONNECT")
        val rc = connack.body[1].toInt() and 0xff
        if (rc != 0) throw BrokerRefused("the broker refused the connection (code $rc)")

        val subId = sendSubscribe(out)
        val suback = readBlocking(input, 10_000)
        if (suback.type != 9) throw BrokerRefused("unexpected answer to SUBSCRIBE")
        val ackId = ((suback.body[0].toInt() and 0xff) shl 8) or (suback.body[1].toInt() and 0xff)
        if (ackId != subId) throw BrokerRefused("SUBACK for another packet")
        if ((2 until suback.body.size).any { (suback.body[it].toInt() and 0xff) == 0x80 }) {
            throw BrokerRefused("the broker refused a topic subscription")
        }
        listener.onMqttState(true, null)

        val pending = ArrayList<Pending>()
        var batchStart = 0L
        var lastSent = System.currentTimeMillis()
        var pingOutstanding = false
        while (running) {
            val p = readPacket(input)
            val now = System.currentTimeMillis()
            if (p != null) {
                when (p.type) {
                    3 -> {
                        val pub = parsePublish(p)
                        if (pending.isEmpty()) batchStart = now
                        pending.add(pub)
                        listener.onMqttReceived(1)
                    }
                    13 -> pingOutstanding = false
                    else -> {}
                }
            }
            if (pending.isNotEmpty() && (pending.size >= BATCH_MAX || now - batchStart >= BATCH_MS)) {
                flush(pending, out)
                lastSent = System.currentTimeMillis()
            }
            if (now - lastSent >= KEEPALIVE_S * 1000L / 2) {
                if (pingOutstanding) throw IOException("the broker stopped answering pings")
                out.write(byteArrayOf(0xC0.toByte(), 0))
                out.flush()
                pingOutstanding = true
                lastSent = now
            }
        }
        if (pending.isNotEmpty()) flush(pending, out)
        try {
            out.write(byteArrayOf(0xE0.toByte(), 0))
            out.flush()
        } catch (_: IOException) {
        }
    }

    /** Appends the batch durably, then acknowledges every QoS 1 message in it. Throws (no acks) when the log is full. */
    private fun flush(pending: MutableList<Pending>, out: OutputStream) {
        val batch = JSONArray()
        for (p in pending) batch.put(p.record)
        Core.append(batch)
        for (p in pending) {
            val id = p.packetId ?: continue
            out.write(byteArrayOf(0x40, 2, (id shr 8).toByte(), id.toByte()))
        }
        out.flush()
        pending.clear()
    }

    private fun parsePublish(p: Packet): Pending {
        val qos = (p.flags shr 1) and 3
        val b = p.body
        val topicLen = ((b[0].toInt() and 0xff) shl 8) or (b[1].toInt() and 0xff)
        if (2 + topicLen > b.size) throw IOException("malformed PUBLISH")
        val topic = String(b, 2, topicLen, Charsets.UTF_8)
        var off = 2 + topicLen
        var id: Int? = null
        if (qos > 0) {
            if (off + 2 > b.size) throw IOException("malformed PUBLISH")
            id = ((b[off].toInt() and 0xff) shl 8) or (b[off + 1].toInt() and 0xff)
            off += 2
        }
        val payload = String(b, off, b.size - off, Charsets.UTF_8)
        val record = JSONObject()
            .put("received_ms", System.currentTimeMillis())
            .put("origin", origin(topic))
            .put("payload", payload)
        return Pending(id, record)
    }

    /** `mqtt:<topic>`, without characters the core refuses in an origin label. */
    private fun origin(topic: String): String {
        val clean = topic.map { c -> if (c == '@' || c.isISOControl()) '_' else c }.joinToString("")
        return "mqtt:" + clean.take(240)
    }

    private fun clientId(): String = ("ogun-" + settings.name).take(23)

    private fun writeString(buf: ByteArrayOutputStream, s: String) {
        val bytes = s.toByteArray(Charsets.UTF_8)
        buf.write(bytes.size shr 8)
        buf.write(bytes.size and 0xff)
        buf.write(bytes)
    }

    private fun writePacket(out: OutputStream, header: Int, body: ByteArray) {
        out.write(header)
        var len = body.size
        do {
            var digit = len % 128
            len /= 128
            if (len > 0) digit = digit or 0x80
            out.write(digit)
        } while (len > 0)
        out.write(body)
        out.flush()
    }

    private fun sendConnect(out: OutputStream) {
        val body = ByteArrayOutputStream()
        writeString(body, "MQTT")
        body.write(4)
        var flags = 0 // clean session off: the broker keeps the subscription and queues QoS 1 messages
        val user = settings.mqttUsername
        if (user != null) flags = flags or 0x80
        if (user != null && password != null) flags = flags or 0x40
        body.write(flags)
        body.write(KEEPALIVE_S shr 8)
        body.write(KEEPALIVE_S and 0xff)
        writeString(body, clientId())
        if (user != null) writeString(body, user)
        if (user != null && password != null) writeString(body, password)
        writePacket(out, 0x10, body.toByteArray())
    }

    private fun sendSubscribe(out: OutputStream): Int {
        val id = nextId
        nextId = if (nextId >= 0xffff) 1 else nextId + 1
        val body = ByteArrayOutputStream()
        body.write(id shr 8)
        body.write(id and 0xff)
        for (t in settings.mqttTopics) {
            writeString(body, t)
            body.write(1)
        }
        writePacket(out, 0x82, body.toByteArray())
        return id
    }

    private fun readBlocking(input: DataInputStream, timeoutMs: Long): Packet {
        val deadline = System.currentTimeMillis() + timeoutMs
        while (System.currentTimeMillis() < deadline) {
            readPacket(input)?.let { return it }
        }
        throw SocketTimeoutException("no answer")
    }

    /** One packet, or null when nothing arrived within the socket timeout. A timeout mid-packet is an error. */
    private fun readPacket(input: DataInputStream): Packet? {
        val first = try {
            input.read()
        } catch (_: SocketTimeoutException) {
            return null
        }
        if (first < 0) throw EOFException("the broker closed the connection")
        var mult = 1
        var len = 0
        var i = 0
        while (true) {
            val d = readByte(input)
            len += (d and 0x7f) * mult
            if (d and 0x80 == 0) break
            mult *= 128
            if (++i >= 4) throw IOException("malformed length")
        }
        if (len > MAX_PACKET) throw IOException("packet too large")
        val body = ByteArray(len)
        try {
            input.readFully(body)
        } catch (e: SocketTimeoutException) {
            throw IOException("the broker stalled mid-packet")
        }
        return Packet((first shr 4) and 0x0f, first and 0x0f, body)
    }

    private fun readByte(input: InputStream): Int {
        val b = try {
            input.read()
        } catch (e: SocketTimeoutException) {
            throw IOException("the broker stalled mid-packet")
        }
        if (b < 0) throw EOFException("the broker closed the connection")
        return b
    }
}
