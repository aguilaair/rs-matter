package org.rsmatter.android

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class UnitsTest {
    @Test
    fun illuminance() {
        assertEquals(0, Units.illuminance(0.0))
        assertEquals(0, Units.illuminance(-3.0))
        assertEquals(1, Units.illuminance(1.0))          // 10000 * log10(1) + 1
        assertEquals(20001, Units.illuminance(100.0))
        assertEquals(40001, Units.illuminance(10_000.0))
        assertEquals(0xFFFE, Units.illuminance(1e9))      // capped
    }

    @Test
    fun hundredths() {
        assertEquals(2150, Units.hundredths(21.5))
        assertEquals(-1025, Units.hundredths(-10.25))
        assertEquals(Int.MIN_VALUE, Units.hundredths(Double.NaN)) // unknown: reads as null
    }

    @Test
    fun threadScanPacking() {
        // The layout rs-matter-android/src/scan.rs unpacks (its unit test packs the same bytes).
        val b = ThreadScan.pack(listOf(MatterNode.ThreadNetwork(
            networkName = "ha-thread-4108", extPanId = 0x108e20b33659615bL, panId = 0x4108, channel = 26,
            extAddress = byteArrayOf(0xba.toByte(), 0x75, 0x4f, 0x15, 0x5a, 0x49, 0x8d.toByte(), 0x30), rssi = -60, lqi = 200,
        )))
        val expected = byteArrayOf(0x41, 0x08) +
            byteArrayOf(0x10, 0x8e.toByte(), 0x20, 0xb3.toByte(), 0x36, 0x59, 0x61, 0x5b) +
            byteArrayOf(0, 26, (-60).toByte(), 200.toByte(), 4) +
            byteArrayOf(0xba.toByte(), 0x75, 0x4f, 0x15, 0x5a, 0x49, 0x8d.toByte(), 0x30) +
            byteArrayOf(14) + "ha-thread-4108".toByteArray()
        assertArrayEquals(expected, b)
    }

    @Test
    fun meshcopTxt() {
        val n = ThreadBorderRouters.fromTxt(
            "ha-thread-4108".toByteArray(), byteArrayOf(0x10, 0x8e.toByte(), 0x20, 0xb3.toByte(), 0x36, 0x59, 0x61, 0x5b), null,
        )!!
        assertEquals("ha-thread-4108", n.networkName)
        assertEquals(0x108e20b33659615bL, n.extPanId)
        assertEquals(0, n.channel)
        assertNull(ThreadBorderRouters.fromTxt(null, ByteArray(8), null))
        assertNull(ThreadBorderRouters.fromTxt("x".toByteArray(), ByteArray(7), null))
    }
}
