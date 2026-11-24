package com.techhouseblr.meshwav

import java.util.TreeMap
import kotlin.math.abs
import kotlin.math.ceil

/**
 * Adaptive jitter buffer: pure logic, no threads, no Android classes, clock passed in, so it is
 * unit-testable with simulated loss, reordering and delay. The caller serialises access.
 *
 * Target depth follows an RFC 3550 style interarrival jitter estimate: it grows immediately when
 * jitter or late packets appear and shrinks by one frame per second. Total depth is capped so
 * latency stays bounded (stale audio is worthless).
 */
class JitterBuffer(
    val frameMs: Int = 10,
    val minFrames: Int = 2,
    val maxFrames: Int = 20,   // hard cap, 200 ms at 10 ms frames
    val lossGap: Int = 2,      // this many later frames present => the missing one is declared lost
) {
    enum class Kind { FRAME, LOST, WAIT }
    class Out(val kind: Kind, val payload: ByteArray? = null)

    companion object {
        /**
         * Same 200 ms hard cap for every frame size. The floor is 20 ms, or 10 ms in low-latency mode (opt-in:
         * it leaves less room for network jitter, so a weak link will conceal more).
         */
        fun forFrameMs(ms: Int, lowLatency: Boolean = false): JitterBuffer {
            val floorMs = if (lowLatency) 10 else 20
            return JitterBuffer(frameMs = ms, minFrames = maxOf(if (lowLatency) 1 else 2, (floorMs + ms - 1) / ms), maxFrames = 200 / ms)
        }

        const val RESYNC_GAP = 1000L      // sequence jump this large means the sender restarted
        const val REBUFFER_MS = 150L      // empty this long => underrun, rebuffer
        const val SHRINK_EVERY_MS = 1000L // target shrinks at most one frame per second
    }

    private val buf = TreeMap<Long, ByteArray>()
    private var next = -1L
    private var maxSeq = -1L
    private var emptySince = 0L
    private var holeSince = 0L
    private var lastShrink = 0L
    private var prevTransit = Double.NaN

    var jitterMs = 0.0; private set
    var targetFrames = minFrames; private set

    var received = 0L; private set
    var lost = 0L; private set
    var late = 0L; private set
    var duplicates = 0L; private set
    var reordered = 0L; private set
    var trimmed = 0L; private set
    var underruns = 0L; private set
    var resyncs = 0L; private set

    val depthFrames: Int get() = buf.size

    fun push(seq: Long, payload: ByteArray, nowMs: Long) {
        val ref = if (next >= 0) next else (if (buf.isEmpty()) maxSeq else buf.firstKey())
        if (maxSeq >= 0 && (seq + RESYNC_GAP < ref || seq > maxSeq + RESYNC_GAP)) resync()
        if (next >= 0 && seq < next) {
            late++
            targetFrames = minOf(maxFrames, targetFrames + 1)
            lastShrink = nowMs
            return
        }
        if (buf.containsKey(seq)) { duplicates++; return }
        if (maxSeq >= 0 && seq < maxSeq) reordered++
        received++

        val transit = nowMs - seq * frameMs.toDouble()
        if (!prevTransit.isNaN()) jitterMs += (abs(transit - prevTransit) - jitterMs) / 16.0
        prevTransit = transit

        buf[seq] = payload
        if (seq > maxSeq) maxSeq = seq

        val want = (ceil(4 * jitterMs / frameMs).toInt() + minFrames).coerceIn(minFrames, maxFrames)
        if (want > targetFrames) {
            targetFrames = want; lastShrink = nowMs
        } else if (want < targetFrames && nowMs - lastShrink >= SHRINK_EVERY_MS) {
            targetFrames--; lastShrink = nowMs
        }
    }

    /** Called once per playout slot (one frame of audio needed). */
    fun pop(nowMs: Long): Out {
        if (next < 0) {
            if (buf.size >= targetFrames) { next = buf.firstKey(); emptySince = 0; holeSince = 0 } else return Out(Kind.WAIT)
        }
        val cap = minOf(maxFrames, targetFrames * 2 + 2)
        if (maxSeq - next + 1 > cap) { // too far behind: skip ahead, bounded latency
            val n2 = maxSeq - targetFrames + 1
            trimmed += n2 - next
            buf.headMap(n2).clear()
            next = n2
        }
        val f = buf.remove(next)
        if (f != null) { next++; emptySince = 0; holeSince = 0; return Out(Kind.FRAME, f) }
        if (maxSeq > next) { // hole with later audio queued
            if (holeSince == 0L) holeSince = nowMs
            if (maxSeq - next >= lossGap || nowMs - holeSince >= 2L * frameMs) {
                next++; lost++; holeSince = 0
                return Out(Kind.LOST)
            }
            return Out(Kind.WAIT)
        }
        // nothing buffered at all
        if (emptySince == 0L) emptySince = nowMs
        if (nowMs - emptySince > REBUFFER_MS) {
            underruns++
            targetFrames = minOf(maxFrames, targetFrames + 1)
            lastShrink = nowMs
            // Start from scratch: whatever arrives next defines the new base (the sender may have restarted).
            next = -1; maxSeq = -1; emptySince = 0; prevTransit = Double.NaN
        }
        return Out(Kind.WAIT)
    }

    private fun resync() {
        buf.clear(); next = -1; maxSeq = -1; emptySince = 0; holeSince = 0
        prevTransit = Double.NaN
        resyncs++
    }
}
