package com.techhouseblr.meshwav

import org.junit.Assert.*
import org.junit.Test

/** Mirrors `is_tailnet_or_loopback` in `crates/audio-transport` (without loopback: a phone has no use for it). */
class PairLinkTest {
    @Test fun tailnetAddressesAreAccepted() {
        for (h in listOf("100.64.0.0", "100.72.29.9", "100.127.255.255", "fd7a:115c:a1e0::1", "fd7a:115c:a1e0:ab12::7", "[fd7a:115c:a1e0::1]", "FD7A:115C:A1E0::1"))
            assertTrue(h, Handshake.tailnetHost(h))
    }

    @Test fun everythingElseIsRefused() {
        for (h in listOf(
            "", "example.com", "evil.example.com", "galaxy", "galaxy.tail0fa12f.ts.net", "localhost",
            "127.0.0.1", "0.0.0.0", "8.8.8.8", "192.168.1.5", "10.0.0.1",
            "100.63.255.255", "100.128.0.0", "100.64.0.1.evil.com", "100.64.0.1@evil.com", "100.64.0.256",
            "1000.64.0.1", "100.64.0", "::1", "fe80::1", "fd7a:115c:a1e1::1", "fd7a:115c:a1e0", "fd7a:115c:a1e0:1:2:3:4:5:6:7",
            "http://100.64.0.1", "100.64.0.1:47900", "100.64.0.1/x",
        )) assertFalse(h, Handshake.tailnetHost(h))
    }
}
