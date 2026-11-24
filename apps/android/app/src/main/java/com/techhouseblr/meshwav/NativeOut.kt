package com.techhouseblr.meshwav

/** Native (AAudio) audio output, implemented in the Rust library. */
object NativeOut {
    init { System.loadLibrary("android_spike") }
    /** "ok:<sharing>:<performance>:<device id>" or "error: ...". */
    external fun open(): String
    /** Queues interleaved stereo 16-bit PCM, blocking until it fits. Returns frames written or -1. */
    external fun write(pcm: ByteArray, n: Int): Int
    /** Milliseconds from queueing a frame now to it reaching the output; -1 when unknown. */
    external fun queueMs(): Double
    external fun underruns(): Int
    external fun bufferMs(): Int
    external fun close()
}
