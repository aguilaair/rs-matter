package org.rsmatter.android

import android.content.Context
import android.net.nsd.NsdManager
import android.net.nsd.NsdServiceInfo
import android.util.Log
import java.util.ArrayDeque
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.Executors

/**
 * The Thread networks that border routers on this device's LAN advertise (`_meshcop._udp`, Thread
 * spec 8.4.2.2): a scan for Android devices without a Thread radio of their own. Answers
 * [MatterNode.Listener.onThreadScan] with [networks].
 *
 * The TXT records carry the network name (`nn`), the extended PAN ID (`xp`) and the border
 * router's extended address (`xa`), not the channel or the PAN ID: those are reported as 0 unless
 * the app knows them ([networks] takes overrides).
 *
 * Needs the INTERNET permission (and, on some devices, a multicast lock for mDNS).
 */
class ThreadBorderRouters(context: Context) {

    private val nsd = context.applicationContext.getSystemService(NsdManager::class.java)
    private val found = ConcurrentHashMap<String, MatterNode.ThreadNetwork>()
    private val resolving = ArrayDeque<NsdServiceInfo>()
    private var busy = false
    private val exec = Executors.newSingleThreadExecutor { r -> Thread(r, "meshcop-resolve").apply { isDaemon = true } }
    @Volatile private var listener: NsdManager.DiscoveryListener? = null

    /** Starts browsing; results accumulate until [stop]. */
    @Synchronized
    fun start() {
        if (listener != null || nsd == null) return
        val l = object : NsdManager.DiscoveryListener {
            override fun onDiscoveryStarted(serviceType: String) {}
            override fun onDiscoveryStopped(serviceType: String) {}
            override fun onStartDiscoveryFailed(serviceType: String, errorCode: Int) {
                Log.w(TAG, "meshcop discovery failed: $errorCode")
            }
            override fun onStopDiscoveryFailed(serviceType: String, errorCode: Int) {}
            override fun onServiceFound(info: NsdServiceInfo) = enqueue(info)
            override fun onServiceLost(info: NsdServiceInfo) {
                found.remove(info.serviceName)
            }
        }
        listener = l
        nsd.discoverServices(SERVICE, NsdManager.PROTOCOL_DNS_SD, l)
    }

    @Synchronized
    fun stop() {
        listener?.let { runCatching { nsd?.stopServiceDiscovery(it) } }
        listener = null
    }

    /**
     * One entry per Thread network (by extended PAN ID) advertised right now. [channel] and
     * [panId] fill in what the TXT records don't carry, when the app knows them.
     */
    fun networks(channel: Int = 0, panId: Int = 0): List<MatterNode.ThreadNetwork> =
        found.values.groupBy { it.extPanId }.values.map { it.first().copy(channel = channel, panId = panId) }

    private fun enqueue(info: NsdServiceInfo) {
        synchronized(resolving) {
            resolving.add(info)
            if (busy) return
            busy = true
        }
        exec.execute(::resolveNext)
    }

    // NsdManager resolves one service at a time before Android 14.
    private fun resolveNext() {
        val next = synchronized(resolving) {
            resolving.poll() ?: run { busy = false; return }
        }
        @Suppress("DEPRECATION")
        nsd?.resolveService(next, object : NsdManager.ResolveListener {
            override fun onResolveFailed(info: NsdServiceInfo, errorCode: Int) {
                exec.execute(::resolveNext)
            }

            override fun onServiceResolved(info: NsdServiceInfo) {
                parse(info)?.let { found[info.serviceName] = it }
                exec.execute(::resolveNext)
            }
        }) ?: synchronized(resolving) { busy = false }
    }

    companion object {
        private const val TAG = "rs-matter-meshcop"
        const val SERVICE = "_meshcop._udp"

        /** A `_meshcop._udp` TXT record as a [MatterNode.ThreadNetwork], or null without nn/xp. */
        fun parse(info: NsdServiceInfo): MatterNode.ThreadNetwork? {
            val a = info.attributes
            return fromTxt(a["nn"], a["xp"], a["xa"])
        }

        fun fromTxt(nn: ByteArray?, xp: ByteArray?, xa: ByteArray?): MatterNode.ThreadNetwork? {
            if (nn == null || xp == null || xp.size != 8) return null
            val name = String(nn, Charsets.UTF_8).take(16)
            if (name.isEmpty()) return null
            val ext = xp.fold(0L) { acc, b -> (acc shl 8) or (b.toLong() and 0xff) }
            return MatterNode.ThreadNetwork(
                networkName = name, extPanId = ext, panId = 0, channel = 0,
                extAddress = if (xa != null && xa.size == 8) xa else ByteArray(8),
                rssi = -60, lqi = 200, version = 4,
            )
        }
    }
}
