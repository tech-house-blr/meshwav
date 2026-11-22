package com.techhouseblr.meshwav

import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.util.Random
import org.junit.Assert.*
import org.junit.Test

/** Loss-injection tests: a simulated 10 ms sender, a lossy/jittery link, and a consumer popping once per 10 ms. */
class JitterBufferTest {
    private class Arrival(val t: Long, val seq: Long)

    private class Result(val frames: Int, val lost: Int, val waits: Int, val maxDepth: Int, val maxCap: Int)

    private fun run(jb: JitterBuffer, arrivals: List<Arrival>, totalMs: Long): Result {
        val sorted = arrivals.sortedBy { it.t }
        var ai = 0
        var frames = 0; var lost = 0; var waits = 0; var maxDepth = 0
        var t = 0L
        while (t <= totalMs) {
            while (ai < sorted.size && sorted[ai].t <= t) {
                jb.push(sorted[ai].seq, byteArrayOf(1), sorted[ai].t); ai++
            }
            when (jb.pop(t).kind) {
                JitterBuffer.Kind.FRAME -> frames++
                JitterBuffer.Kind.LOST -> lost++
                JitterBuffer.Kind.WAIT -> waits++
            }
            maxDepth = maxOf(maxDepth, jb.depthFrames)
            t += 10
        }
        return Result(frames, lost, waits, maxDepth, jb.maxFrames)
    }

    private fun sender(n: Int, startSeq: Long = 0, startT: Long = 0, jitter: (Int) -> Long = { 0 }, drop: (Int) -> Boolean = { false }) =
        (0 until n).filter { !drop(it) }.map { Arrival(startT + it * 10L + jitter(it), startSeq + it) }

    @Test fun cleanLinkPlaysEverythingAfterPrebuffer() {
        val jb = JitterBuffer()
        val r = run(jb, sender(500), 5030)
        assertEquals(0, jb.lost.toInt())
        assertEquals(500, r.frames)
        assertTrue("waits ${r.waits}", r.waits <= 8)
        assertEquals(0, jb.underruns.toInt())
    }

    @Test fun tenPercentRandomLossIsConcealedNotStalled() {
        val rnd = Random(42)
        val dropped = BooleanArray(1000) { it in 11..994 && rnd.nextDouble() < 0.10 }
        val jb = JitterBuffer()
        val r = run(jb, sender(1000, drop = { dropped[it] }), 10_100)
        val nDropped = dropped.count { it }
        assertEquals("every dropped frame concealed exactly once", nDropped, r.lost)
        assertEquals(1000 - nDropped, r.frames)
        assertEquals(0, jb.underruns.toInt())
    }

    @Test fun burstLossOfFiftyMsIsConcealedAndRecovers() {
        val jb = JitterBuffer()
        val r = run(jb, sender(600, drop = { it in 200..204 }), 6300)
        assertEquals(5, r.lost)
        assertEquals(595, r.frames)
    }

    @Test fun adjacentReorderingLosesNothing() {
        val jb = JitterBuffer()
        // every 7th packet is delayed by 12 ms so it arrives after its successor
        val r = run(jb, sender(700, jitter = { if (it % 7 == 3) 12L else 0L }), 7300)
        assertEquals(0, r.lost)
        assertEquals(700, r.frames)
        assertTrue(jb.reordered > 50)
    }

    @Test fun duplicatesAreIgnored() {
        val jb = JitterBuffer()
        val a = sender(300)
        val r = run(jb, a + a.filter { it.seq % 3 == 0L }.map { Arrival(it.t + 1, it.seq) }, 3300)
        assertEquals(300, r.frames)
        assertEquals(100, jb.duplicates.toInt())
    }

    @Test fun delaySpikesGrowTargetAndKeepDepthBounded() {
        val rnd = Random(7)
        val jb = JitterBuffer()
        val r = run(jb, sender(1500, jitter = { if (rnd.nextDouble() < 0.1) 60L else 0L }), 15_500)
        assertTrue("target should have grown, was ${jb.targetFrames}", jb.targetFrames > jb.minFrames)
        assertTrue("depth ${r.maxDepth} must stay under the hard cap", r.maxDepth <= r.maxCap)
    }

    @Test fun targetShrinksBackAfterLinkCalms() {
        val rnd = Random(9)
        val jb = JitterBuffer()
        run(jb, sender(400, jitter = { if (rnd.nextDouble() < 0.2) 80L else 0L }), 4300)
        val grown = jb.targetFrames
        assertTrue(grown > jb.minFrames)
        run(jb, sender(3000, startSeq = 400, startT = 4300), 4300 + 31_000) // 30 s of clean link
        assertTrue("target $grown -> ${jb.targetFrames}", jb.targetFrames < grown)
    }

    @Test fun oneSecondOutageRebuffersAndResumesWithBoundedDepth() {
        val jb = JitterBuffer()
        val arrivals = sender(300, drop = { it in 100..199 }) // 1 s of nothing
        val r = run(jb, arrivals, 3300)
        assertEquals("every frame that was sent is eventually played", 200, r.frames)
        assertTrue(jb.underruns >= 1)
        assertTrue(r.maxDepth <= r.maxCap)
    }

    @Test fun quickSenderRestartRecovers() {
        val jb = JitterBuffer()
        val first = sender(500)
        val second = sender(500, startSeq = 0, startT = 5030) // agent restarted fast: seq back to 0
        val r = run(jb, first + second, 10_100)
        assertTrue("frames ${r.frames}", r.frames >= 960)
    }

    @Test fun slowSenderRestartRecoversAfterRebuffer() {
        val jb = JitterBuffer()
        val first = sender(500)
        val second = sender(500, startSeq = 0, startT = 5400) // agent restarted after a 400 ms gap
        val r = run(jb, first + second, 10_500)
        assertTrue("frames ${r.frames}", r.frames >= 995)
        assertTrue(jb.underruns >= 1)
    }

    @Test fun latePacketsAreCountedAndDropped() {
        val jb = JitterBuffer()
        run(jb, sender(300, jitter = { if (it == 100) 400L else 0L }), 3300)
        assertTrue(jb.late >= 1)
    }

    @Test fun concealerFadesToSilenceAndKeepsLength() {
        val c = Concealer()
        val pcm = ByteBuffer.allocate(1920).order(ByteOrder.LITTLE_ENDIAN)
        for (i in 0 until 960) pcm.putShort(10000)
        c.accept(pcm.array())
        var prevPeak = 10000
        var last = ByteArray(0)
        for (i in 1..6) {
            last = c.conceal(1920)
            assertEquals(1920, last.size)
            val s = ByteBuffer.wrap(last).order(ByteOrder.LITTLE_ENDIAN).asShortBuffer()
            var peak = 0
            for (k in 0 until s.limit()) peak = maxOf(peak, Math.abs(s.get(k).toInt()))
            assertTrue("peak $peak must not rise above $prevPeak", peak <= prevPeak)
            prevPeak = peak
        }
        assertTrue(last.all { it.toInt() == 0 })
    }

    @Test fun concealerFadesInAfterLoss() {
        val c = Concealer()
        val pcm = ByteBuffer.allocate(1920).order(ByteOrder.LITTLE_ENDIAN)
        for (i in 0 until 960) pcm.putShort(10000)
        c.accept(pcm.array()); c.conceal(1920)
        val out = ByteBuffer.wrap(c.accept(pcm.array())).order(ByteOrder.LITTLE_ENDIAN).asShortBuffer()
        assertEquals(0, out.get(0).toInt())
        assertEquals(10000, out.get(Concealer.FADE_IN_SAMPLES + 10).toInt())
    }
}
