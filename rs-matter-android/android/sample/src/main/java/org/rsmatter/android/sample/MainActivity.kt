package org.rsmatter.android.sample

import android.Manifest
import android.app.Activity
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import android.util.Log
import android.widget.TextView
import org.rsmatter.android.MatterNode
import java.io.File

/**
 * A Matter On/Off light on this Android device, commissionable over BLE. Shows the pairing code
 * and what the commissioner sent. A Thread dataset is only described (length), never shown: it
 * holds the network key.
 */
class MainActivity : Activity() {
    private lateinit var text: TextView
    private var node: MatterNode? = null
    private val lines = ArrayList<String>()

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        text = TextView(this).apply { textSize = 20f; setPadding(40, 40, 40, 40) }
        setContentView(text)
        val needed = if (Build.VERSION.SDK_INT >= 31) {
            arrayOf(Manifest.permission.BLUETOOTH_CONNECT, Manifest.permission.BLUETOOTH_ADVERTISE)
        } else {
            emptyArray()
        }
        val missing = needed.filter { checkSelfPermission(it) != PackageManager.PERMISSION_GRANTED }
        if (missing.isEmpty()) startNode() else requestPermissions(missing.toTypedArray(), 1)
    }

    override fun onRequestPermissionsResult(requestCode: Int, permissions: Array<out String>, grantResults: IntArray) {
        if (grantResults.all { it == PackageManager.PERMISSION_GRANTED }) startNode() else show("Bluetooth permission denied")
    }

    private fun startNode() {
        val n = MatterNode(
            this,
            MatterNode.Config(storageDir = File(filesDir, "matter"), deviceName = "rs-matter sample"),
            object : MatterNode.Listener {
                override fun onThreadDataset(dataset: ByteArray): Boolean {
                    show("Thread dataset received: ${dataset.size} bytes")
                    return true
                }

                // For the test: the Thread network to report, from intent extras
                // (adb shell am start ... --es thread_name NAME --es thread_extpanid HEX
                //  --ei thread_panid N --ei thread_channel N --es thread_extaddr HEX).
                override fun onThreadScan(): List<MatterNode.ThreadNetwork> {
                    val name = intent.getStringExtra("thread_name") ?: return emptyList()
                    val net = MatterNode.ThreadNetwork(
                        networkName = name,
                        extPanId = java.lang.Long.parseUnsignedLong(intent.getStringExtra("thread_extpanid") ?: "0", 16),
                        panId = intent.getIntExtra("thread_panid", 0),
                        channel = intent.getIntExtra("thread_channel", 11),
                        extAddress = (intent.getStringExtra("thread_extaddr") ?: "0000000000000000")
                            .chunked(2).map { it.toInt(16).toByte() }.toByteArray(),
                    )
                    show("Thread scan: reporting ${net.networkName} (channel ${net.channel})")
                    return listOf(net)
                }

                override fun onWifiCredentials(ssid: ByteArray, password: ByteArray): Boolean {
                    show("Wi-Fi credentials received for ${String(ssid)}")
                    return true
                }

                override fun onOnOff(on: Boolean) = show("Light ${if (on) "on" else "off"}")

                override fun onEvent(kind: String, detail: String) = show("$kind: $detail")
            },
        )
        node = n
        show(if (n.start()) "Node started" else "Node failed to start")
    }

    private fun show(line: String) {
        Log.i("rs-matter-sample", line)
        runOnUiThread {
            lines += line
            text.text = lines.takeLast(14).joinToString("\n")
        }
    }

    override fun onDestroy() {
        node?.stop()
        super.onDestroy()
    }
}
