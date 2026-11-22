package com.techhouseblr.meshwav

import java.nio.ByteBuffer

/** Runtime settings chosen in the UI; 0 means "leave it to the agent". */
object Controls {
    @Volatile var desiredKbps = 0
    @Volatile var desiredFrameMs = 0
    val kbpsChoices = intArrayOf(64, 96, 128, 160, 192, 256)
    val frameChoices = intArrayOf(5, 10, 20)
}

/** Control messages (docs/protocol.md section 0), same framing as the Rust side: `u16 len | u8 type | body`. */
object ControlWire {
    const val TYPE_SET_CONFIG = 11
    const val TYPE_CONFIG_APPLIED = 12
    const val TYPE_CONFIG_REJECTED = 13
    const val REJECT_BITRATE = 1
    const val REJECT_FRAME = 2
    const val REJECT_ENCODER = 3
    const val FLAG_FRAME_SHIFT = 2
    const val FLAG_FRAME_MASK = 0b111 shl FLAG_FRAME_SHIFT

    sealed class Msg {
        data class SetConfig(val req: Int, val kbps: Int, val frameMs: Int) : Msg()
        data class Applied(val req: Int, val kbps: Int, val frameMs: Int) : Msg()
        data class Rejected(val req: Int, val reason: Int) : Msg()
    }

    fun encodeSetConfig(req: Int, kbps: Int, frameMs: Int): ByteArray {
        val b = ByteBuffer.allocate(2 + 1 + 2 + 4 + 1)
        b.putShort(8).put(TYPE_SET_CONFIG.toByte()).putShort(req.toShort()).putInt(kbps).put(frameMs.toByte())
        return b.array()
    }

    /** Strict: null for anything that is not exactly one known message. */
    fun decode(d: ByteArray): Msg? {
        if (d.size < 3) return null
        val b = ByteBuffer.wrap(d)
        val len = b.short.toInt() and 0xffff
        if (len == 0 || d.size != 2 + len) return null
        return when (b.get().toInt() and 0xff) {
            TYPE_CONFIG_APPLIED -> if (len == 8) Msg.Applied(b.short.toInt() and 0xffff, b.int, b.get().toInt() and 0xff) else null
            TYPE_CONFIG_REJECTED -> if (len == 4) Msg.Rejected(b.short.toInt() and 0xffff, b.get().toInt() and 0xff) else null
            TYPE_SET_CONFIG -> if (len == 8) Msg.SetConfig(b.short.toInt() and 0xffff, b.int, b.get().toInt() and 0xff) else null
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

    fun rejectText(reason: Int) = when (reason) {
        REJECT_BITRATE -> "bitrate out of range"
        REJECT_FRAME -> "frame size not supported"
        REJECT_ENCODER -> "encoder could not switch"
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
    var note: String? = null; private set

    private var nextReq = 1
    private var outReq = -1
    private var outKbps = 0
    private var outFrame = 0
    private var tries = 0
    private var lastSent = 0L
    private var blockedKbps = -1
    private var blockedFrame = -1

    /** New session: the agent's state is unknown again, and earlier refusals no longer apply. */
    fun reset() {
        appliedKbps = 0; appliedFrameMs = 0; note = null; outReq = -1; tries = 0
        blockedKbps = -1; blockedFrame = -1
    }

    /** The frame size every audio packet announces is the truth about what the agent is encoding. */
    fun frameSeen(ms: Int) { if (ms != 0) appliedFrameMs = ms }

    /** Returns an encoded request to send now, or null. */
    fun tick(nowMs: Long, wantKbps: Int, wantFrameMs: Int): ByteArray? {
        val needK = wantKbps != 0 && wantKbps != appliedKbps
        val needF = wantFrameMs != 0 && wantFrameMs != appliedFrameMs
        if (!needK && !needF) { outReq = -1; return null }
        if (wantKbps == blockedKbps && wantFrameMs == blockedFrame) return null
        if (outReq < 0 || outKbps != wantKbps || outFrame != wantFrameMs) {
            outReq = nextReq; nextReq = (nextReq + 1) and 0xffff
            outKbps = wantKbps; outFrame = wantFrameMs; tries = 0; note = null
        } else if (nowMs - lastSent < retryMs) return null
        if (tries >= maxTries) {
            note = "the agent did not answer"
            blockedKbps = wantKbps; blockedFrame = wantFrameMs; outReq = -1
            return null
        }
        tries++; lastSent = nowMs
        // Only ask for what is actually different; 0 tells the agent to keep its current value.
        return ControlWire.encodeSetConfig(outReq, if (needK) wantKbps else 0, if (needF) wantFrameMs else 0)
    }

    fun onMessage(m: ControlWire.Msg) {
        when (m) {
            is ControlWire.Msg.Applied -> {
                appliedKbps = m.kbps; appliedFrameMs = m.frameMs
                if (m.req == outReq) { outReq = -1; note = null }
            }
            is ControlWire.Msg.Rejected -> if (m.req == outReq) {
                note = "agent refused: " + ControlWire.rejectText(m.reason)
                blockedKbps = outKbps; blockedFrame = outFrame; outReq = -1
            }
            else -> {}
        }
    }
}
