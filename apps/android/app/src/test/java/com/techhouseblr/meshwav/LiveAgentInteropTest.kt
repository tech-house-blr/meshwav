package com.techhouseblr.meshwav

import java.net.DatagramPacket
import java.net.DatagramSocket
import java.net.InetSocketAddress
import java.security.SecureRandom
import org.junit.Assert.*
import org.junit.Assume.assumeTrue
import org.junit.Test

/**
 * Interop against a REAL meshwav-agent. Skipped unless MESHWAV_E2E_PORT / _PK / _CODE are set
 * (start the agent with --pair on loopback and a fake parec).
 */
class LiveAgentInteropTest {
    private fun hex(s: String) = ByteArray(s.length / 2) { s.substring(2 * it, 2 * it + 2).toInt(16).toByte() }

    private fun sock(port: Int) = DatagramSocket().also { it.connect(InetSocketAddress("127.0.0.1", port)); it.soTimeout = 300 }

    /** Punch every 500 ms for [ms]; returns (audio seqs, keepalive count). */
    private fun listen(s: DatagramSocket, k: SessionKeys, ms: Long): Pair<List<Long>, Int> {
        val seqs = ArrayList<Long>(); var ka = 0
        val buf = ByteArray(2048); val pkt = DatagramPacket(buf, buf.size)
        var punchSeq = 0L; var nextPunch = 0L
        val end = System.currentTimeMillis() + ms
        while (System.currentTimeMillis() < end) {
            if (System.currentTimeMillis() >= nextPunch) {
                val d = Wire.seal(k.toAgent, Wire.DIR_RECEIVER_TO_AGENT, 1, punchSeq++, Wire.TYPE_PUNCH, 0, ByteArray(0))
                s.send(DatagramPacket(d, d.size)); nextPunch = System.currentTimeMillis() + 500
            }
            try { s.receive(pkt) } catch (_: java.net.SocketTimeoutException) { continue }
            val a = Wire.open(k.toReceiver, Wire.DIR_AGENT_TO_RECEIVER, 1, buf, pkt.length)
            if (a != null) { if (a.type == Wire.TYPE_AUDIO) seqs += a.seq; continue }
            val c = Wire.open(k.toReceiver, Wire.DIR_AGENT_TO_RECEIVER, 2, buf, pkt.length)
            if (c != null && c.type == Wire.TYPE_PUNCH) ka++
        }
        return seqs to ka
    }

    @Test fun pairReconnectAndRejections() {
        val port = System.getenv("MESHWAV_E2E_PORT")?.toIntOrNull()
        assumeTrue("MESHWAV_E2E_PORT not set", port != null)
        val pk = hex(System.getenv("MESHWAV_E2E_PK")!!)
        val code = System.getenv("MESHWAV_E2E_CODE")!!
        val priv = ByteArray(32).also { SecureRandom().nextBytes(it) }

        // 1. pairing with the one-time code
        val s1 = sock(port!!)
        val k1 = Handshake.run(s1, priv, pk, code, "kotlin-test") { true }
        assertArrayEquals(pk, k1.agentPublic)
        val (seqs, ka) = listen(s1, k1, 3000)
        assertTrue("audio packets: ${seqs.size}", seqs.size > 50)
        assertTrue("keepalives: $ka", ka >= 2)
        assertTrue("audio seqs gap-free", seqs.zipWithNext().all { (a, b) -> b == a + 1 })

        // 2. reconnect with the pinned key only: fresh keys, audio again
        val s2 = sock(port)
        val k2 = Handshake.run(s2, priv, pk, null, "kotlin-test") { true }
        assertFalse(k1.toReceiver.contentEquals(k2.toReceiver))
        assertTrue(listen(s2, k2, 2000).first.size > 30)

        // 2b. runtime config against the real agent: 20 ms frames at 96 kbit/s, then a refused value
        run {
            val buf = ByteArray(2048); val pkt = DatagramPacket(buf, buf.size)
            var punchSeq = 0L; var ctlSeq = 0L
            fun sendPunch() { val d = Wire.seal(k2.toAgent, Wire.DIR_RECEIVER_TO_AGENT, 1, punchSeq++, Wire.TYPE_PUNCH, 0, ByteArray(0)); s2.send(DatagramPacket(d, d.size)) }
            fun sendCtl(b: ByteArray) { val d = Wire.seal(k2.toAgent, Wire.DIR_RECEIVER_TO_AGENT, 2, ctlSeq++, Wire.TYPE_CONTROL, 0, b); s2.send(DatagramPacket(d, d.size)) }
            fun await(ms: Long, want: (ControlWire.Msg) -> Boolean): ControlWire.Msg? {
                val end = System.currentTimeMillis() + ms
                while (System.currentTimeMillis() < end) {
                    try { s2.receive(pkt) } catch (_: java.net.SocketTimeoutException) { continue }
                    val c = Wire.open(k2.toReceiver, Wire.DIR_AGENT_TO_RECEIVER, 2, buf, pkt.length) ?: continue
                    if (c.type == Wire.TYPE_CONTROL) ControlWire.decode(c.payload)?.let { if (want(it)) return it }
                }
                return null
            }
            sendPunch()
            sendCtl(ControlWire.encodeSetConfig(1, 96, 20))
            assertEquals(ControlWire.Msg.Applied(1, 96, 20), await(3000) { it is ControlWire.Msg.Applied })
            val frames = HashSet<Int>()
            val end = System.currentTimeMillis() + 800
            while (System.currentTimeMillis() < end) {
                try { s2.receive(pkt) } catch (_: java.net.SocketTimeoutException) { continue }
                Wire.open(k2.toReceiver, Wire.DIR_AGENT_TO_RECEIVER, 1, buf, pkt.length)?.let { if (it.type == Wire.TYPE_AUDIO) frames += ControlWire.frameMsFromFlags(it.flags) }
            }
            assertTrue("frames after switch: $frames", frames.contains(20) && frames.size <= 2)
            sendPunch()
            sendCtl(ControlWire.encodeSetConfig(2, 999, 0))
            assertEquals(ControlWire.Msg.Rejected(2, ControlWire.REJECT_BITRATE), await(3000) { it is ControlWire.Msg.Rejected })
        }

        // 3. wrong pinned agent key aborts fatally before sending our identity
        val e = assertThrows(HandshakeException::class.java) { Handshake.run(sock(port), priv, ByteArray(32) { 5 }, null, "x") { true } }
        assertTrue(e.fatal)

        // 4. a device that was never paired completes the exchange but gets nothing
        val other = ByteArray(32).also { SecureRandom().nextBytes(it) }
        val s4 = sock(port)
        val k4 = Handshake.run(s4, other, pk, null, "stranger") { true }
        assertEquals(0, listen(s4, k4, 2000).first.size)

        // 5. the code is single use
        val e5 = assertThrows(HandshakeException::class.java) { Handshake.run(sock(port), other, pk, code, "again") { true } }
        assertFalse(e5.fatal)
    }
}
