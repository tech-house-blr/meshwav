package com.techhouseblr.meshwav

import java.math.BigInteger
import java.security.MessageDigest
import java.security.SecureRandom
import javax.crypto.Cipher
import javax.crypto.Mac
import javax.crypto.spec.IvParameterSpec
import javax.crypto.spec.SecretKeySpec

/**
 * X25519 (RFC 7748) on BigInteger. Android only ships X25519 from API 33 and minSdk is 28.
 * NOT constant-time: acceptable for a handset-local key, but do not reuse this elsewhere.
 */
object X25519 {
    private val P: BigInteger = BigInteger.ONE.shiftLeft(255).subtract(BigInteger.valueOf(19))
    private val A24: BigInteger = BigInteger.valueOf(121665)
    private val BASE = ByteArray(32).also { it[0] = 9 }

    private fun decode(b: ByteArray, maskTop: Boolean): BigInteger {
        val r = b.reversedArray()
        if (maskTop) r[0] = (r[0].toInt() and 0x7f).toByte()
        return BigInteger(1, r)
    }

    private fun encode(v: BigInteger): ByteArray {
        val be = v.toByteArray()
        val out = ByteArray(32)
        for (j in 0 until minOf(32, be.size)) out[j] = be[be.size - 1 - j]
        return out
    }

    fun scalarMult(k: ByteArray, u: ByteArray): ByteArray {
        require(k.size == 32 && u.size == 32) { "X25519 inputs must be 32 bytes" }
        val kc = k.copyOf()
        kc[0] = (kc[0].toInt() and 248).toByte()
        kc[31] = ((kc[31].toInt() and 127) or 64).toByte()
        val kn = decode(kc, false)
        val x1 = decode(u, true).mod(P)
        var x2 = BigInteger.ONE
        var z2 = BigInteger.ZERO
        var x3 = x1
        var z3 = BigInteger.ONE
        var swap = 0
        for (t in 254 downTo 0) {
            val kt = if (kn.testBit(t)) 1 else 0
            swap = swap xor kt
            if (swap == 1) { val a = x2; x2 = x3; x3 = a; val b = z2; z2 = z3; z3 = b }
            swap = kt
            val a = x2.add(z2).mod(P)
            val aa = a.multiply(a).mod(P)
            val b = x2.subtract(z2).mod(P)
            val bb = b.multiply(b).mod(P)
            val e = aa.subtract(bb).mod(P)
            val c = x3.add(z3).mod(P)
            val d = x3.subtract(z3).mod(P)
            val da = d.multiply(a).mod(P)
            val cb = c.multiply(b).mod(P)
            val s = da.add(cb).mod(P)
            val df = da.subtract(cb).mod(P)
            x3 = s.multiply(s).mod(P)
            z3 = x1.multiply(df.multiply(df).mod(P)).mod(P)
            x2 = aa.multiply(bb).mod(P)
            z2 = e.multiply(aa.add(A24.multiply(e)).mod(P)).mod(P)
        }
        if (swap == 1) { val a = x2; x2 = x3; x3 = a; val b = z2; z2 = z3; z3 = b }
        return encode(x2.multiply(z2.modPow(P.subtract(BigInteger.TWO), P)).mod(P))
    }

    fun publicKey(priv: ByteArray): ByteArray = scalarMult(priv, BASE)

    /** Diffie-Hellman; rejects the all-zero result produced by low-order points. */
    fun dh(priv: ByteArray, pub: ByteArray): ByteArray {
        val out = scalarMult(priv, pub)
        require(!out.all { it.toInt() == 0 }) { "low-order public key" }
        return out
    }
}

class HandshakeException(message: String, val fatal: Boolean = false) : Exception(message)

/**
 * Noise initiator for Noise_XX_25519_ChaChaPoly_SHA256, or Noise_XXpsk3_... when [psk] is given.
 * Must stay byte-compatible with crates/audio-protocol/src/handshake.rs (vectors in
 * src/test/resources/noise_v1.txt). The phone is always the initiator.
 */
