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
    @Volatile var decodeUs = 0.0      // smoothed time to decode one frame (0 for PCM)
    @Volatile var trackQueueMs = -1.0 // smoothed time between handing audio to AudioTrack and it reaching the output position; -1 = unknown
    @Volatile var trackBufferMs = 0   // AudioTrack buffer capacity
    @Volatile var trackUnderruns = 0
    @Volatile var kbps = 0          // 0 = not known yet
    @Volatile var frameMs = 0
    @Volatile var configNote = ""
    @Volatile var lossless = false
    @Volatile var qualityNote = ""   // e.g. why lossless was dropped
    @Volatile var output = ""        // what Android actually granted for playback
    @Volatile var standby = false    // the agent sends audio to another of your receivers

    fun reset() {
        received = 0; lost = 0; late = 0; duplicates = 0; reordered = 0; trimmed = 0
        underruns = 0; resyncs = 0; reconnects = 0; bufferMs = 0; targetMs = 0; jitterMs = 0.0
        decodeUs = 0.0; trackQueueMs = -1.0; trackBufferMs = 0; trackUnderruns = 0
        kbps = 0; frameMs = 0; configNote = ""; lossless = false; qualityNote = ""; output = ""; standby = false
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
        const val FRAME_MS = 10 // initial; the agent announces the real size in every audio packet
        const val AGENT_QUIET_REBIND_MS = 4_000L // no authentic packet at all (the agent sends keepalives every second)
        const val PAIRING_ACK_MS = 8_000L
    }

    private class Live(val keys: SessionKeys) { val punchSeq = AtomicLong(0); val ctlSeq = AtomicLong(0) }

    private val running = AtomicBoolean(false)
    private val lock = Object()
    private var jb = JitterBuffer.forFrameMs(FRAME_MS)
    private val cfg = ConfigClient()
    private val guard = LosslessGuard()
    // Counters of jitter buffers retired by a frame-size switch, so the totals keep growing.
    private val base = LongArray(8)
    @Volatile private var frameChanged = false
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
        sayGoodbye()
        unregisterNetworkCallback()
        socket?.close()
        synchronized(lock) { lock.notifyAll() }
        val ts = synchronized(threads) { threads.toList() }
        ts.forEach { it.interrupt(); it.join(1000) }
        threads.clear()
        live = null
        if (!Stats.state.contains("error", ignoreCase = true)) Stats.state = "Idle"
    }

    /** Tell the agent we are leaving so it restores the PC's audio output now. Sent 3 times (UDP), off the caller's thread. */
    private fun sayGoodbye() {
        val l = live ?: return
        val sock = socket ?: return
        val t = Thread {
            try {
                repeat(3) {
                    val d = Wire.seal(l.keys.toAgent, Wire.DIR_RECEIVER_TO_AGENT, CONTROL_STREAM, l.ctlSeq.getAndIncrement(), Wire.TYPE_CONTROL, 0, ControlWire.encodeBye())
                    sock.send(DatagramPacket(d, d.size))
                    Thread.sleep(20)
                }
            } catch (_: Exception) {}
        }
        t.start()
        t.join(300)
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
                synchronized(cfg) { cfg.reset() }
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

    /** Keepalive punches once a second (150 ms apart at first) plus the config exchange, ticking every 50 ms. */
    private fun punchLoop() {
        var n = 0
        var sendErrors = 0
        var nextPunch = 0L
        while (running.get()) {
            val l = live
            val now = SystemClock.elapsedRealtime()
            if (l != null) {
                try {
                    if (now >= nextPunch) {
                        val d = Wire.seal(l.keys.toAgent, Wire.DIR_RECEIVER_TO_AGENT, STREAM_ID, l.punchSeq.getAndIncrement(), Wire.TYPE_PUNCH, 0, ByteArray(0))
                        socket?.send(DatagramPacket(d, d.size))
                        nextPunch = now + if (++n < 5) 150L else 1000L
                    }
                    if (Stats.lossless && Controls.desiredCodec == ControlWire.CODEC_PCM &&
                        guard.feed(now, Stats.received, Stats.lost + Stats.late + Stats.underruns)) {
                        Controls.desiredCodec = ControlWire.CODEC_OPUS
                        Stats.qualityNote = "Lossless stopped: this connection could not carry it (lost or late packets). Back on Opus."
                    }
                    val req = synchronized(cfg) { cfg.tick(now, Controls.desiredKbps, Controls.desiredFrameMs, Controls.desiredCodec) }
                    if (req != null) {
                        val d = Wire.seal(l.keys.toAgent, Wire.DIR_RECEIVER_TO_AGENT, CONTROL_STREAM, l.ctlSeq.getAndIncrement(), Wire.TYPE_CONTROL, 0, req)
                        socket?.send(DatagramPacket(d, d.size))
                    }
                    synchronized(cfg) { Stats.kbps = cfg.appliedKbps; Stats.configNote = cfg.note ?: "" }
                    sendErrors = 0
                } catch (_: Exception) {
                    if (++sendErrors >= 3) { rebind("send failed"); sendErrors = 0 }
                }
            }
            try { Thread.sleep(50) } catch (_: InterruptedException) { return }
        }
    }

    private fun recvLoop() {
        val buf = ByteArray(2048)
        val pkt = DatagramPacket(buf, buf.size)
        var lastAudio = SystemClock.elapsedRealtime()
        var errors = 0
        var candidateMs = 0
        var candidateCount = 0
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
                    else if (now - lastAudio > 3000 && !Stats.standby) Stats.state = "No audio from $host (is anything playing?)"
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
            if (p.type == Wire.TYPE_CONTROL) {
                ControlWire.decode(p.payload)?.let { m ->
                    if (m is ControlWire.Msg.Active) {
                        if (!m.active) {
                            Stats.standby = true
                            Stats.state = "Standby: the agent is sending audio to another device"
                        } else if (Stats.standby) {
                            Stats.standby = false
                            Stats.state = "Connected"
                        }
                    } else synchronized(cfg) { cfg.onMessage(m) }
                }
                continue
            }
            if (p.type != Wire.TYPE_AUDIO || p.fragCnt != 1) continue
            if (!ControlWire.audioPlausible(p.flags, p.payload.size)) continue
            val pcmPacket = ControlWire.isPcm(p.flags)
            lastAudio = now
            val ms = ControlWire.frameMsFromFlags(p.flags)
            synchronized(lock) {
                // A switch needs 3 packets in a row announcing the new size, so a straggler of the old size
                // that arrives late (reordering across the switch) cannot make the buffer flap back and forth.
                if (ms != 0 && ms != jb.frameMs) {
                    if (ms == candidateMs) candidateCount++ else { candidateMs = ms; candidateCount = 1 }
                    if (candidateCount >= 3) { switchFrameSize(ms); candidateMs = 0; candidateCount = 0 }
                } else candidateCount = 0
                jb.push(p.seq, p.payload, now)
                lock.notifyAll()
            }
            synchronized(cfg) {
                // While lossless the packets are 5 ms whatever Opus size was asked for, so they say nothing about it.
                if (ms != 0 && !pcmPacket) cfg.frameSeen(ms)
                if (ms != 0) Stats.frameMs = ms
                if (pcmPacket != Stats.lossless) { Stats.lossless = pcmPacket; guard.reset() }
                cfg.codecSeen(pcmPacket)
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

    /** What Android actually opened, as reported by the AudioTrack. It cannot show whether the system mixer resamples. */
    private fun describeOutput(t: AudioTrack): String {
        val enc = when (t.audioFormat) { AudioFormat.ENCODING_PCM_16BIT -> "16-bit"; AudioFormat.ENCODING_PCM_FLOAT -> "float"; else -> "format ${t.audioFormat}" }
        val dev = try {
            when (val ty = t.routedDevice?.type) {
                null -> "route unknown"
                1 -> "phone earpiece"; 2 -> "phone speaker"; 3, 4 -> "wired headphones"
                7 -> "Bluetooth call"; 8 -> "Bluetooth A2DP"; 11, 12, 22 -> "USB audio"; 26 -> "Bluetooth LE"
                else -> "output type $ty"
            }
        } catch (_: Exception) { "route unknown" }
        val mode = if (android.os.Build.VERSION.SDK_INT >= 29) when (t.performanceMode) {
            AudioTrack.PERFORMANCE_MODE_LOW_LATENCY -> "low-latency granted"
            AudioTrack.PERFORMANCE_MODE_POWER_SAVING -> "power-saving"
            else -> "low-latency NOT granted"
        } else "mode unknown"
        return "${t.sampleRate} Hz, $enc, $dev, $mode"
    }

    /** The agent changed its frame size: start a fresh buffer sized for it, keeping the running totals. Caller holds [lock]. */
    private fun switchFrameSize(ms: Int) {
        val o = jb
        base[0] += o.received; base[1] += o.lost; base[2] += o.late; base[3] += o.duplicates
        base[4] += o.reordered; base[5] += o.trimmed; base[6] += o.underruns; base[7] += o.resyncs
        jb = JitterBuffer.forFrameMs(ms)
        frameChanged = true
    }

    private fun publishStats() {
        Stats.received = base[0] + jb.received; Stats.lost = base[1] + jb.lost; Stats.late = base[2] + jb.late
        Stats.duplicates = base[3] + jb.duplicates; Stats.reordered = base[4] + jb.reordered; Stats.trimmed = base[5] + jb.trimmed
        Stats.underruns = base[6] + jb.underruns; Stats.resyncs = base[7] + jb.resyncs
        Stats.bufferMs = jb.depthFrames * jb.frameMs; Stats.targetMs = jb.targetFrames * jb.frameMs
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
        val ts = android.media.AudioTimestamp()
        var written = 0L
        var n = 0
        try {
            Stats.trackBufferMs = track.bufferSizeInFrames * 1000 / 48000
            while (running.get()) {
                var out: JitterBuffer.Out
                var frameMs: Int
                synchronized(lock) {
                    if (frameChanged) { frameChanged = false; concealer.reset() }
                    out = jb.pop(SystemClock.elapsedRealtime())
                    frameMs = jb.frameMs
                    if (out.kind == JitterBuffer.Kind.WAIT) lock.wait(3)
                    publishStats()
                }
                val pcm = when (out.kind) {
                    JitterBuffer.Kind.FRAME -> concealer.accept(out.payload!!.let {
                        if (it.size == ControlWire.PCM_FRAME_BYTES) { Stats.decodeUs = 0.0; it } else {
                            val t0 = System.nanoTime()
                            val d = decoder.decode(it, frameMs * 1000L)
                            Stats.decodeUs += ((System.nanoTime() - t0) / 1000.0 - Stats.decodeUs) * 0.05
                            d
                        }
                    })
                    JitterBuffer.Kind.LOST -> concealer.conceal(48 * frameMs * 4)
                    JitterBuffer.Kind.WAIT -> continue
                }
                if (pcm.isNotEmpty()) {
                    track.write(pcm, 0, pcm.size)
                    written += pcm.size / 4
                    if (++n % 20 == 0) {
                        // Frames handed over minus the position the output has reached, corrected for the age of that position.
                        if (track.getTimestamp(ts)) {
                            val q = (written - ts.framePosition) * 1000.0 / 48000 - (System.nanoTime() - ts.nanoTime) / 1e6
                            Stats.trackQueueMs = if (Stats.trackQueueMs < 0) q else Stats.trackQueueMs + (q - Stats.trackQueueMs) * 0.1
                        }
                        Stats.trackUnderruns = track.underrunCount
                    }
                    if (Stats.state != "Playing") Stats.state = "Playing"
                    if (Stats.output.isEmpty()) Stats.output = describeOutput(track)
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
