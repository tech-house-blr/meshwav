package com.techhouseblr.meshwav

import org.junit.Assert.*
import org.junit.Test

/** The jitter buffer keeps the same 200 ms cap for every frame size and a 20 ms floor (at least 2 frames) the agent can switch to. */
class FrameSizeTest {
    @Test fun floorAndCapAreTimeBasedNotFrameBased() {
        for (ms in listOf(5, 10, 20)) {
            val jb = JitterBuffer.forFrameMs(ms)
            assertEquals(ms, jb.frameMs)
            assertEquals(200, jb.maxFrames * ms)
            assertEquals("floor at $ms ms", maxOf(20, 2 * ms), jb.minFrames * ms) // never below 2 frames, so 40 ms at 20 ms frames
            assertEquals(jb.minFrames, jb.targetFrames)
        }
    }

    /** Clean link, consumer pops once per frame: nothing is lost or concealed at any frame size. */
    @Test fun cleanLinkAtEveryFrameSize() {
        for (ms in listOf(5, 10, 20)) {
            val jb = JitterBuffer.forFrameMs(ms)
            var frames = 0; var lost = 0
            var t = 0L; var seq = 0L
            while (t < 5000) {
                jb.push(seq++, byteArrayOf(1), t)
                when (jb.pop(t).kind) {
                    JitterBuffer.Kind.FRAME -> frames++
                    JitterBuffer.Kind.LOST -> lost++
                    else -> {}
                }
                t += ms
            }
            assertEquals("lost at $ms ms", 0, lost)
            assertTrue("frames at $ms ms: $frames", frames > 5000 / ms - 10)
            assertTrue("depth stays near the floor at $ms ms", jb.depthFrames * ms <= 60)
        }
    }

    /** 3% random loss and +-ms jitter at 5 and 20 ms: concealed frames track the loss, depth respects the cap. */
    @Test fun lossyLinkRespectsCap() {
        for (ms in listOf(5, 20)) {
            val rnd = java.util.Random(7)
            val jb = JitterBuffer.forFrameMs(ms)
            var lost = 0; var maxDepth = 0
            val arrivals = (0 until (8000 / ms)).filter { rnd.nextInt(100) >= 3 }.map { it.toLong() to (it * ms.toLong() + rnd.nextInt(ms)) }.sortedBy { it.second }
            var ai = 0; var t = 0L
            while (t <= 8000) {
                while (ai < arrivals.size && arrivals[ai].second <= t) { jb.push(arrivals[ai].first, byteArrayOf(1), arrivals[ai].second); ai++ }
                if (jb.pop(t).kind == JitterBuffer.Kind.LOST) lost++
                maxDepth = maxOf(maxDepth, jb.depthFrames)
                t += ms
            }
            val sent = 8000 / ms
            assertTrue("lost $lost of $sent at $ms ms", lost in (sent * 2 / 100)..(sent * 6 / 100))
            assertTrue("depth $maxDepth frames at $ms ms", maxDepth * ms <= 200)
        }
    }
}
