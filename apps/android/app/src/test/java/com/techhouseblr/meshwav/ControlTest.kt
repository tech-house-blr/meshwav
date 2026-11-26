package com.techhouseblr.meshwav

import org.junit.Assert.*
import org.junit.Test

class ControlTest {
    private fun hex(s: String) = ByteArray(s.length / 2) { s.substring(2 * it, 2 * it + 2).toInt(16).toByte() }
    private fun ByteArray.hex() = joinToString("") { "%02x".format(it) }

    // Same literals as the Rust test runtime_config_messages_have_fixed_wire_bytes.
    @Test fun byeIsTwoBytesOfBodyAndMatchesRust() {
        assertEquals("00020900", ControlWire.encodeBye().hex())
    }

    @Test fun encodesTheRustBytes() {
        assertEquals("00090b0007000000801400", ControlWire.encodeSetConfig(7, 128, 20).hex())
    }

    @Test fun decodesTheRustBytes() {
        assertEquals(ControlWire.Msg.Applied(7, 128, 20, 1), ControlWire.decode(hex("00090c0007000000801401")))
        assertEquals(ControlWire.Msg.Rejected(8, 1), ControlWire.decode(hex("00040d000801")))
        assertEquals(ControlWire.Msg.SetConfig(7, 128, 20, 0), ControlWire.decode(hex("00090b0007000000801400")))
    }

    // Same literals as the Rust test runtime_config_messages_have_fixed_wire_bytes.
    @Test fun activeStateMatchesRust() {
        assertEquals(ControlWire.Msg.TimeRef(300, 0x0102030405060708), ControlWire.decode(hex("000d0f0000012c0102030405060708")))
        assertEquals(ControlWire.Msg.Pong(1, 2), ControlWire.decode(hex("00110700000000000000010000000000000002")))
        assertEquals("0009060102030405060708", ControlWire.encodePing(0x0102030405060708).hex())
        assertEquals(ControlWire.Msg.Active(true), ControlWire.decode(hex("00020e01")))
        assertEquals(ControlWire.Msg.Active(false), ControlWire.decode(hex("00020e00")))
        assertEquals(ControlWire.Msg.PlayHereSupported, ControlWire.decode(hex("000111")))
        assertNull(ControlWire.decode(hex("00021100")))
        assertEquals("000110", ControlWire.encodePlayHere().joinToString("") { "%02x".format(it) })
        assertNull(ControlWire.decode(hex("00020e02")))      // not a bool
        assertNull(ControlWire.decode(hex("00030e0100")))    // wrong length
        assertNull(ControlWire.decode(hex("00010e")))
    }

    @Test fun rejectsMalformed() {
        assertNull(ControlWire.decode(ByteArray(0)))
        assertNull(ControlWire.decode(hex("0008")))
        assertNull(ControlWire.decode(hex("00090c00070000008014")))        // truncated
        assertNull(ControlWire.decode(hex("00090c000700000080140100")))    // trailing byte
        assertNull(ControlWire.decode(hex("00040c000801")))              // wrong length for the type
        assertNull(ControlWire.decode(hex("00010c")))
        assertNull(ControlWire.decode(hex("0000")))
        assertNull(ControlWire.decode(hex("00020f00")))                  // unknown type
        assertNull(ControlWire.decode(hex("ffff0c")))
    }

    @Test fun frameFlags() {
        for ((code, ms) in listOf(1 to 5, 2 to 10, 4 to 20)) {
            assertEquals(ms, ControlWire.frameMsFromFlags(code shl 2))
            assertEquals(ms, ControlWire.frameMsFromFlags((code shl 2) or 0b11)) // other bits ignored
        }
        for (code in listOf(0, 3, 5, 6, 7)) assertEquals(0, ControlWire.frameMsFromFlags(code shl 2))
    }

    private fun decodeReq(b: ByteArray) = ControlWire.decode(b) as ControlWire.Msg.SetConfig

    @Test fun clientSendsOnlyWhatDiffersAndRetries() {
        val c = ConfigClient(retryMs = 300, maxTries = 3)
        assertNull(c.tick(0, 0, 0))                    // no preference
        c.frameSeen(10)
        assertNull(c.tick(0, 0, 10))                   // already in force
        val r1 = decodeReq(c.tick(100, 128, 10)!!)     // bitrate unknown, frame already 10: only bitrate asked
        assertEquals(128, r1.kbps); assertEquals(0, r1.frameMs)
        assertNull(c.tick(200, 128, 10))               // too soon
        val r2 = decodeReq(c.tick(400, 128, 10)!!)
        assertEquals(r1.req, r2.req)                   // a retry reuses the id
        c.onMessage(ControlWire.Msg.Applied(r2.req, 128, 10, 1))
        assertEquals(128, c.appliedKbps)
        assertNull(c.tick(1000, 128, 10))              // done, nothing more to send
    }

    @Test fun changedWishGetsANewId() {
        val c = ConfigClient()
        c.frameSeen(10)
        val a = decodeReq(c.tick(0, 96, 0)!!)
        val b = decodeReq(c.tick(10, 64, 0)!!)         // user changed their mind: send at once
        assertNotEquals(a.req, b.req)
        assertEquals(64, b.kbps)
    }

