package com.techhouseblr.meshwav

import android.content.Context
import java.net.DatagramPacket
import java.net.DatagramSocket
import java.net.SocketTimeoutException
import java.nio.ByteBuffer
import java.security.SecureRandom

/** Keys of one session. [toAgent] seals our packets, [toReceiver] opens the agent's. */
class SessionKeys(val toAgent: ByteArray, val toReceiver: ByteArray, val agentPublic: ByteArray)

object Handshake {
    const val KIND_XX = 0x10
    const val KIND_XXPSK3 = 0x11
    const val KIND_INVITE = 0x13
    /** Where a waiting phone listens for a sender that has its pairing window open. */
    const val INVITE_PORT = 47902
    private const val FRAME_HEADER = 9
    private val rng = SecureRandom()

    fun frame(kind: Int, sid: Int, step: Int, body: ByteArray): ByteArray {
        val b = ByteBuffer.allocate(FRAME_HEADER + body.size)
        b.put('M'.code.toByte()).put('W'.code.toByte()).put(1).put(kind.toByte()).putInt(sid).put(step.toByte()).put(body)
        return b.array()
    }

    /** Body of a step-2 reply for this session id and kind, or null. */
    fun parseReply(d: ByteArray, len: Int, kind: Int, sid: Int): ByteArray? {
        if (len < FRAME_HEADER || d[0] != 'M'.code.toByte() || d[1] != 'W'.code.toByte() || d[2].toInt() != 1) return null
        if ((d[3].toInt() and 0xff) != kind || (d[8].toInt() and 0xff) != 2) return null
        if (ByteBuffer.wrap(d, 4, 4).int != sid) return null
        return d.copyOfRange(FRAME_HEADER, len)
    }

    /** The sender's public key and port from an invite datagram (see `invite_frame` in audio-protocol), or null. */
    fun parseInvite(d: ByteArray, len: Int): Pair<ByteArray, Int>? {
        if (len != FRAME_HEADER + 34 || d[0] != 'M'.code.toByte() || d[1] != 'W'.code.toByte() || d[2].toInt() != 1) return null
        if ((d[3].toInt() and 0xff) != KIND_INVITE || (d[8].toInt() and 0xff) != 1) return null
        val port = ((d[FRAME_HEADER + 32].toInt() and 0xff) shl 8) or (d[FRAME_HEADER + 33].toInt() and 0xff)
        if (port == 0) return null
        return d.copyOfRange(FRAME_HEADER, FRAME_HEADER + 32) to port
    }

    fun pairingPsk(agentPublic: ByteArray, code: String): ByteArray {
        val md = java.security.MessageDigest.getInstance("SHA-256")
        md.update("meshwav-pair-v1".toByteArray()); md.update(agentPublic); md.update(code.toByteArray())
        return md.digest()
    }

    fun validCode(code: String) = code.length == 6 && code.all { it in '0'..'9' }

    /**
     * Runs the handshake over a connected UDP socket (soTimeout must be set). Retransmits msg1,
     * and sends msg3 a few times since nothing acknowledges it. Blocking: call off the main thread.
     */
    fun run(sock: DatagramSocket, localPriv: ByteArray, agentPublic: ByteArray, code: String?, deviceName: String, stillRunning: () -> Boolean): SessionKeys {
        require(code == null || validCode(code)) { "pairing code must be 6 digits" }
        val psk = code?.let { pairingPsk(agentPublic, it) }
        val kind = if (psk != null) KIND_XXPSK3 else KIND_XX
        val noise = NoiseInitiator(localPriv, psk, agentPublic)
        val sid = rng.nextInt()
        val m1 = frame(kind, sid, 1, noise.writeMsg1())
        val buf = ByteArray(1024)
        val pkt = DatagramPacket(buf, buf.size)
        var reply: ByteArray? = null
        for (attempt in 0 until 6) {
            if (!stillRunning()) throw HandshakeException("stopped")
            sock.send(DatagramPacket(m1, m1.size))
            val until = System.currentTimeMillis() + 500
            while (reply == null && System.currentTimeMillis() < until) {
                try { sock.receive(pkt) } catch (_: SocketTimeoutException) { break }
                reply = parseReply(buf, pkt.length, kind, sid)
            }
            if (reply != null) break
        }
        val r = reply ?: throw HandshakeException(
            if (code != null) "No answer from the agent. Is it running with --pair (code valid 2 minutes)?" else "No answer from the agent"
        )
        noise.readMsg2(r)
        val m3 = frame(kind, sid, 3, noise.writeMsg3(deviceName))
        for (i in 0 until 3) {
            sock.send(DatagramPacket(m3, m3.size))
            if (i < 2) Thread.sleep(20)
        }
        val (a, b) = noise.split()
        return SessionKeys(a, b, noise.remoteStatic)
    }
}

/** Phone-side persistent state: our identity key and the agent we are paired with. */
class PairingStore(context: Context) {
    private val prefs = context.getSharedPreferences("mw_pairing", Context.MODE_PRIVATE)

    private fun hex(b: ByteArray) = b.joinToString("") { "%02x".format(it) }
    private fun unhex(s: String) = ByteArray(s.length / 2) { s.substring(2 * it, 2 * it + 2).toInt(16).toByte() }

    fun identityPrivate(): ByteArray {
        prefs.getString("identity", null)?.let { if (it.length == 64) return unhex(it) }
        val k = ByteArray(32).also { SecureRandom().nextBytes(it) }
        prefs.edit().putString("identity", hex(k)).apply()
        return k
    }

    data class Paired(val host: String, val port: Int, val agentPublic: ByteArray)

    fun paired(): Paired? {
        val host = prefs.getString("host", null) ?: return null
        val pk = prefs.getString("agent_pk", null)?.takeIf { it.length == 64 } ?: return null
        return Paired(host, prefs.getInt("port", 47900), unhex(pk))
    }

    fun savePaired(host: String, port: Int, agentPublic: ByteArray) {
        prefs.edit().putString("host", host).putInt("port", port).putString("agent_pk", hex(agentPublic)).apply()
    }

    fun forget() { prefs.edit().remove("host").remove("port").remove("agent_pk").apply() }
}
