package com.skadi.pairing

import android.content.Context
import android.net.nsd.NsdManager
import android.net.nsd.NsdServiceInfo
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.withTimeoutOrNull

/**
 * Best-effort mDNS/NSD discovery of skadi on the LAN (SKADI-I-0049, ported
 * from squire's NsdDiscovery): browse `_skadi._tcp` and resolve the first
 * responder's host:port, or null on timeout. Strictly a convenience to
 * prefill the host — the pairing screen always offers manual entry, and the
 * server side of this (mDNS advertisement) lands in SKADI-T-0340.
 */
class NsdDiscovery(context: Context) {
    private val nsd =
        context.applicationContext.getSystemService(Context.NSD_SERVICE) as NsdManager

    suspend fun discover(timeoutMs: Long = 4000): Pair<String, Int>? =
        withTimeoutOrNull(timeoutMs) {
            val result = CompletableDeferred<Pair<String, Int>?>()
            val listener = object : NsdManager.DiscoveryListener {
                override fun onDiscoveryStarted(serviceType: String) {}
                override fun onServiceFound(info: NsdServiceInfo) {
                    @Suppress("DEPRECATION")
                    nsd.resolveService(info, object : NsdManager.ResolveListener {
                        override fun onResolveFailed(s: NsdServiceInfo, errorCode: Int) {}
                        override fun onServiceResolved(s: NsdServiceInfo) {
                            @Suppress("DEPRECATION")
                            val host = s.host?.hostAddress
                            if (host != null && !result.isCompleted) result.complete(host to s.port)
                        }
                    })
                }
                override fun onServiceLost(info: NsdServiceInfo) {}
                override fun onDiscoveryStopped(serviceType: String) {}
                override fun onStartDiscoveryFailed(serviceType: String, errorCode: Int) {
                    if (!result.isCompleted) result.complete(null)
                }
                override fun onStopDiscoveryFailed(serviceType: String, errorCode: Int) {}
            }
            try {
                nsd.discoverServices(SERVICE_TYPE, NsdManager.PROTOCOL_DNS_SD, listener)
                result.await()
            } finally {
                runCatching { nsd.stopServiceDiscovery(listener) }
            }
        }

    private companion object {
        const val SERVICE_TYPE = "_skadi._tcp."
    }
}
