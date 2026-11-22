package com.techhouseblr.meshwav

import android.content.Context
import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioTrack
import android.net.ConnectivityManager
import android.net.Network
import android.os.SystemClock
import java.net.DatagramPacket
import java.net.DatagramSocket
import java.net.InetSocketAddress
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong

/** Live counters shown in the UI. */
object Stats {
    @Volatile var state = "Idle"
    @Volatile var agentKey = ""
    @Volatile var received = 0L
    @Volatile var lost = 0L
    @Volatile var late = 0L
    @Volatile var duplicates = 0L
    @Volatile var reordered = 0L
    @Volatile var trimmed = 0L
    @Volatile var underruns = 0L
    @Volatile var resyncs = 0L
    @Volatile var reconnects = 0L
    @Volatile var bufferMs = 0
    @Volatile var targetMs = 0
    @Volatile var jitterMs = 0.0

    fun reset() {
        received = 0; lost = 0; late = 0; duplicates = 0; reordered = 0; trimmed = 0
        underruns = 0; resyncs = 0; reconnects = 0; bufferMs = 0; targetMs = 0; jitterMs = 0.0
    }
}

/**
 * UDP receive -> adaptive jitter buffer -> Opus decode (+ concealment of lost frames) -> AudioTrack.
 * Every connection starts with a Noise handshake (pairing with a code the first time, pinned
 * agent key afterwards) that yields fresh per-session keys. The socket and the session are
 * rebuilt automatically after a network change, send failures or when the agent goes quiet.
 */
