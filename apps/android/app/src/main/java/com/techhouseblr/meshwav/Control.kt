package com.techhouseblr.meshwav

import java.nio.ByteBuffer

/** Runtime settings chosen in the UI; 0 means "leave it to the agent". */
object Controls {
    @Volatile var desiredKbps = 0
    @Volatile var desiredFrameMs = 0
    /** 1 = Opus, 2 = lossless PCM. */
    @Volatile var desiredCodec = 1
    val kbpsChoices = intArrayOf(64, 96, 128, 160, 192, 256)
    val frameChoices = intArrayOf(5, 10, 20)
}

/** Control messages (docs/protocol.md section 0), same framing as the Rust side: `u16 len | u8 type | body`. */
object ControlWire {
    const val TYPE_BYE = 9
    const val TYPE_SET_CONFIG = 11
    const val TYPE_CONFIG_APPLIED = 12
    const val TYPE_CONFIG_REJECTED = 13
    const val REJECT_BITRATE = 1
    const val REJECT_FRAME = 2
    const val REJECT_ENCODER = 3
    const val REJECT_CODEC = 4
    const val CODEC_OPUS = 1
    const val CODEC_PCM = 2
    const val FLAG_PCM = 1 shl 5
    /** 5 ms of 48 kHz stereo 16-bit PCM: one whole datagram, so lossless needs no fragmentation. */
    const val PCM_FRAME_BYTES = 960
    const val FLAG_FRAME_SHIFT = 2
    const val FLAG_FRAME_MASK = 0b111 shl FLAG_FRAME_SHIFT

    sealed class Msg {
        data class SetConfig(val req: Int, val kbps: Int, val frameMs: Int, val codec: Int) : Msg()
        data class Applied(val req: Int, val kbps: Int, val frameMs: Int, val codec: Int) : Msg()
        data class Rejected(val req: Int, val reason: Int) : Msg()
    }

    /** Goodbye: tells the agent to stop and give the PC's audio output back at once. */
    fun encodeBye(): ByteArray = ByteBuffer.allocate(4).putShort(2).put(TYPE_BYE.toByte()).put(0).array()

    fun encodeSetConfig(req: Int, kbps: Int, frameMs: Int, codec: Int = 0): ByteArray {
        val b = ByteBuffer.allocate(2 + 1 + 2 + 4 + 1 + 1)
        b.putShort(9).put(TYPE_SET_CONFIG.toByte()).putShort(req.toShort()).putInt(kbps).put(frameMs.toByte()).put(codec.toByte())
        return b.array()
    }

    /** Strict: null for anything that is not exactly one known message. */
    fun decode(d: ByteArray): Msg? {
        if (d.size < 3) return null
        val b = ByteBuffer.wrap(d)
        val len = b.short.toInt() and 0xffff
        if (len == 0 || d.size != 2 + len) return null
        return when (b.get().toInt() and 0xff) {
            TYPE_CONFIG_APPLIED -> if (len == 9) Msg.Applied(b.short.toInt() and 0xffff, b.int, b.get().toInt() and 0xff, b.get().toInt() and 0xff) else null
            TYPE_CONFIG_REJECTED -> if (len == 4) Msg.Rejected(b.short.toInt() and 0xffff, b.get().toInt() and 0xff) else null
            TYPE_SET_CONFIG -> if (len == 9) Msg.SetConfig(b.short.toInt() and 0xffff, b.int, b.get().toInt() and 0xff, b.get().toInt() and 0xff) else null
            else -> null
        }
    }

    /** Frame duration announced in an audio packet's flags (5, 10 or 20), or 0 if absent or invalid. */
    fun frameMsFromFlags(flags: Int): Int = when ((flags and FLAG_FRAME_MASK) shr FLAG_FRAME_SHIFT) {
        1 -> 5
        2 -> 10
        4 -> 20
        else -> 0
    }

    fun isPcm(flags: Int) = flags and FLAG_PCM != 0

    /**
     * An audio packet is lossless PCM exactly when its flag says so, and then it must be one whole 5 ms frame.
     * The size doubles as the codec marker downstream (an Opus packet is never 960 bytes), so a packet whose
     * flag and size disagree is dropped rather than played as noise.
     */
    fun audioPlausible(flags: Int, size: Int): Boolean =
        if (isPcm(flags)) size == PCM_FRAME_BYTES && frameMsFromFlags(flags) == 5 else size in 1 until PCM_FRAME_BYTES

    fun rejectText(reason: Int) = when (reason) {
        REJECT_BITRATE -> "bitrate out of range"
        REJECT_FRAME -> "frame size not supported"
        REJECT_ENCODER -> "encoder could not switch"
        REJECT_CODEC -> "codec not supported"
        else -> "refused ($reason)"
    }
}

