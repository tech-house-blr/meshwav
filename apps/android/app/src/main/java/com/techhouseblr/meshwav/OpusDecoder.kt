package com.techhouseblr.meshwav

import android.media.MediaCodec
import android.media.MediaFormat
import java.nio.ByteBuffer
import java.nio.ByteOrder

/** Opus 48 kHz stereo decode through the platform MediaCodec (no native code). */
class OpusDecoder {
    private val codec: MediaCodec = MediaCodec.createDecoderByType(MediaFormat.MIMETYPE_AUDIO_OPUS)
    private val info = MediaCodec.BufferInfo()
    private var ptsUs = 0L

    init {
        val head = ByteBuffer.allocate(19).order(ByteOrder.LITTLE_ENDIAN)
        head.put("OpusHead".toByteArray()).put(1).put(2).putShort(312).putInt(48000).putShort(0).put(0)
        head.flip()
        val fmt = MediaFormat.createAudioFormat(MediaFormat.MIMETYPE_AUDIO_OPUS, 48000, 2)
        fmt.setByteBuffer("csd-0", head)
        fmt.setByteBuffer("csd-1", ByteBuffer.allocate(8).order(ByteOrder.LITTLE_ENDIAN).putLong(6_500_000L).flip() as ByteBuffer)
        fmt.setByteBuffer("csd-2", ByteBuffer.allocate(8).order(ByteOrder.LITTLE_ENDIAN).putLong(80_000_000L).flip() as ByteBuffer)
        codec.configure(fmt, null, null, 0)
        codec.start()
    }

    /** Decode one packet; returns interleaved PCM16 bytes (possibly empty while the decoder primes). */
    fun decode(packet: ByteArray, frameUs: Long): ByteArray {
        val inIdx = codec.dequeueInputBuffer(5_000)
        if (inIdx < 0) return ByteArray(0)
        val inBuf = codec.getInputBuffer(inIdx)!!
        inBuf.clear()
        inBuf.put(packet)
        codec.queueInputBuffer(inIdx, 0, packet.size, ptsUs, 0)
        ptsUs += frameUs
        val out = java.io.ByteArrayOutputStream()
        var wait = 5_000L
        while (true) {
            val o = codec.dequeueOutputBuffer(info, wait)
            if (o >= 0) {
                val buf = codec.getOutputBuffer(o)!!
                val bytes = ByteArray(info.size)
                buf.position(info.offset)
                buf.get(bytes)
                out.write(bytes)
                codec.releaseOutputBuffer(o, false)
                wait = 0
            } else if (o == MediaCodec.INFO_OUTPUT_FORMAT_CHANGED || o == MediaCodec.INFO_OUTPUT_BUFFERS_CHANGED) {
                continue
            } else break
        }
        return out.toByteArray()
    }

    fun release() {
        try { codec.stop() } catch (_: Exception) {}
        codec.release()
    }
}
