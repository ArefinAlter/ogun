package dev.verne.ogun.gateway

import java.io.ByteArrayInputStream
import java.net.InetSocketAddress
import java.net.Socket
import java.security.KeyStore
import java.security.cert.CertificateFactory
import javax.net.ssl.SSLContext
import javax.net.ssl.SSLSocket
import javax.net.ssl.SSLSocketFactory
import javax.net.ssl.TrustManagerFactory

/** TLS for the hub and the broker: the system trust store, or exactly the private CA a person pinned. */
internal object Tls {
    fun socketFactory(caPem: String?): SSLSocketFactory {
        if (caPem.isNullOrBlank()) return SSLSocketFactory.getDefault() as SSLSocketFactory
        val certs = CertificateFactory.getInstance("X.509")
            .generateCertificates(ByteArrayInputStream(caPem.trim().toByteArray(Charsets.US_ASCII)))
        require(certs.isNotEmpty()) { "the pinned CA has no certificate" }
        val ks = KeyStore.getInstance(KeyStore.getDefaultType()).apply { load(null, null) }
        certs.forEachIndexed { i, c -> ks.setCertificateEntry("ca$i", c) }
        val tmf = TrustManagerFactory.getInstance(TrustManagerFactory.getDefaultAlgorithm()).apply { init(ks) }
        return SSLContext.getInstance("TLS").apply { init(null, tmf.trustManagers, null) }.socketFactory
    }

    /** A TCP connection upgraded to TLS with SNI and host name verification. */
    fun connect(host: String, port: Int, caPem: String?, timeoutMs: Int): SSLSocket {
        val plain = Socket()
        plain.connect(InetSocketAddress(host, port), timeoutMs)
        val ssl = socketFactory(caPem).createSocket(plain, host, port, true) as SSLSocket
        val params = ssl.sslParameters
        params.endpointIdentificationAlgorithm = "HTTPS"
        ssl.sslParameters = params
        ssl.startHandshake()
        return ssl
    }
}
