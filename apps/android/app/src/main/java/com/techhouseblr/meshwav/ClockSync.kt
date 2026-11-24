package com.techhouseblr.meshwav

/**
 * Receiver-side clock mapping for end-to-end latency (port of `crates/audio-protocol/src/clocksync.rs`;
 * the same test vectors run on both sides). Pure arithmetic, no clock reads. All times are microseconds.
 *
 * Ping/Pong gives the offset between this device's clock and the agent's; `TimeRef` says which agent-clock
 * time a given audio timestamp was captured at. Together: when each frame was captured, on our clock.
 */
class ClockSync {
    private class Sample(val offsetUs: Long, val rttUs: Long)
    private val samples = ArrayDeque<Sample>()
    private var refTs = 0L
    private var refAgentUs = 0L
    private var haveRef = false

    /** `echoUs` is our own send time, `agentUs` the agent's clock when it answered, `nowUs` our clock on arrival. */
    fun onPong(echoUs: Long, agentUs: Long, nowUs: Long) {
        val rtt = nowUs - echoUs
        if (rtt < 0) return
        val midLocal = echoUs + rtt / 2
        samples.addLast(Sample(midLocal - agentUs, rtt))
        if (samples.size > KEEP) samples.removeFirst()
    }

    fun onTimeRef(ts: Long, agentUs: Long) { refTs = ts and 0xffffffffL; refAgentUs = agentUs; haveRef = true }

    private fun best(): Sample? = samples.minByOrNull { it.rttUs }

    val ready: Boolean get() = best() != null && haveRef

    /** Worst-case error of the clock offset (half the best round trip), or -1 when unknown. */
    val errorUs: Long get() = best()?.let { it.rttUs / 2 } ?: -1L

    /** When the frame stamped `ts` was captured, on our clock; null until both pieces are known. */
    fun capturedAtLocalUs(ts: Long): Long? {
        val b = best() ?: return null
        if (!haveRef) return null
        val dts = ((ts - refTs).toInt()).toLong() // signed 32-bit difference, survives the wrap
        return refAgentUs + dts * 1_000_000L / RATE + b.offsetUs
    }

    companion object {
        const val RATE = 48_000L
        private const val KEEP = 8
    }
}
