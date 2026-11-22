package com.techhouseblr.meshwav

import org.junit.Assert.*
import org.junit.Test

class ControlTest {
    private fun hex(s: String) = ByteArray(s.length / 2) { s.substring(2 * it, 2 * it + 2).toInt(16).toByte() }
    private fun ByteArray.hex() = joinToString("") { "%02x".format(it) }

    // Same literals as the Rust test runtime_config_messages_have_fixed_wire_bytes.
    @Test fun encodesTheRustBytes() {
        assertEquals("00080b00070000008014", ControlWire.encodeSetConfig(7, 128, 20).hex())
    }

    @Test fun decodesTheRustBytes() {
        assertEquals(ControlWire.Msg.Applied(7, 128, 20), ControlWire.decode(hex("00080c00070000008014")))
        assertEquals(ControlWire.Msg.Rejected(8, 1), ControlWire.decode(hex("00040d000801")))
        assertEquals(ControlWire.Msg.SetConfig(7, 128, 20), ControlWire.decode(hex("00080b00070000008014")))
    }

    @Test fun rejectsMalformed() {
        assertNull(ControlWire.decode(ByteArray(0)))
        assertNull(ControlWire.decode(hex("0008")))
        assertNull(ControlWire.decode(hex("00080c000700000080")))        // truncated
        assertNull(ControlWire.decode(hex("00080c0007000000801400")))    // trailing byte
        assertNull(ControlWire.decode(hex("00040c000801")))              // wrong length for the type
        assertNull(ControlWire.decode(hex("00010c")))
        assertNull(ControlWire.decode(hex("0000")))
        assertNull(ControlWire.decode(hex("00020e0000")))                // unknown type
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
        c.onMessage(ControlWire.Msg.Applied(r2.req, 128, 10))
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
        c.onMessage(ControlWire.Msg.Applied(1, 160, 10))
        assertNull(c.tick(0, 160, 10))
        c.reset()
        assertNotNull(c.tick(0, 160, 10))                 // agent state unknown again: ask once more
    }
}
