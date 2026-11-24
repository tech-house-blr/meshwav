package com.techhouseblr.meshwav

import org.junit.Assert.*
import org.junit.Test

/** Same cases as the Rust tests in `crates/audio-protocol/src/clocksync.rs`. */
class ClockSyncTest {
    private val offset = 5_000_000L

    private fun pong(c: ClockSync, sendLocal: Long, there: Long, back: Long) =
        c.onPong(sendLocal, sendLocal + there - offset, sendLocal + there + back)

    @Test fun notReadyUntilBothPiecesArrive() {
        val c = ClockSync()
        assertFalse(c.ready); assertNull(c.capturedAtLocalUs(0))
        pong(c, 10_000_000, 2500, 2500)
        assertFalse(c.ready)
        c.onTimeRef(48_000, 1_000_000)
        assertTrue(c.ready)
    }

    @Test fun symmetricDelayGivesTheExactCaptureTime() {
        val c = ClockSync()
        pong(c, 10_000_000, 2500, 2500)
        c.onTimeRef(48_000, 20_000_000)
        assertEquals(25_000_000L, c.capturedAtLocalUs(48_000))
        assertEquals(25_010_000L, c.capturedAtLocalUs(48_480))
        assertEquals(24_990_000L, c.capturedAtLocalUs(47_520))
        assertEquals(2500L, c.errorUs)
    }

    @Test fun asymmetricDelayErrsByLessThanHalfTheRoundTrip() {
        val c = ClockSync()
        pong(c, 10_000_000, 1000, 9000)
        c.onTimeRef(0, 20_000_000)
        assertTrue(Math.abs(c.capturedAtLocalUs(0)!! - 25_000_000L) <= c.errorUs)
    }

    @Test fun theShortestRoundTripWinsOverACongestedOne() {
        val c = ClockSync()
        pong(c, 10_000_000, 40_000, 2000)
        pong(c, 11_000_000, 2500, 2500)
        pong(c, 12_000_000, 30_000, 1000)
        c.onTimeRef(0, 20_000_000)
        assertEquals(25_000_000L, c.capturedAtLocalUs(0))
        assertEquals(2500L, c.errorUs)
    }

    @Test fun theSampleClockWrappingDoesNotMatter() {
        val c = ClockSync()
        pong(c, 10_000_000, 2500, 2500)
        c.onTimeRef(0xffffffffL - 239, 20_000_000)
        assertEquals(25_010_000L, c.capturedAtLocalUs(240))
        assertEquals(24_990_000L, c.capturedAtLocalUs(0xffffffffL - 719))
    }

    @Test fun oldSamplesAgeOut() {
        val c = ClockSync()
        pong(c, 1_000_000, 100, 100)
        for (i in 0 until 8) pong(c, 2_000_000 + i * 1_000_000L, 5000, 5000)
        assertEquals(5000L, c.errorUs)
    }

    @Test fun aPongFromTheFutureIsIgnored() {
        val c = ClockSync()
        c.onPong(10_000_000, 1, 9_000_000)
        assertEquals(-1L, c.errorUs)
    }
}
