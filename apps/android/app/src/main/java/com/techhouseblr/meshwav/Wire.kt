package com.techhouseblr.meshwav

import java.nio.ByteBuffer
import javax.crypto.Cipher
import javax.crypto.spec.IvParameterSpec
import javax.crypto.spec.SecretKeySpec

/** Meshwav wire protocol v1 (see docs/protocol.md and docs/vectors/packet_v1.txt). */
object Wire {
    const val HEADER_LEN = 20
    const val TAG_LEN = 16
    const val TYPE_AUDIO = 1
    const val TYPE_PUNCH = 2
    const val DIR_AGENT_TO_RECEIVER = 0
    const val DIR_RECEIVER_TO_AGENT = 1

    class Packet(val type: Int, val seq: Long, val timestamp: Long, val fragIdx: Int, val fragCnt: Int, val flags: Int, val payload: ByteArray)

    private fun nonce(dir: Int, seq: Long, stream: Long): ByteArray {
        val n = ByteBuffer.allocate(12)
        n.put(dir.toByte()).put(0).put(0).put(0)
        n.putInt(seq.toInt()).putInt(stream.toInt())
        return n.array()
    }

    private fun cipher(mode: Int, key: ByteArray, nonce: ByteArray): Cipher {
        val c = Cipher.getInstance("ChaCha20-Poly1305")
        c.init(mode, SecretKeySpec(key, "ChaCha20"), IvParameterSpec(nonce))
        return c
    }

    fun seal(key: ByteArray, dir: Int, stream: Long, seq: Long, type: Int, timestamp: Long, payload: ByteArray): ByteArray {
        val h = ByteBuffer.allocate(HEADER_LEN)
        h.put('M'.code.toByte()).put('W'.code.toByte()).put(1).put(type.toByte())
        h.putInt(stream.toInt()).putInt(seq.toInt()).putInt(timestamp.toInt())
        h.put(0).put(1).putShort(0)
        val hdr = h.array()
        val c = cipher(Cipher.ENCRYPT_MODE, key, nonce(dir, seq, stream))
        c.updateAAD(hdr)
        return hdr + c.doFinal(payload)
    }

    /** Returns null for anything that is not an authentic packet for this stream. */
    fun open(key: ByteArray, dir: Int, stream: Long, d: ByteArray, len: Int): Packet? {
        if (len < HEADER_LEN + TAG_LEN || len > 1200) return null
        if (d[0] != 'M'.code.toByte() || d[1] != 'W'.code.toByte() || d[2].toInt() != 1) return null
        val b = ByteBuffer.wrap(d, 0, HEADER_LEN)
        b.position(3)
        val type = b.get().toInt() and 0xff
        val sid = b.int.toLong() and 0xffffffffL
        val seq = b.int.toLong() and 0xffffffffL
        val ts = b.int.toLong() and 0xffffffffL
        val fi = b.get().toInt() and 0xff
        val fc = b.get().toInt() and 0xff
        val flags = b.short.toInt() and 0xffff
        if (sid != stream || fc == 0 || fi >= fc) return null
        return try {
            val c = cipher(Cipher.DECRYPT_MODE, key, nonce(dir, seq, stream))
            c.updateAAD(d, 0, HEADER_LEN)
            Packet(type, seq, ts, fi, fc, flags, c.doFinal(d, HEADER_LEN, len - HEADER_LEN))
        } catch (e: Exception) {
            null
        }
    }

    fun hex(s: String): ByteArray {
        require(s.length == 64) { "key must be 64 hex characters" }
        return ByteArray(32) { s.substring(2 * it, 2 * it + 2).toInt(16).toByte() }
    }
}