    @Test fun givesUpAfterMaxTriesAndNotesIt() {
        val c = ConfigClient(retryMs = 100, maxTries = 3)
        var sent = 0
        var t = 0L
        while (t < 5000) { if (c.tick(t, 160, 0) != null) sent++; t += 50 }
        assertEquals(3, sent)
        assertEquals("the agent did not answer", c.note)
        assertNotNull(c.tick(6000, 96, 0))             // a different wish is tried again
    }

    @Test fun refusalIsNotRetriedAndShowsWhy() {
        val c = ConfigClient()
        val r = decodeReq(c.tick(0, 999, 0)!!)
        c.onMessage(ControlWire.Msg.Rejected(r.req, ControlWire.REJECT_BITRATE))
        assertEquals("agent refused: bitrate out of range", c.note)
        assertNull(c.tick(1000, 999, 0))
        assertNull(c.tick(5000, 999, 0))
    }

    @Test fun staleReplyDoesNotClearTheCurrentRequest() {
        val c = ConfigClient()
        val a = decodeReq(c.tick(0, 96, 0)!!)
        val b = decodeReq(c.tick(10, 128, 0)!!)
        c.onMessage(ControlWire.Msg.Rejected(a.req, 1))   // late answer to the first request
        assertNull(c.note)
        assertNotNull(c.tick(1000, 128, 0))               // second request still outstanding, retried
        assertEquals(b.req, decodeReq(c.tick(2000, 128, 0)!!).req)
    }

    @Test fun newSessionForgetsAppliedState() {
        val c = ConfigClient()
        c.onMessage(ControlWire.Msg.Applied(1, 160, 10, 1))
        assertNull(c.tick(0, 160, 10))
        c.reset()
        assertNotNull(c.tick(0, 160, 10))                 // agent state unknown again: ask once more
    }

    @Test fun losslessRequestMatchesRust() {
        assertEquals("00090b0001000000000002", ControlWire.encodeSetConfig(1, 0, 0, 2).hex())
        assertEquals(ControlWire.Msg.Applied(1, 160, 10, 2), ControlWire.decode(hex("00090c0001000000a00a02")))
    }

    @Test fun pcmPacketsMustBeOneWholeFiveMsFrame() {
        val pcm = ControlWire.FLAG_PCM or (1 shl 2)           // PCM, 5 ms
        assertTrue(ControlWire.audioPlausible(pcm, 960))
        assertFalse(ControlWire.audioPlausible(pcm, 959))
        assertFalse(ControlWire.audioPlausible(pcm, 1164))
        assertFalse(ControlWire.audioPlausible(ControlWire.FLAG_PCM or (2 shl 2), 960))  // PCM claiming 10 ms
        assertFalse(ControlWire.audioPlausible(ControlWire.FLAG_PCM, 960))               // PCM without a frame size
        assertTrue(ControlWire.audioPlausible(2 shl 2, 160))   // Opus
        assertFalse(ControlWire.audioPlausible(2 shl 2, 960))  // size says PCM, flag says Opus
        assertFalse(ControlWire.audioPlausible(2 shl 2, 0))
    }

    @Test fun clientAsksForLosslessOnlyWhenNotAlreadyPlaying() {
        val c = ConfigClient()
        c.codecSeen(false)
        val r = decodeReq(c.tick(0, 0, 0, ControlWire.CODEC_PCM)!!)
        assertEquals(ControlWire.CODEC_PCM, r.codec)
        assertEquals(0, r.kbps); assertEquals(0, r.frameMs)  // nothing else asked
        c.onMessage(ControlWire.Msg.Applied(r.req, 160, 10, ControlWire.CODEC_PCM))
        assertNull(c.tick(1000, 0, 0, ControlWire.CODEC_PCM))
        c.codecSeen(true)
        assertNull(c.tick(2000, 0, 0, ControlWire.CODEC_PCM))
        assertEquals(ControlWire.CODEC_OPUS, decodeReq(c.tick(3000, 0, 0, ControlWire.CODEC_OPUS)!!).codec) // and back
    }

    @Test fun refusedCodecIsNotAskedAgain() {
        val c = ConfigClient()
        c.codecSeen(false)
        val r = decodeReq(c.tick(0, 0, 0, ControlWire.CODEC_PCM)!!)
        c.onMessage(ControlWire.Msg.Rejected(r.req, ControlWire.REJECT_CODEC))
        assertNull(c.tick(1000, 0, 0, ControlWire.CODEC_PCM))
        assertTrue(c.note!!.contains("codec"))
    }

    @Test fun guardFiresOnlyForSustainedLossWithEnoughTraffic() {
        val g = LosslessGuard(windowMs = 4000, minPackets = 400, maxBad = 0.02)
        var good = 0L; var bad = 0L; var t = 0L
        fun run(sec: Int, lossPct: Double): Boolean {
            var fired = false
            repeat(sec) { good += (200 * (1 - lossPct)).toLong(); bad += (200 * lossPct).toLong(); t += 1000; if (g.feed(t, good, bad)) fired = true }
            return fired
        }
        assertFalse(run(12, 0.0))      // clean link
        assertFalse(run(12, 0.01))     // 1% is tolerated
        assertTrue(run(5, 0.05))       // 5% is not
        g.reset()
        assertFalse(g.feed(t, good, bad))
        t += 4000; good += 10; bad += 10   // a window with almost no traffic proves nothing
        assertFalse(g.feed(t, good, bad))
    }
}
