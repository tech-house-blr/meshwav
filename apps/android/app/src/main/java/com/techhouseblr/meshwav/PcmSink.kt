package com.techhouseblr.meshwav

import android.content.Context
import android.media.AudioAttributes
import android.media.AudioDeviceInfo
import android.media.AudioFormat
import android.media.AudioManager
import android.media.AudioTimestamp
import android.media.AudioTrack

/** Where decoded audio (48 kHz stereo 16-bit) goes. Used from the playback thread only. */
interface PcmSink {
    fun write(pcm: ByteArray)
    /** Time from handing a frame over to it reaching the output, in ms; -1 when unknown. */
    fun queueMs(): Double
    fun underruns(): Int
    fun bufferMs(): Int
    /** Underrun events and the longest gap between writes in ms; -1 when this sink does not track them. */
    fun stalls(): Int = -1
    fun maxGapMs(): Int = -1
    fun lateWrites(): Int = -1
    fun lateStalls(): Int = -1
    fun describe(): String
    fun close()

    companion object {
        /** The native sink when asked for and available, otherwise an AudioTrack. The note says why a fallback happened. */
        fun create(context: Context?, preferNative: Boolean): Pair<PcmSink, String> {
            if (preferNative) {
                try {
                    return Pair(AAudioSink(context), "")
                } catch (e: Throwable) {
                    return Pair(TrackSink(), "native output unavailable (${e.message}), using AudioTrack; ")
                }
            }
            return Pair(TrackSink(), "")
        }

        fun routeName(type: Int?): String = when (type) {
            null -> "route unknown"
            1 -> "phone earpiece"; 2 -> "phone speaker"; 3, 4 -> "wired headphones"
            7 -> "Bluetooth call"; 8 -> "Bluetooth A2DP"; 11, 12, 22 -> "USB audio"; 26 -> "Bluetooth LE"
            else -> "output type $type"
        }
    }
}

class TrackSink : PcmSink {
    private val track: AudioTrack
    private val ts = AudioTimestamp()
    private var written = 0L

    init {
        val min = AudioTrack.getMinBufferSize(48000, AudioFormat.CHANNEL_OUT_STEREO, AudioFormat.ENCODING_PCM_16BIT)
        track = AudioTrack.Builder()
            .setAudioAttributes(AudioAttributes.Builder().setUsage(AudioAttributes.USAGE_MEDIA).setContentType(AudioAttributes.CONTENT_TYPE_MUSIC).build())
            .setAudioFormat(AudioFormat.Builder().setSampleRate(48000).setEncoding(AudioFormat.ENCODING_PCM_16BIT).setChannelMask(AudioFormat.CHANNEL_OUT_STEREO).build())
            .setTransferMode(AudioTrack.MODE_STREAM)
            .setPerformanceMode(AudioTrack.PERFORMANCE_MODE_LOW_LATENCY)
            .setBufferSizeInBytes(maxOf(min, 1920 * 4))
            .build()
        track.play()
    }

    override fun write(pcm: ByteArray) {
        track.write(pcm, 0, pcm.size)
        written += pcm.size / 4
    }

    /** Frames handed over minus the position the output has reached, corrected for the age of that position. */
    override fun queueMs(): Double =
        if (track.getTimestamp(ts)) (written - ts.framePosition) * 1000.0 / 48000 - (System.nanoTime() - ts.nanoTime) / 1e6 else -1.0

    override fun underruns() = track.underrunCount
    override fun bufferMs() = track.bufferSizeInFrames * 1000 / 48000

    /** What Android actually opened, as reported by the AudioTrack. It cannot show whether the system mixer resamples. */
    override fun describe(): String {
        val t = track
        val enc = when (t.audioFormat) { AudioFormat.ENCODING_PCM_16BIT -> "16-bit"; AudioFormat.ENCODING_PCM_FLOAT -> "float"; else -> "format ${t.audioFormat}" }
        val dev = try { PcmSink.routeName(t.routedDevice?.type) } catch (_: Exception) { "route unknown" }
        val mode = if (android.os.Build.VERSION.SDK_INT >= 29) when (t.performanceMode) {
            AudioTrack.PERFORMANCE_MODE_LOW_LATENCY -> "low-latency granted"
            AudioTrack.PERFORMANCE_MODE_POWER_SAVING -> "power-saving"
            else -> "low-latency NOT granted"
        } else "mode unknown"
        return "${t.sampleRate} Hz, $enc, $dev, $mode"
    }

    override fun close() {
        try { track.stop() } catch (_: Exception) {}
        track.release()
    }
}

class AAudioSink(private val context: Context?) : PcmSink {
    private val info: List<String>

    init {
        val r = NativeOut.open()
        if (!r.startsWith("ok:")) throw IllegalStateException(r.removePrefix("error: "))
        info = r.removePrefix("ok:").split(":")
    }

    override fun write(pcm: ByteArray) { NativeOut.write(pcm, pcm.size) }
    override fun queueMs() = NativeOut.queueMs()
    override fun underruns() = NativeOut.underruns()
    override fun bufferMs() = NativeOut.bufferMs()
    override fun stalls() = NativeOut.stalls()
    override fun maxGapMs() = NativeOut.maxGapMs()
    override fun lateWrites() = NativeOut.lateWrites()
    override fun lateStalls() = NativeOut.lateStalls()

    override fun describe(): String {
        val id = info.getOrNull(2)?.toIntOrNull()
        val type = try {
            (context?.getSystemService(Context.AUDIO_SERVICE) as? AudioManager)
                ?.getDevices(AudioManager.GET_DEVICES_OUTPUTS)?.firstOrNull { it.id == id }?.type
        } catch (_: Exception) { null }
        val mode = if (info.getOrNull(1) == "LowLatency") "low-latency granted" else "low-latency NOT granted"
        return "AAudio ${info.getOrNull(0)?.lowercase()}, 48000 Hz, 16-bit, ${PcmSink.routeName(type)}, $mode"
    }

    override fun close() = NativeOut.close()
}
