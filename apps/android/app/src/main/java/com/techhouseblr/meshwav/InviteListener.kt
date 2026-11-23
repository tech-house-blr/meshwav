package com.techhouseblr.meshwav

import java.net.DatagramPacket
import java.net.DatagramSocket
import java.net.Inet4Address
import java.net.InetAddress
import java.net.NetworkInterface
import java.net.SocketTimeoutException

/**
 * Finds the sender that is showing a pairing code, so the user types only the code. While the user
 * waits here, a sender with an open pairing window sends its key and port to this phone (see
 * `desktop-agent/src/invite.rs`). Listens only on the Tailscale address and accepts only tailnet
 * sources. The invite is unauthenticated; the pairing code and the key pinned by the handshake
 * protect the pairing itself.
 */
class InviteListener {
    class Sender(val host: String, val port: Int, val publicKey: ByteArray)

    @Volatile private var sock: DatagramSocket? = null
    @Volatile private var cancelled = false

    fun cancel() { cancelled = true; sock?.close() }

    /** Blocks until a sender shows up, [timeoutMs] passes or [cancel] is called. Call off the main thread. */
    fun wait(timeoutMs: Long): Result<Sender?> {
        val mine = tailnetAddress() ?: return Result.failure(IllegalStateException("Tailscale does not seem to be connected on this phone"))
        val s = try {
            DatagramSocket(null).apply { reuseAddress = true; bind(java.net.InetSocketAddress(mine, Handshake.INVITE_PORT)); soTimeout = 500 }
        } catch (e: Exception) {
            return Result.failure(IllegalStateException("Cannot listen for a sender: ${e.message}"))
        }
        sock = s
        val buf = ByteArray(256)
        val pkt = DatagramPacket(buf, buf.size)
        val end = System.currentTimeMillis() + timeoutMs
        try {
            while (!cancelled && System.currentTimeMillis() < end) {
                try { s.receive(pkt) } catch (_: SocketTimeoutException) { continue }
                val from = pkt.address
                if (!isTailnet(from)) continue
                val (key, port) = Handshake.parseInvite(buf, pkt.length) ?: continue
                return Result.success(Sender(from.hostAddress ?: continue, port, key))
            }
        } catch (_: java.net.SocketException) {
            // closed by cancel()
        } finally {
            s.close()
        }
        return Result.success(null)
    }

    companion object {
        /** 100.64.0.0/10, the tailnet range. */
        fun isTailnet(a: InetAddress): Boolean {
            if (a !is Inet4Address) return false
            val b = a.address
            return (b[0].toInt() and 0xff) == 100 && (b[1].toInt() and 0xc0) == 64
        }

        fun tailnetAddress(): InetAddress? = try {
            NetworkInterface.getNetworkInterfaces().toList().flatMap { it.inetAddresses.toList() }.firstOrNull { isTailnet(it) }
        } catch (_: Exception) { null }
    }
}
