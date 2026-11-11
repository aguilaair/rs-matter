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
 * What the device is, is [Config.endpoints]: On/Off lights and sensors ([Endpoint]). Controllers'
 * On/Off commands come to [Listener.onOnOff]; the app reports its own state and sensor readings with
 * [setOnOff], [setOccupied], [setIlluminance], [setTemperature] and [setHumidity].
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
        /** The device's endpoints (ids 1 and up, unique). The root endpoint 0 is the node's own. */
        val endpoints: List<Endpoint> = listOf(Endpoint.OnOff(1)),
    )

    /** An endpoint of the device: a Matter device type with its cluster. */
    sealed class Endpoint(val id: Int, internal val kind: Int) {
        /** An On/Off light (`OnOff` cluster). */
        class OnOff(id: Int) : Endpoint(id, 1)
        /** An occupancy sensor (`OccupancySensing`); report with [setOccupied]. */
        class OccupancySensor(id: Int) : Endpoint(id, 2)
        /** A light sensor (`IlluminanceMeasurement`); report with [setIlluminance]. */
        class LightSensor(id: Int) : Endpoint(id, 3)
        /** A temperature sensor (`TemperatureMeasurement`); report with [setTemperature]. */
        class TemperatureSensor(id: Int) : Endpoint(id, 4)
        /** A humidity sensor (`RelativeHumidityMeasurement`); report with [setHumidity]. */
        class HumiditySensor(id: Int) : Endpoint(id, 5)

        override fun toString() = "${javaClass.simpleName}($id)"
    }

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
        /** A controller switched the On/Off endpoint [endpoint]. */
        fun onOnOff(endpoint: Int, on: Boolean) {}
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
        val ids = config.endpoints.map { it.id }
        require(ids.all { it in 1..0xfffe } && ids.toSet().size == ids.size) { "endpoint ids must be unique, 1 and up" }
        config.storageDir.mkdirs()
        handle = nativeStart(
            config.storageDir.absolutePath, config.discriminator, config.passcode,
            config.deviceName, config.serialNumber, config.onNetworkCommissioning,
            config.endpoints.flatMap { listOf(it.id, it.kind) }.toIntArray(),
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

    /** The app switched On/Off endpoint [endpoint] itself (a physical switch, its own UI). */
    fun setOnOff(endpoint: Int, on: Boolean) = setValue(endpoint, if (on) 1 else 0)

    /** Occupancy sensor [endpoint]: someone is there (or not). */
    fun setOccupied(endpoint: Int, occupied: Boolean) = setValue(endpoint, if (occupied) 1 else 0)

    /** Light sensor [endpoint]: illuminance in lux. */
    fun setIlluminance(endpoint: Int, lux: Double) = setValue(endpoint, Units.illuminance(lux))

    /** Temperature sensor [endpoint]: °C. */
    fun setTemperature(endpoint: Int, celsius: Double) = setValue(endpoint, Units.hundredths(celsius))

    /** Humidity sensor [endpoint]: % relative humidity. */
    fun setHumidity(endpoint: Int, percent: Double) = setValue(endpoint, Units.hundredths(percent))

    private fun setValue(endpoint: Int, value: Int) {
        val h = handle
        if (h != 0L) nativeSetValue(h, endpoint, value)
    }

    /**
     * Opens the commissioning window (15 minutes, over BLE): to pair a node that was removed from its
     * controller, or to add it to another one. [Listener.onEvent] reports `state: commissionable`.
     */
    fun openCommissioningWindow() {
        val h = handle
        if (h != 0L) nativeOpenCommissioningWindow(h)
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
    fun onThreadScan(): ByteArray = ThreadScan.pack(guarded(emptyList()) { listener.onThreadScan() })

    @Keep
    fun onOnOff(endpoint: Int, on: Boolean) = guarded(Unit) { listener.onOnOff(endpoint, on) }

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
        onNetworkCommissioning: Boolean, endpoints: IntArray,
    ): Long

    private external fun nativeStop(handle: Long)
    private external fun nativeC1Write(handle: Long, addr: ByteArray, mtu: Int, value: ByteArray)
    private external fun nativeC2Subscribed(handle: Long, addr: ByteArray, subscribed: Boolean)
    private external fun nativeIndicated(handle: Long, ok: Boolean)
    private external fun nativeSetValue(handle: Long, endpoint: Int, value: Int)
    private external fun nativeOpenCommissioningWindow(handle: Long)

    companion object {
        private const val TAG = "rs-matter"

        init {
            System.loadLibrary("rs_matter_android")
        }
    }
}
