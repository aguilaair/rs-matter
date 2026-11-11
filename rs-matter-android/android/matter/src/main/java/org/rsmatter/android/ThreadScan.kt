package org.rsmatter.android

/** Thread scan results as the Rust side reads them (`rs-matter-android/src/scan.rs`). Pure, for tests. */
object ThreadScan {
    /**
     * Each network: pan_id u16, ext_pan_id u64, channel u16, rssi i8, lqi u8, version u8 (big
     * endian), ext_addr 8 bytes, then the name's length and the name (UTF-8, at most 16 bytes).
     */
    fun pack(nets: List<MatterNode.ThreadNetwork>): ByteArray {
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
}
