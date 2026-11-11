package org.rsmatter.android

import android.annotation.SuppressLint
import android.bluetooth.BluetoothDevice
import android.bluetooth.BluetoothGatt
import android.bluetooth.BluetoothGattCharacteristic
import android.bluetooth.BluetoothGattDescriptor
import android.bluetooth.BluetoothGattServer
import android.bluetooth.BluetoothGattServerCallback
import android.bluetooth.BluetoothGattService
import android.bluetooth.BluetoothManager
import android.bluetooth.BluetoothProfile
import android.bluetooth.le.AdvertiseCallback
import android.bluetooth.le.AdvertiseData
import android.bluetooth.le.AdvertiseSettings
import android.content.Context
import android.os.Build
import android.os.ParcelUuid
import android.util.Log
import java.util.UUID
import java.util.concurrent.ConcurrentHashMap

/**
 * The Matter BLE commissioning service (Matter Core spec 4.18, "BTP") on Android's
 * [BluetoothGattServer]: service `0xFFF6` with C1 (the commissioner writes) and C2 (the device
 * indicates), advertised with the Matter service data. It only moves bytes: the BTP session itself
 * runs in Rust, which gets [Events] and calls [indicate].
 */
@SuppressLint("MissingPermission") // the app holds BLUETOOTH_CONNECT/ADVERTISE (or the pre-31 ones)
internal class MatterBlePeripheral(private val context: Context, private val events: Events) {

    interface Events {
        /** A peer wrote [value] to C1 (already acknowledged). [mtu] is its ATT MTU. */
        fun onC1Write(addr: ByteArray, mtu: Int, value: ByteArray)
        /** A peer (un)subscribed to C2 indications; a disconnect counts as unsubscribing. */
        fun onC2Subscribed(addr: ByteArray, subscribed: Boolean)
        /** The last [indicate] was confirmed by the peer (or failed). */
        fun onIndicated(ok: Boolean)
    }

    private val manager = context.getSystemService(BluetoothManager::class.java)
    private var server: BluetoothGattServer? = null
    private var c2: BluetoothGattCharacteristic? = null
    private val mtus = ConcurrentHashMap<String, Int>()
    private val devices = ConcurrentHashMap<String, BluetoothDevice>()
    private val subscribed = ConcurrentHashMap.newKeySet<String>()
    @Volatile private var advertising = false

    /** Publishes the service and starts advertising [serviceData]. Returns null, or why not. */
    @Synchronized
    fun start(serviceData: ByteArray): String? {
        stop()
        val adapter = manager?.adapter ?: return "no Bluetooth adapter"
        if (!adapter.isEnabled) return "Bluetooth is off"
        val advertiser = adapter.bluetoothLeAdvertiser ?: return "this device can't advertise over BLE"
        val s = try {
            manager.openGattServer(context, callback)
        } catch (e: SecurityException) {
            return "missing Bluetooth permission: ${e.message}"
        } ?: return "openGattServer returned null"
        val service = BluetoothGattService(SERVICE, BluetoothGattService.SERVICE_TYPE_PRIMARY)
        val c1 = BluetoothGattCharacteristic(
            C1, BluetoothGattCharacteristic.PROPERTY_WRITE, BluetoothGattCharacteristic.PERMISSION_WRITE,
        )
        val c2c = BluetoothGattCharacteristic(
            C2, BluetoothGattCharacteristic.PROPERTY_INDICATE or BluetoothGattCharacteristic.PROPERTY_READ,
            BluetoothGattCharacteristic.PERMISSION_READ,
        ).apply {
            addDescriptor(
                BluetoothGattDescriptor(
                    CCCD, BluetoothGattDescriptor.PERMISSION_READ or BluetoothGattDescriptor.PERMISSION_WRITE,
                ),
            )
        }
        service.addCharacteristic(c1)
        service.addCharacteristic(c2c)
        if (!s.addService(service)) {
            s.close()
            return "addService failed"
        }
        server = s
        c2 = c2c

        val settings = AdvertiseSettings.Builder()
            .setAdvertiseMode(AdvertiseSettings.ADVERTISE_MODE_LOW_LATENCY)
            .setTxPowerLevel(AdvertiseSettings.ADVERTISE_TX_POWER_MEDIUM)
            .setConnectable(true)
            .setTimeout(0)
            .build()
        val data = AdvertiseData.Builder()
            .setIncludeDeviceName(false)
            .setIncludeTxPowerLevel(false)
            .addServiceData(ParcelUuid(SERVICE), serviceData)
            .build()
        return try {
            advertiser.startAdvertising(settings, data, advertiseCallback)
            advertising = true
            Log.i(TAG, "Advertising the Matter BLE service")
            null
        } catch (e: Exception) {
            stop()
            "startAdvertising failed: $e"
        }
    }

    /** Stops advertising, drops the connections and closes the GATT server. */
    @Synchronized
    fun stop() {
        if (advertising) {
            runCatching { manager?.adapter?.bluetoothLeAdvertiser?.stopAdvertising(advertiseCallback) }
            advertising = false
        }
        server?.let { s ->
            devices.values.forEach { runCatching { s.cancelConnection(it) } }
            runCatching { s.close() }
        }
        server = null
        c2 = null
        devices.clear()
        mtus.clear()
        subscribed.clear()
    }

