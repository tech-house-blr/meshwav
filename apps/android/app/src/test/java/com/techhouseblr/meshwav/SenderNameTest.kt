package com.techhouseblr.meshwav

import org.junit.Assert.assertEquals
import org.junit.Test

class SenderNameTest {
    @Test fun plainNameIsKept() = assertEquals("liberation", NoiseInitiator.cleanName("liberation".toByteArray()))

    @Test fun controlCharactersAndTheStoreSeparatorAreDropped() =
        assertEquals("ab c", NoiseInitiator.cleanName("a\u0000b\n |c".toByteArray()).replace("  ", " "))

    @Test fun longNamesAreCappedAt32Characters() = assertEquals(32, NoiseInitiator.cleanName("x".repeat(100).toByteArray()).length)

    @Test fun emptyNameFallsBackToSenderNotTheAddress() {
        val p = PairingStore.Paired("100.64.0.9", 47900, ByteArray(32))
        assertEquals("Sender", p.label)
        assertEquals("pc", p.copy(name = "pc").label)
    }
}