class NoiseInitiator(
    private val localPriv: ByteArray,
    private val psk: ByteArray?,
    private val expectedRemote: ByteArray?,
    private val fixedEphemeral: ByteArray? = null, // tests only
) {
    companion object {
        val PROLOGUE = "meshwav-v1".toByteArray()
        const val NAME_XX = "Noise_XX_25519_ChaChaPoly_SHA256"
        const val NAME_XXPSK3 = "Noise_XXpsk3_25519_ChaChaPoly_SHA256"
        const val MAX_NAME = 32

        /** A label only, never an identity: control characters and anything past 32 characters are dropped. */
        fun cleanName(raw: ByteArray): String = String(raw, Charsets.UTF_8).filter { !it.isISOControl() && it != '|' }.take(MAX_NAME).trim()
        private val rng = SecureRandom()
    }

    private val localPub = X25519.publicKey(localPriv)
    private var h: ByteArray
    private var ck: ByteArray
    private var k: ByteArray? = null
    private var n = 0L
    private var ePriv = ByteArray(0)
    private var re = ByteArray(0)
    var remoteStatic = ByteArray(0); private set
    /** The name the agent reported in message 2 (this computer's host name). Empty from an older agent. */
    var remoteName = ""; private set

    init {
        val nb = (if (psk != null) NAME_XXPSK3 else NAME_XX).toByteArray()
        h = if (nb.size <= 32) nb.copyOf(32) else sha256(nb)
        ck = h
        mixHash(PROLOGUE)
    }

    private fun sha256(vararg parts: ByteArray): ByteArray {
        val md = MessageDigest.getInstance("SHA-256")
        parts.forEach { md.update(it) }
        return md.digest()
    }

    private fun hmac(key: ByteArray, vararg parts: ByteArray): ByteArray {
        val m = Mac.getInstance("HmacSHA256")
        m.init(SecretKeySpec(key, "HmacSHA256"))
        parts.forEach { m.update(it) }
        return m.doFinal()
    }

    private fun hkdf(chain: ByteArray, ikm: ByteArray, outputs: Int): List<ByteArray> {
        val temp = hmac(chain, ikm)
        val o1 = hmac(temp, byteArrayOf(1))
        val o2 = hmac(temp, o1, byteArrayOf(2))
        return if (outputs == 2) listOf(o1, o2) else listOf(o1, o2, hmac(temp, o2, byteArrayOf(3)))
    }

    private fun mixHash(d: ByteArray) { h = sha256(h, d) }

    private fun mixKey(ikm: ByteArray) {
        val (a, b) = hkdf(ck, ikm, 2)
        ck = a; k = b; n = 0
    }

    private fun mixKeyAndHash(ikm: ByteArray) {
        val (a, tempH, tempK) = hkdf(ck, ikm, 3)
        ck = a; mixHash(tempH); k = tempK; n = 0
    }

    private fun aead(mode: Int, data: ByteArray): ByteArray {
        val nonce = ByteArray(12)
        for (i in 0 until 8) nonce[4 + i] = (n ushr (8 * i)).toByte() // little-endian counter
        val c = Cipher.getInstance("ChaCha20-Poly1305")
        c.init(mode, SecretKeySpec(k, "ChaCha20"), IvParameterSpec(nonce))
        c.updateAAD(h)
        val out = c.doFinal(data)
        n++
        return out
    }

    private fun encryptAndHash(pt: ByteArray): ByteArray {
        if (k == null) { mixHash(pt); return pt }
        val ct = aead(Cipher.ENCRYPT_MODE, pt)
        mixHash(ct)
        return ct
    }

    private fun decryptAndHash(ct: ByteArray): ByteArray {
        if (k == null) { mixHash(ct); return ct }
        val pt = try { aead(Cipher.DECRYPT_MODE, ct) } catch (e: Exception) { throw HandshakeException("handshake message failed authentication") }
        mixHash(ct)
        return pt
    }

    /** -> e */
    fun writeMsg1(): ByteArray {
        ePriv = fixedEphemeral ?: ByteArray(32).also { rng.nextBytes(it) }
        val ePub = X25519.publicKey(ePriv)
        mixHash(ePub)
        if (psk != null) mixKey(ePub)
        return ePub + encryptAndHash(ByteArray(0))
    }

    /** <- e, ee, s, es. Returns the agent's static key; throws fatally if it is not the pinned one. */
    fun readMsg2(msg: ByteArray): ByteArray {
        if (msg.size < 32 + 48 + 16) throw HandshakeException("handshake reply too short")
        re = msg.copyOfRange(0, 32)
        mixHash(re)
        if (psk != null) mixKey(re)
        try { mixKey(X25519.dh(ePriv, re)) } catch (e: IllegalArgumentException) { throw HandshakeException("bad agent key", fatal = true) }
        val rs = decryptAndHash(msg.copyOfRange(32, 80))
        try { mixKey(X25519.dh(ePriv, rs)) } catch (e: IllegalArgumentException) { throw HandshakeException("bad agent key", fatal = true) }
        remoteName = cleanName(decryptAndHash(msg.copyOfRange(80, msg.size)))
        remoteStatic = rs
        if (expectedRemote != null && !MessageDigest.isEqual(expectedRemote, rs)) {
            throw HandshakeException("The agent's key is not the one this phone paired with. Pair again if you reinstalled the agent.", fatal = true)
        }
        return rs
    }

    /** -> s, se, [psk], payload(device name) */
    fun writeMsg3(deviceName: String): ByteArray {
        val name = deviceName.filter { !it.isISOControl() }.toByteArray().let { if (it.size > MAX_NAME) it.copyOf(MAX_NAME) else it }
        val s = encryptAndHash(localPub)
        mixKey(X25519.dh(localPriv, re))
        if (psk != null) mixKeyAndHash(psk)
        return s + encryptAndHash(name)
    }

    /** Raw split: (receiver->agent key, agent->receiver key). */
    fun split(): Pair<ByteArray, ByteArray> {
        val (a, b) = hkdf(ck, ByteArray(0), 2)
        return a to b
    }
}