    /** Indicates [value] on C2 to [addr]; the outcome comes through [Events.onIndicated]. */
    fun indicate(addr: ByteArray, value: ByteArray): Boolean {
        val s = server ?: return false
        val c = c2 ?: return false
        val device = devices[addrString(addr)] ?: return false
        return try {
            if (Build.VERSION.SDK_INT >= 33) {
                s.notifyCharacteristicChanged(device, c, true, value) == BluetoothGatt.GATT_SUCCESS
            } else {
                @Suppress("DEPRECATION")
                c.value = value
                @Suppress("DEPRECATION")
                s.notifyCharacteristicChanged(device, c, true)
            }
        } catch (e: Exception) {
            Log.w(TAG, "indicate failed", e)
            false
        }
    }

    fun disconnect(addr: ByteArray) {
        val device = devices[addrString(addr)] ?: return
        runCatching { server?.cancelConnection(device) }
    }

    private val callback = object : BluetoothGattServerCallback() {
        override fun onConnectionStateChange(device: BluetoothDevice, status: Int, newState: Int) {
            val a = device.address
            if (newState == BluetoothProfile.STATE_CONNECTED) {
                devices[a] = device
                mtus.putIfAbsent(a, DEFAULT_ATT_MTU)
                Log.i(TAG, "Peer connected")
            } else if (newState == BluetoothProfile.STATE_DISCONNECTED) {
                devices.remove(a)
                mtus.remove(a)
                if (subscribed.remove(a)) events.onC2Subscribed(addrBytes(a), false)
                Log.i(TAG, "Peer disconnected")
            }
        }

        override fun onMtuChanged(device: BluetoothDevice, mtu: Int) {
            mtus[device.address] = mtu
        }

        override fun onCharacteristicWriteRequest(
            device: BluetoothDevice, requestId: Int, characteristic: BluetoothGattCharacteristic,
            preparedWrite: Boolean, responseNeeded: Boolean, offset: Int, value: ByteArray?,
        ) {
            val ok = characteristic.uuid == C1 && !preparedWrite && offset == 0 && value != null
            if (responseNeeded) {
                server?.sendResponse(
                    device, requestId,
                    if (ok) BluetoothGatt.GATT_SUCCESS else BluetoothGatt.GATT_WRITE_NOT_PERMITTED, 0, null,
                )
            }
            if (ok) events.onC1Write(addrBytes(device.address), mtus[device.address] ?: DEFAULT_ATT_MTU, value!!)
        }

        override fun onDescriptorWriteRequest(
            device: BluetoothDevice, requestId: Int, descriptor: BluetoothGattDescriptor,
            preparedWrite: Boolean, responseNeeded: Boolean, offset: Int, value: ByteArray?,
        ) {
            val isC2Cccd = descriptor.uuid == CCCD && descriptor.characteristic.uuid == C2
            if (responseNeeded) {
                server?.sendResponse(
                    device, requestId,
                    if (isC2Cccd) BluetoothGatt.GATT_SUCCESS else BluetoothGatt.GATT_WRITE_NOT_PERMITTED, 0, null,
                )
            }
            if (!isC2Cccd || value == null) return
            val on = value.isNotEmpty() && (value[0].toInt() and 0x02) != 0 // indications
            val a = device.address
            if (on) subscribed.add(a) else subscribed.remove(a)
            events.onC2Subscribed(addrBytes(a), on)
        }

        override fun onDescriptorReadRequest(
            device: BluetoothDevice, requestId: Int, offset: Int, descriptor: BluetoothGattDescriptor,
        ) {
            val v = if (descriptor.uuid == CCCD && device.address in subscribed) {
                BluetoothGattDescriptor.ENABLE_INDICATION_VALUE
            } else {
                BluetoothGattDescriptor.DISABLE_NOTIFICATION_VALUE
            }
            server?.sendResponse(device, requestId, BluetoothGatt.GATT_SUCCESS, 0, v)
        }

        override fun onCharacteristicReadRequest(
            device: BluetoothDevice, requestId: Int, offset: Int, characteristic: BluetoothGattCharacteristic,
        ) {
            server?.sendResponse(device, requestId, BluetoothGatt.GATT_SUCCESS, 0, ByteArray(0))
        }

        override fun onNotificationSent(device: BluetoothDevice, status: Int) {
            events.onIndicated(status == BluetoothGatt.GATT_SUCCESS)
        }
    }

    private val advertiseCallback = object : AdvertiseCallback() {
        override fun onStartFailure(errorCode: Int) {
            Log.w(TAG, "Advertising the Matter BLE service failed: error $errorCode")
            advertising = false
        }
    }

    companion object {
        private const val TAG = "rs-matter-ble"
        private const val DEFAULT_ATT_MTU = 23

        val SERVICE: UUID = UUID.fromString("0000FFF6-0000-1000-8000-00805F9B34FB")
        val C1: UUID = UUID.fromString("18EE2EF5-263D-4559-959F-4F9C429F9D11")
        val C2: UUID = UUID.fromString("18EE2EF5-263D-4559-959F-4F9C429F9D12")
        val CCCD: UUID = UUID.fromString("00002902-0000-1000-8000-00805F9B34FB")

        /** `AA:BB:CC:DD:EE:FF` → 6 bytes. */
        fun addrBytes(a: String): ByteArray = a.split(':').map { it.toInt(16).toByte() }.toByteArray()

        fun addrString(b: ByteArray): String = b.joinToString(":") { "%02X".format(it.toInt() and 0xff) }
    }
}