/**
 * Receiver side of the config exchange. Pure logic with an injected clock. Re-sends a request every
 * [retryMs] until the agent answers (the agent replays its answer for a repeated id and never applies
 * it twice), gives up after [maxTries], and never re-sends a request the agent refused.
 */
class ConfigClient(private val retryMs: Long = 300, private val maxTries: Int = 8) {
    var appliedKbps = 0; private set
    var appliedFrameMs = 0; private set
    /** 0 = unknown, else CODEC_OPUS or CODEC_PCM. */
    var appliedCodec = 0; private set
    var note: String? = null; private set

    private var nextReq = 1
    private var outReq = -1
    private var outKbps = 0
    private var outFrame = 0
    private var outCodec = 0
    private var tries = 0
    private var lastSent = 0L
    private var blockedKbps = -1
    private var blockedFrame = -1
    private var blockedCodec = -1

    /** New session: the agent's state is unknown again, and earlier refusals no longer apply. */
    fun reset() {
        appliedKbps = 0; appliedFrameMs = 0; appliedCodec = 0; note = null; outReq = -1; tries = 0
        blockedKbps = -1; blockedFrame = -1; blockedCodec = -1
    }

    /** The frame size every audio packet announces is the truth about what the agent is encoding. */
    fun frameSeen(ms: Int) { if (ms != 0) appliedFrameMs = ms }

    /** Same for the codec: every audio packet says whether it is PCM. */
    fun codecSeen(pcm: Boolean) { appliedCodec = if (pcm) ControlWire.CODEC_PCM else ControlWire.CODEC_OPUS }

    /** Returns an encoded request to send now, or null. */
    fun tick(nowMs: Long, wantKbps: Int, wantFrameMs: Int, wantCodec: Int = 0): ByteArray? {
        val needK = wantKbps != 0 && wantKbps != appliedKbps
        val needF = wantFrameMs != 0 && wantFrameMs != appliedFrameMs
        val needC = wantCodec != 0 && wantCodec != appliedCodec
        if (!needK && !needF && !needC) { outReq = -1; return null }
        if (wantKbps == blockedKbps && wantFrameMs == blockedFrame && wantCodec == blockedCodec) return null
        if (outReq < 0 || outKbps != wantKbps || outFrame != wantFrameMs || outCodec != wantCodec) {
            outReq = nextReq; nextReq = (nextReq + 1) and 0xffff
            outKbps = wantKbps; outFrame = wantFrameMs; outCodec = wantCodec; tries = 0; note = null
        } else if (nowMs - lastSent < retryMs) return null
        if (tries >= maxTries) {
            note = "the agent did not answer"
            blockedKbps = wantKbps; blockedFrame = wantFrameMs; blockedCodec = wantCodec; outReq = -1
            return null
        }
        tries++; lastSent = nowMs
        // Only ask for what is actually different; 0 tells the agent to keep its current value.
        return ControlWire.encodeSetConfig(outReq, if (needK) wantKbps else 0, if (needF) wantFrameMs else 0, if (needC) wantCodec else 0)
    }

    fun onMessage(m: ControlWire.Msg) {
        when (m) {
            is ControlWire.Msg.Applied -> {
                appliedKbps = m.kbps; appliedFrameMs = m.frameMs; appliedCodec = m.codec
                if (m.req == outReq) { outReq = -1; note = null }
            }
            is ControlWire.Msg.Rejected -> if (m.req == outReq) {
                note = "agent refused: " + ControlWire.rejectText(m.reason)
                blockedKbps = outKbps; blockedFrame = outFrame; blockedCodec = outCodec; outReq = -1
            }
            else -> {}
        }
    }
}

/**
 * Watches a lossless stream and says when the link cannot carry it. Lossless is about 1.7 Mbit/s in 200
 * packets a second, so a weak Wi-Fi link shows up as lost or late packets. Fed cumulative counters; looks at
 * one window at a time and fires when at least [minPackets] were expected and [maxBad] of them were bad.
 */
class LosslessGuard(private val windowMs: Long = 4000, private val minPackets: Int = 400, private val maxBad: Double = 0.02) {
    private var start = -1L
    private var good0 = 0L
    private var bad0 = 0L

    fun reset() { start = -1 }

    /** [good] packets received so far, [bad] = lost + late + underruns so far. True when the stream should fall back. */
    fun feed(nowMs: Long, good: Long, bad: Long): Boolean {
        if (start < 0 || good < good0 || bad < bad0) { start = nowMs; good0 = good; bad0 = bad; return false }
        if (nowMs - start < windowMs) return false
        val g = good - good0
        val b = bad - bad0
        start = nowMs; good0 = good; bad0 = bad
        val total = g + b
        return total >= minPackets && b.toDouble() / total >= maxBad
    }
}
