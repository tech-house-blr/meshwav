package com.techhouseblr.meshwav

import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioTrack
import java.net.DatagramPacket
import java.net.DatagramSocket
import java.net.InetSocketAddress
import java.util.TreeMap
import java.util.concurrent.atomic.AtomicBoolean

/** Live counters shown in the UI. */
object Stats {
    @Volatile var state = "Idle"
    @Volatile var received = 0L
    @Volatile var lost = 0L
    @Volatile var bufferMs = 0
}

/**
 * UDP receive -> jitter buffer -> Opus decode -> AudioTrack.
 * Frames are 10 ms (fixed for now; negotiated over the control channel later). The buffer is bounded
 * and skips ahead when it grows, so latency stays bounded.
 */
class StreamReceiver(private val host: String, private val port: Int, private val key: ByteArray) {
    companion object {
        const val STREAM_ID = 1L
        const val FRAME_MS = 10
        const val PREBUFFER = 4      // frames to collect before starting (40 ms)
        const val LOSS_GAP = 3       // later frames present => declare an earlier one lost
        const val MAX_BUFFERED = 12  // beyond this, skip ahead to bound latency
    }

    private val running = AtomicBoolean(false)
    private val lock = Object()
    private val buffer = TreeMap<Long, ByteArray>()
    private var maxSeq = -1L
    private val threads = java.util.Collections.synchronizedList(mutableListOf<Thread>())
    @Volatile private var socket: DatagramSocket? = null

    fun start() {
        if (!running.compareAndSet(false, true)) return
        Stats.received = 0; Stats.lost = 0; Stats.state = "Connecting to $host:$port"
        // Socket setup must not run on the main thread (StrictMode network policy), so do it here.
        threads += Thread {
            try {
                val s = DatagramSocket()
                s.connect(InetSocketAddress(host, port))
                s.soTimeout = 500
                socket = s
            } catch (e: Throwable) {
                Stats.state = "Socket error: ${e.javaClass.simpleName}: ${e.message}"
                running.set(false)
                return@Thread
            }
            threads += Thread(::recvLoop, "mw-recv").also { it.start() }
            threads += Thread(::punchLoop, "mw-punch").also { it.start() }
            threads += Thread(::playLoop, "mw-play").also { it.priority = Thread.MAX_PRIORITY; it.start() }
        }.also { it.start() }
    }

    fun stop() {
        if (!running.compareAndSet(true, false)) return
        socket?.close()
        synchronized(lock) { lock.notifyAll() }
        val ts = synchronized(threads) { threads.toList() }
        ts.forEach { it.interrupt(); it.join(1000) }
        threads.clear()
        if (!Stats.state.contains("error", ignoreCase = true)) Stats.state = "Idle"
    }

    private fun punchLoop() {
        // Seq must increase across app restarts (the agent keeps a replay window), so start from time.
        var seq = (System.currentTimeMillis() / 100) and 0xFFFFFFFFL
        var n = 0
        while (running.get()) {
            try {
                val d = Wire.seal(key, Wire.DIR_RECEIVER_TO_AGENT, STREAM_ID, seq, Wire.TYPE_PUNCH, 0, ByteArray(0))
                socket?.send(DatagramPacket(d, d.size))
                seq++
            } catch (_: Exception) {}
            n++
            try { Thread.sleep(if (n < 5) 150L else 1000L) } catch (_: InterruptedException) { return }
        }
    }

    private fun recvLoop() {
        val buf = ByteArray(2048)
        val pkt = DatagramPacket(buf, buf.size)
        var lastAudio = 0L
        while (running.get()) {
            try {
                socket?.receive(pkt)
            } catch (_: java.net.SocketTimeoutException) {
                if (lastAudio != 0L && System.currentTimeMillis() - lastAudio > 3000) Stats.state = "No audio from $host (is anything playing?)"
                continue
            } catch (_: Exception) {
                if (!running.get()) return else continue
            }
            val p = Wire.open(key, Wire.DIR_AGENT_TO_RECEIVER, STREAM_ID, buf, pkt.length) ?: continue
            if (p.type != Wire.TYPE_AUDIO || p.fragCnt != 1) continue
            lastAudio = System.currentTimeMillis()
            synchronized(lock) {
                if (!buffer.containsKey(p.seq)) {
                    buffer[p.seq] = p.payload
                    if (p.seq > maxSeq) maxSeq = p.seq
                    lock.notifyAll()
                }
            }
            Stats.received++
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
        val silence = ByteArray(1920)
        var next = -1L
        var emptySince = 0L
        try {
            while (running.get()) {
                var frame: ByteArray? = null
                var concealed = false
                synchronized(lock) {
                    if (next < 0) {
                        if (buffer.size >= PREBUFFER) next = buffer.firstKey() else lock.wait(20)
                    }
                    if (next >= 0) {
                        if (maxSeq - next > MAX_BUFFERED) { // too far behind: skip ahead, bounded latency
                            val target = maxSeq - PREBUFFER
                            Stats.lost += (target - next).coerceAtLeast(0)
                            buffer.headMap(target).clear()
                            next = target
                        }
                        frame = buffer.remove(next)
                        if (frame != null) {
                            next++
                            emptySince = 0
                        } else if (maxSeq - next >= LOSS_GAP) {
                            next++; Stats.lost++; concealed = true
                        } else if (buffer.isEmpty()) {
                            if (emptySince == 0L) emptySince = System.currentTimeMillis()
                            if (System.currentTimeMillis() - emptySince > 150) { next = -1; emptySince = 0 } // underrun: rebuffer
                            lock.wait(3)
                        } else lock.wait(3)
                        Stats.bufferMs = buffer.size * FRAME_MS
                    }
                }
                val f = frame
                val pcm = when {
                    f != null -> decoder.decode(f, FRAME_MS * 1000L)
                    concealed -> silence // TODO: libopus PLC; MediaCodec has none
                    else -> continue
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
