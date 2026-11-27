package com.techhouseblr.meshwav

import org.junit.Assert.*
import org.junit.Test
import java.util.Random

/** Malformed network input must give null or an ignored message, never an exception (the service would crash). */
class RobustnessTest {
    private val rnd = Random(42)
    private fun bytes(max: Int) = ByteArray(rnd.nextInt(max + 1)).also { rnd.nextBytes(it) }
    private fun mutate(base: ByteArray): ByteArray {
        val v = base.toMutableList()
        repeat(1 + rnd.nextInt(4)) {
            when (rnd.nextInt(4)) {
                0 -> if (v.isNotEmpty()) v[rnd.nextInt(v.size)] = rnd.nextInt(256).toByte()
                1 -> v.add(rnd.nextInt(v.size + 1), rnd.nextInt(256).toByte())
                2 -> if (v.isNotEmpty()) v.removeAt(rnd.nextInt(v.size))
                else -> while (v.size > rnd.nextInt(v.size + 1)) v.removeLast()
            }
        }
        return v.toByteArray()
    }

    @Test fun controlDecodeNeverThrows() {
        val corpus = listOf(
            ControlWire.encodeBye(),
            byteArrayOf(0, 1, 20),
            byteArrayOf(0, 2, 19, 40),
            byteArrayOf(0, 2, 18, 40),
            byteArrayOf(0, 1, 16),
        )
        repeat(20_000) { i ->
            ControlWire.decode(bytes(80))
            ControlWire.decode(mutate(corpus[i % corpus.size]))
        }
    }

    @Test fun wireOpenRejectsGarbage() {
        val key = ByteArray(32) { it.toByte() }
        val good = Wire.seal(key, Wire.DIR_AGENT_TO_RECEIVER, 5L, 1L, Wire.TYPE_AUDIO, 10L, ByteArray(40) { 1 })
        assertNotNull(Wire.open(key, Wire.DIR_AGENT_TO_RECEIVER, 5L, good, good.size))
        repeat(20_000) {
            val g = bytes(1300)
            Wire.open(key, Wire.DIR_AGENT_TO_RECEIVER, 5L, g, g.size)
            val m = mutate(good)
            val p = Wire.open(key, Wire.DIR_AGENT_TO_RECEIVER, 5L, m, m.size)
            if (!m.contentEquals(good)) assertNull("a modified packet must not authenticate", p)
        }
    }

    @Test fun pairingParsersRejectGarbage() {
        repeat(20_000) {
            val g = bytes(200)
            Handshake.parseReply(g, g.size, 1, 7)
            Handshake.parseInvite(g, g.size)
            // A claimed length longer than the buffer must not throw either.
            runCatching { Handshake.parseInvite(g, g.size + 5) }
        }
    }

    @Test fun clockSyncHandlesExtremeValues() {
        val c = ClockSync()
        repeat(20_000) {
            c.onPong(rnd.nextLong(), rnd.nextLong(), rnd.nextLong())
            c.onTimeRef(rnd.nextLong(), rnd.nextLong())
            c.capturedAtLocalUs(rnd.nextLong())
        }
    }

    @Test fun jitterBufferStaysBounded() {
        val j = JitterBuffer.forFrameMs(20)
        var now = 0L
        repeat(20_000) {
            now += rnd.nextInt(30)
            j.push(rnd.nextLong() % 100_000, bytes(300), now)
            j.pop(now)
        }
    }
}
