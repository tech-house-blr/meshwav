package com.techhouseblr.meshwav

import org.junit.Assert.*
import org.junit.Test

class InviteTest {
    private fun hex(s: String) = ByteArray(s.length / 2) { s.substring(2 * it, 2 * it + 2).toInt(16).toByte() }

    // The datagram the Rust side builds for key 0x09 * 32, session 7, port 47900.
    private val datagram = "4d57011300000007010909090909090909090909090909090909090909090909090909090909090909bb1c"

    @Test fun readsTheSendersKeyAndPort() {
        val d = hex(datagram)
        val (key, port) = Handshake.parseInvite(d, d.size)!!
        assertEquals(47900, port)
        assertArrayEquals(ByteArray(32) { 9 }, key)
    }

    @Test fun rejectsAnythingElse() {
        val d = hex(datagram)
        assertNull(Handshake.parseInvite(d, d.size - 1))
        val hello = d.copyOf().also { it[3] = 0x12 }
        assertNull(Handshake.parseInvite(hello, hello.size))
        val noPort = d.copyOf().also { it[d.size - 1] = 0; it[d.size - 2] = 0 }
        assertNull(Handshake.parseInvite(noPort, noPort.size))
    }

    @Test fun onlyTailnetAddressesCount() {
        fun ip(s: String) = java.net.InetAddress.getByName(s)
        assertTrue(InviteListener.isTailnet(ip("100.72.29.9")))
        assertTrue(InviteListener.isTailnet(ip("100.127.116.12")))
        assertFalse(InviteListener.isTailnet(ip("100.63.0.1")))
        assertFalse(InviteListener.isTailnet(ip("100.128.0.1")))
        assertFalse(InviteListener.isTailnet(ip("192.168.1.5")))
    }
}
