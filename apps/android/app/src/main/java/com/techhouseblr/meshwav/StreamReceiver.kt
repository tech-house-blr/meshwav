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

/** Live counters shown in the UI. */
object Stats {
    @Volatile var state = "Idle"
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
 * Frames are 10 ms (fixed for now; negotiated over the control channel later). The socket is
 * rebuilt automatically after a network change, send failures or prolonged silence.
 */
class StreamReceiver(
    private val host: String,
    private val port: Int,
    private val key: ByteArray,
    private val context: Context? = null,
) {
    companion object {
        const val STREAM_ID = 1L
        const val FRAME_MS = 10
        const val SILENCE_REBIND_MS = 10_000L
    }

    private val running = AtomicBoolean(false)
    private val lock = Object()
    private val jb = JitterBuffer(frameMs = FRAME_MS)
    private val threads = java.util.Collections.synchronizedList(mutableListOf<Thread>())
    private val sockLock = Any()
    @Volatile private var socket: DatagramSocket? = null
    @Volatile private var rebindReason: String? = null
    private var netCb: ConnectivityManager.NetworkCallback? = null

    fun start() {
        if (!running.compareAndSet(false, true)) return
        Stats.reset(); Stats.state = "Connecting to $host:$port"
        // Socket setup must not run on the main thread (StrictMode network policy), so do it here.
        threads += Thread {
            if (!openSocket()) { running.set(false); return@Thread }
            registerNetworkCallback()
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
        if (!Stats.state.contains("error", ignoreCase = true)) Stats.state = "Idle"
    }

    /** Ask the receive thread to rebuild the socket (safe from any thread). */
    fun rebind(reason: String) { rebindReason = reason }

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
        // Seq must increase across app restarts (the agent keeps a replay window), so start from time.
        var seq = (System.currentTimeMillis() / 100) and 0xFFFFFFFFL
        var n = 0
        var sendErrors = 0
        while (running.get()) {
            try {
                val d = Wire.seal(key, Wire.DIR_RECEIVER_TO_AGENT, STREAM_ID, seq, Wire.TYPE_PUNCH, 0, ByteArray(0))
                socket?.send(DatagramPacket(d, d.size))
                seq++
                sendErrors = 0
            } catch (_: Exception) {
                if (++sendErrors >= 3) { rebind("send failed"); sendErrors = 0 }
            }
            n++
            try { Thread.sleep(if (n < 5) 150L else 1000L) } catch (_: InterruptedException) { return }
        }
    }

    private fun recvLoop() {
        val buf = ByteArray(2048)
        val pkt = DatagramPacket(buf, buf.size)
        var lastAudio = 0L
        var errors = 0
        while (running.get()) {
            val why = rebindReason
            if (why != null) {
                rebindReason = null
                Stats.reconnects++
                Stats.state = "Reconnecting ($why)"
                if (!openSocket()) {
                    rebindReason = why // network still down: retry shortly
                    try { Thread.sleep(1000) } catch (_: InterruptedException) { return }
                    continue
                }
                lastAudio = SystemClock.elapsedRealtime()
            }
            try {
                socket?.receive(pkt)
                errors = 0
            } catch (_: java.net.SocketTimeoutException) {
                if (lastAudio != 0L) {
                    val quiet = SystemClock.elapsedRealtime() - lastAudio
                    if (quiet > 3000) Stats.state = "No audio from $host (is anything playing?)"
                    if (quiet > SILENCE_REBIND_MS) { rebind("silence"); lastAudio = SystemClock.elapsedRealtime() }
                }
                continue
            } catch (_: Exception) {
                if (!running.get()) return
                if (++errors >= 5) { rebind("socket error"); errors = 0 }
                try { Thread.sleep(100) } catch (_: InterruptedException) { return }
                continue
            }
            val p = Wire.open(key, Wire.DIR_AGENT_TO_RECEIVER, STREAM_ID, buf, pkt.length) ?: continue
            if (p.type != Wire.TYPE_AUDIO || p.fragCnt != 1) continue
            lastAudio = SystemClock.elapsedRealtime()
            synchronized(lock) {
                jb.push(p.seq, p.payload, lastAudio)
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
