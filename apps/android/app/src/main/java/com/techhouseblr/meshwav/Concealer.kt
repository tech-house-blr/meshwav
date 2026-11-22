package com.techhouseblr.meshwav

import java.nio.ByteBuffer
import java.nio.ByteOrder

/**
 * Packet loss concealment for PCM16 interleaved stereo. The platform Opus decoder has no PLC
 * entry point, so a lost frame repeats the last good one with a fade that reaches silence after
 * MAX_MISSES consecutive losses; the first real frame after a loss fades in over 2 ms to avoid a click.
 */
class Concealer {
    companion object {
        const val MAX_MISSES = 5
        const val FADE_IN_SAMPLES = 192 // 2 ms stereo at 48 kHz
        private const val DECAY = 0.6f
    }

    private var last = ShortArray(0)
    private var misses = 0

    /** Record a decoded frame; returns it (with a short fade-in if it follows a loss). */
    fun accept(pcm: ByteArray): ByteArray {
        if (pcm.isEmpty()) return pcm
        val s = toShorts(pcm)
        last = s.copyOf()
        if (misses > 0) {
            val n = minOf(FADE_IN_SAMPLES, s.size)
            for (i in 0 until n) s[i] = (s[i] * (i / n.toFloat())).toInt().toShort()
        }
        misses = 0
        return toBytes(s)
    }

    /** Forget the last frame (the frame size changed, so it no longer fits). */
    fun reset() { last = ShortArray(0); misses = 0 }

    /** Synthesize a frame for a lost packet. */
    fun conceal(fallbackBytes: Int): ByteArray {
        misses++
        if (last.isEmpty()) return ByteArray(fallbackBytes)
        val out = ShortArray(last.size)
        if (misses <= MAX_MISSES) {
            val g0 = Math.pow(DECAY.toDouble(), (misses - 1).toDouble()).toFloat()
            val g1 = if (misses == MAX_MISSES) 0f else g0 * DECAY
            for (i in out.indices) {
                val g = g0 + (g1 - g0) * (i / out.size.toFloat())
                out[i] = (last[i] * g).toInt().toShort()
            }
        }
        return toBytes(out)
    }

    private fun toShorts(b: ByteArray): ShortArray {
        val s = ShortArray(b.size / 2)
        ByteBuffer.wrap(b).order(ByteOrder.LITTLE_ENDIAN).asShortBuffer().get(s)
        return s
    }

    private fun toBytes(s: ShortArray): ByteArray {
        val b = ByteBuffer.allocate(s.size * 2).order(ByteOrder.LITTLE_ENDIAN)
        b.asShortBuffer().put(s)
        return b.array()
    }
}