class StreamReceiver(
    private val host: String,
    private val port: Int,
    private val identity: ByteArray,
    private val agentPublic: ByteArray,
    pairingCode: String?,
    private val deviceName: String,
    private val context: Context? = null,
    private val onPaired: (() -> Unit)? = null,
) {
    companion object {
        const val STREAM_ID = 1L
        const val CONTROL_STREAM = 2L // agent keepalives; separate seq space so audio seq stays gap-free
        const val FRAME_MS = 10
        const val AGENT_QUIET_REBIND_MS = 4_000L // no authentic packet at all (the agent sends keepalives every second)
        const val PAIRING_ACK_MS = 8_000L
    }

    private class Live(val keys: SessionKeys) { val punchSeq = AtomicLong(0) }

    private val running = AtomicBoolean(false)
    private val lock = Object()
    private val jb = JitterBuffer(frameMs = FRAME_MS)
    private val threads = java.util.Collections.synchronizedList(mutableListOf<Thread>())
    private val sockLock = Any()
    @Volatile private var socket: DatagramSocket? = null
    @Volatile private var live: Live? = null
    @Volatile private var rebindReason: String? = null
    @Volatile private var pairingCode: String? = pairingCode
    @Volatile private var awaitingAck = pairingCode != null
    @Volatile private var lastAlive = 0L
    @Volatile private var ackDeadline = 0L
    private var netCb: ConnectivityManager.NetworkCallback? = null

    fun start() {
        if (!running.compareAndSet(false, true)) return
        Stats.reset(); Stats.state = "Connecting to $host:$port"
        // Socket setup and the handshake must not run on the main thread (StrictMode network policy).
        threads += Thread {
            if (!openSocket()) { running.set(false); return@Thread }
            registerNetworkCallback()
            if (!establish()) { running.set(false); return@Thread }
            threads += Thread(::recvLoop, "mw-recv").also { it.start() }
            threads += Thread(::punchLoop, "mw-punch").also { it.start() }
            threads += Thread(::playLoop, "mw-play").also { it.priority = Thread.MAX_PRIORITY; it.start() }
        }.also { it.start() }
    }

    fun stop() {
        if (!running.compareAndSet(true, false)) return
        unregisterNetworkCallback()
        socket?.close()
        synchronized(lock) { lock.notifyAll() }
        val ts = synchronized(threads) { threads.toList() }
        ts.forEach { it.interrupt(); it.join(1000) }
        threads.clear()
        live = null
        if (!Stats.state.contains("error", ignoreCase = true)) Stats.state = "Idle"
    }

    /** Ask the receive thread to rebuild the socket and session (safe from any thread). */
    fun rebind(reason: String) { rebindReason = reason }

    private fun fail(message: String) {
        Stats.state = "Pairing error: $message"
        running.set(false)
        unregisterNetworkCallback()
        socket?.close()
        synchronized(lock) { lock.notifyAll() }
    }

    private fun openSocket(): Boolean = synchronized(sockLock) {
        try {
            socket?.close()
            val s = DatagramSocket()
            s.connect(InetSocketAddress(host, port))
            s.soTimeout = 500
            socket = s
            true
        } catch (e: Throwable) {
            Stats.state = "Socket error: ${e.javaClass.simpleName}: ${e.message}"
            false
        }
    }

    /**
     * Handshake until a session exists. A pairing code is used exactly once (retrying a wrong code
     * would only burn the agent's attempt budget); with a pinned key we keep trying until the agent is up.
     * Returns false when stopped or on a fatal error.
     */
    private fun establish(): Boolean {
        while (running.get()) {
            val s = socket ?: return false
            val code = pairingCode
            Stats.state = if (code != null) "Pairing with $host…" else "Connecting to $host:$port"
            try {
                val keys = Handshake.run(s, identity, agentPublic, code, deviceName) { running.get() }
                pairingCode = null // consumed, whatever happens next
                live = Live(keys)
                lastAlive = SystemClock.elapsedRealtime()
                ackDeadline = lastAlive + PAIRING_ACK_MS
                Stats.agentKey = agentPublic.joinToString("") { "%02x".format(it) }.take(16)
                Stats.state = if (awaitingAck) "Pairing…" else "Connected, waiting for audio"
                return true
            } catch (e: HandshakeException) {
                if (!running.get()) return false
                if (e.fatal || code != null) { pairingCode = null; fail(e.message ?: "handshake failed"); return false }
                Stats.state = "Waiting for the agent (${e.message})"
            } catch (e: java.io.IOException) {
                if (!running.get()) return false
                Stats.state = "Waiting for the agent (${e.javaClass.simpleName})"
            } catch (e: Exception) {
                fail("${e.javaClass.simpleName}: ${e.message}"); return false
            }
            try { Thread.sleep(1500) } catch (_: InterruptedException) { return false }
            if (!openSocket()) try { Thread.sleep(1000) } catch (_: InterruptedException) { return false }
        }
        return false
    }

    private fun registerNetworkCallback() {
        val cm = context?.getSystemService(ConnectivityManager::class.java) ?: return
        var first = true // the callback fires once immediately on registration
        val cb = object : ConnectivityManager.NetworkCallback() {
            override fun onAvailable(network: Network) {
                if (first) { first = false; return }
                rebind("network changed")
            }
            override fun onLost(network: Network) { rebind("network lost") }
        }
        try { cm.registerDefaultNetworkCallback(cb); netCb = cb } catch (_: Exception) {}
    }

    private fun unregisterNetworkCallback() {
        val cb = netCb ?: return
        netCb = null
        try { context?.getSystemService(ConnectivityManager::class.java)?.unregisterNetworkCallback(cb) } catch (_: Exception) {}
    }

    private fun punchLoop() {
        var n = 0
        var sendErrors = 0
        while (running.get()) {
            val l = live
            if (l != null) {
                try {
                    val d = Wire.seal(l.keys.toAgent, Wire.DIR_RECEIVER_TO_AGENT, STREAM_ID, l.punchSeq.getAndIncrement(), Wire.TYPE_PUNCH, 0, ByteArray(0))
                    socket?.send(DatagramPacket(d, d.size))
                    sendErrors = 0
                } catch (_: Exception) {
                    if (++sendErrors >= 3) { rebind("send failed"); sendErrors = 0 }
                }
            }
            n++
            try { Thread.sleep(if (n < 5) 150L else 1000L) } catch (_: InterruptedException) { return }
        }
    }

    private fun recvLoop() {
        val buf = ByteArray(2048)
        val pkt = DatagramPacket(buf, buf.size)
        var lastAudio = SystemClock.elapsedRealtime()
        var errors = 0
        while (running.get()) {
            val why = rebindReason
            if (why != null) {
                rebindReason = null
                Stats.reconnects++
                Stats.state = "Reconnecting ($why)"
                live = null
                if (!openSocket()) {
                    rebindReason = why // network still down: retry shortly
                    try { Thread.sleep(1000) } catch (_: InterruptedException) { return }
                    continue
                }
                if (!establish()) { running.set(false); return }
                lastAudio = SystemClock.elapsedRealtime()
            }
            try {
                socket?.receive(pkt)
                errors = 0
            } catch (_: java.net.SocketTimeoutException) {
                val now = SystemClock.elapsedRealtime()
                if (awaitingAck && now > ackDeadline) {
                    pairingCode = null
                    fail("no answer after pairing. The code was wrong or expired, or the agent is not in pairing mode.")
                    return
                }
                if (!awaitingAck) {
                    if (now - lastAlive > AGENT_QUIET_REBIND_MS) { rebind("agent not responding"); lastAlive = now }
                    else if (now - lastAudio > 3000) Stats.state = "No audio from $host (is anything playing?)"
                }
                continue
            } catch (_: Exception) {
                if (!running.get()) return
                if (++errors >= 5) { rebind("socket error"); errors = 0 }
                try { Thread.sleep(100) } catch (_: InterruptedException) { return }
                continue
            }
            val l = live ?: continue
            val p = Wire.open(l.keys.toReceiver, Wire.DIR_AGENT_TO_RECEIVER, STREAM_ID, buf, pkt.length)
                ?: Wire.open(l.keys.toReceiver, Wire.DIR_AGENT_TO_RECEIVER, CONTROL_STREAM, buf, pkt.length)
                ?: continue
            val now = SystemClock.elapsedRealtime()
            lastAlive = now
            if (awaitingAck) {
                awaitingAck = false
                Stats.state = "Paired"
                onPaired?.invoke()
            }
            if (p.type != Wire.TYPE_AUDIO || p.fragCnt != 1) continue
            lastAudio = now
            synchronized(lock) {
                jb.push(p.seq, p.payload, now)
                lock.notifyAll()
            }
        }
    }

    private fun newTrack(): AudioTrack {
        val min = AudioTrack.getMinBufferSize(48000, AudioFormat.CHANNEL_OUT_STEREO, AudioFormat.ENCODING_PCM_16BIT)
        val track = AudioTrack.Builder()
            .setAudioAttributes(AudioAttributes.Builder().setUsage(AudioAttributes.USAGE_MEDIA).setContentType(AudioAttributes.CONTENT_TYPE_MUSIC).build())
            .setAudioFormat(AudioFormat.Builder().setSampleRate(48000).setEncoding(AudioFormat.ENCODING_PCM_16BIT).setChannelMask(AudioFormat.CHANNEL_OUT_STEREO).build())
            .setTransferMode(AudioTrack.MODE_STREAM)
            .setPerformanceMode(AudioTrack.PERFORMANCE_MODE_LOW_LATENCY)
            .setBufferSizeInBytes(maxOf(min, 1920 * 4))
            .build()
        track.play()
        return track
    }

    private fun publishStats() {
        Stats.received = jb.received; Stats.lost = jb.lost; Stats.late = jb.late
        Stats.duplicates = jb.duplicates; Stats.reordered = jb.reordered; Stats.trimmed = jb.trimmed
        Stats.underruns = jb.underruns; Stats.resyncs = jb.resyncs
        Stats.bufferMs = jb.depthFrames * FRAME_MS; Stats.targetMs = jb.targetFrames * FRAME_MS
        Stats.jitterMs = jb.jitterMs
    }

    private fun playLoop() {
        val decoder: OpusDecoder
        val track: AudioTrack
        try {
            decoder = OpusDecoder()
            track = newTrack()
        } catch (e: Throwable) {
            Stats.state = "Audio setup error: ${e.javaClass.simpleName}: ${e.message}"
            return
        }
        val concealer = Concealer()
        val frameBytes = 48 * FRAME_MS * 4
        try {
            while (running.get()) {
                var out: JitterBuffer.Out
                synchronized(lock) {
                    out = jb.pop(SystemClock.elapsedRealtime())
                    if (out.kind == JitterBuffer.Kind.WAIT) lock.wait(3)
                    publishStats()
                }
                val pcm = when (out.kind) {
                    JitterBuffer.Kind.FRAME -> concealer.accept(decoder.decode(out.payload!!, FRAME_MS * 1000L))
                    JitterBuffer.Kind.LOST -> concealer.conceal(frameBytes)
                    JitterBuffer.Kind.WAIT -> continue
                }
                if (pcm.isNotEmpty()) {
                    track.write(pcm, 0, pcm.size)
                    if (Stats.state != "Playing") Stats.state = "Playing"
                }
            }
        } catch (_: InterruptedException) {
        } catch (e: Throwable) {
            Stats.state = "Playback error: ${e.javaClass.simpleName}: ${e.message}"
        } finally {
            try { track.stop() } catch (_: Exception) {}
            track.release()
            decoder.release()
        }
    }
}
