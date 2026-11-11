package org.rsmatter.android

import android.content.Context
import android.util.Log
import androidx.annotation.Keep
import java.io.File

/**
 * A Matter device running on this Android device (rs-matter).
 *
 * It is commissioned over BLE (the phone scans its QR code), then runs over the IP network the
 * Android device is already on. What the commissioner sends for the device's own network goes to
 * [Listener]: a Thread operational dataset ([Listener.onThreadDataset]) or Wi-Fi credentials
 * ([Listener.onWifiCredentials]). Return true once the app has taken them.
 *
 * The device has one On/Off endpoint for now ([Listener.onOnOff], [setOnOff]).
 *
 * Needs the Bluetooth permissions (BLUETOOTH_CONNECT and BLUETOOTH_ADVERTISE from Android 12 on)
 * granted before [start].
 */
class MatterNode(
    context: Context,
    private val config: Config,
    private val listener: Listener,
) {
    data class Config(
        /** Where the node keeps its fabrics and state. Delete it to factory-reset the node. */
        val storageDir: File,
        /** 12-bit discriminator advertised over BLE and mDNS. */
        val discriminator: Int = 3840,
        /** The setup passcode (1..99999998, not one of the trivial ones the spec forbids). */
        val passcode: Long = 20202021,
        /** Basic Information `NodeLabel`-like name shown by some controllers. */
        val deviceName: String = "Android Matter device",
        val serialNumber: String = "0001",
        /**
         * Also commissionable over the IP network while the window is open. Off by default: like
         * a Thread device not on any network yet, the node is then commissioned over BLE, which is
         * when commissioners send a Thread dataset or Wi-Fi credentials. Commissioners that can
         * see the device on the network commission it there and send none.
         */
        val onNetworkCommissioning: Boolean = false,
    )

    /** A Thread network as seen by a scan (what a commissioner looks for before it sends a dataset). */
    data class ThreadNetwork(
        val networkName: String,
        val extPanId: Long,
        val panId: Int,
        val channel: Int,
        /** The extended address of the router heard (8 bytes). */
        val extAddress: ByteArray = ByteArray(8),
        val rssi: Int = -50,
        val lqi: Int = 255,
        val version: Int = 4,
    )

    interface Listener {
        /** The commissioner's Thread operational dataset (TLVs). Return true when taken. */
        fun onThreadDataset(dataset: ByteArray): Boolean = false
        /**
         * The Thread networks around, for a commissioner's scan during BLE commissioning. Some
         * commissioners (Apple's) only send a dataset once its network shows up here.
         */
        fun onThreadScan(): List<ThreadNetwork> = emptyList()
        /** Wi-Fi credentials from the commissioner. Return true when taken. */
        fun onWifiCredentials(ssid: ByteArray, password: ByteArray): Boolean = false
        /** A controller switched the On/Off endpoint. */
        fun onOnOff(on: Boolean) {}
        /** What the node is doing: `pairing_code`, `qr_code`, `state`, `stopped`. */
        fun onEvent(kind: String, detail: String) {}
    }

    private val ble = MatterBlePeripheral(context.applicationContext, object : MatterBlePeripheral.Events {
        override fun onC1Write(addr: ByteArray, mtu: Int, value: ByteArray) {
            val h = handle
            if (h != 0L) nativeC1Write(h, addr, mtu, value)
        }

        override fun onC2Subscribed(addr: ByteArray, subscribed: Boolean) {
            val h = handle
            if (h != 0L) nativeC2Subscribed(h, addr, subscribed)
        }

        override fun onIndicated(ok: Boolean) {
            val h = handle
            if (h != 0L) nativeIndicated(h, ok)
        }
    })

    @Volatile private var handle = 0L

    /** The manual pairing code (`1234-567-8901`) once the node has started. */
    @Volatile var pairingCode: String? = null
        private set

    /** The QR code text (`MT:…`) once the node has started. */
    @Volatile var qrCode: String? = null
        private set

    val isRunning: Boolean get() = handle != 0L

    /** Starts the node on its own thread. Returns false if it couldn't. */
    @Synchronized
    fun start(): Boolean {
        if (handle != 0L) return true
        config.storageDir.mkdirs()
        handle = nativeStart(
            config.storageDir.absolutePath, config.discriminator, config.passcode,
            config.deviceName, config.serialNumber, config.onNetworkCommissioning,
        )
        return handle != 0L
    }

    /** Stops the node (and BLE). */
    @Synchronized
    fun stop() {
        val h = handle
        if (h == 0L) return
        handle = 0L
        nativeStop(h)
        ble.stop()
    }

    /** The app switched the On/Off endpoint itself (a physical switch, its own UI). */
    fun setOnOff(on: Boolean) {
        val h = handle
        if (h != 0L) nativeSetOnOff(h, on)
    }

    // ---- called by the Rust side (JNI), on the node's thread

    @Keep
    fun bleStart(serviceData: ByteArray): String? = ble.start(serviceData)

    @Keep
    fun bleStop() = ble.stop()

    @Keep
    fun bleIndicate(addr: ByteArray, value: ByteArray): Boolean = ble.indicate(addr, value)

    @Keep
    fun bleDisconnect(addr: ByteArray) = ble.disconnect(addr)

    @Keep
    fun onThreadDataset(dataset: ByteArray): Boolean = guarded(false) { listener.onThreadDataset(dataset) }

    @Keep
    fun onWifiCredentials(ssid: ByteArray, password: ByteArray): Boolean =
        guarded(false) { listener.onWifiCredentials(ssid, password) }

    @Keep
    fun onThreadScan(): ByteArray = packThreadScan(guarded(emptyList()) { listener.onThreadScan() })

    @Keep
    fun onOnOff(on: Boolean) = guarded(Unit) { listener.onOnOff(on) }

    @Keep
    fun onEvent(kind: String, detail: String) {
        when (kind) {
            "pairing_code" -> pairingCode = detail
            "qr_code" -> qrCode = detail
        }
        Log.i(TAG, "$kind: $detail")
        guarded(Unit) { listener.onEvent(kind, detail) }
    }

    /** A listener's exception must not unwind into Rust. */
    private inline fun <T> guarded(fallback: T, block: () -> T): T = try {
        block()
    } catch (t: Throwable) {
        Log.w(TAG, "listener failed", t)
        fallback
    }

    private external fun nativeStart(
        storageDir: String, discriminator: Int, passcode: Long, deviceName: String, serial: String,
        onNetworkCommissioning: Boolean,
    ): Long

    private external fun nativeStop(handle: Long)
    private external fun nativeC1Write(handle: Long, addr: ByteArray, mtu: Int, value: ByteArray)
    private external fun nativeC2Subscribed(handle: Long, addr: ByteArray, subscribed: Boolean)
    private external fun nativeIndicated(handle: Long, ok: Boolean)
    private external fun nativeSetOnOff(handle: Long, on: Boolean)

    companion object {
        private const val TAG = "rs-matter"

        /** The layout `rs-matter-android/src/scan.rs` reads (big endian). */
        internal fun packThreadScan(nets: List<ThreadNetwork>): ByteArray {
            val out = java.io.ByteArrayOutputStream()
            val b = java.nio.ByteBuffer.allocate(23)
            for (n in nets) {
                val name = n.networkName.toByteArray(Charsets.UTF_8).take(16).toByteArray()
                b.clear()
                b.putShort(n.panId.toShort()).putLong(n.extPanId).putShort(n.channel.toShort())
                b.put(n.rssi.toByte()).put(n.lqi.toByte()).put(n.version.toByte())
                b.put(n.extAddress.copyOf(8))
                out.write(b.array(), 0, 23)
                out.write(name.size)
                out.write(name)
            }
            return out.toByteArray()
        }

        init {
            System.loadLibrary("rs_matter_android")
        }
    }
}
