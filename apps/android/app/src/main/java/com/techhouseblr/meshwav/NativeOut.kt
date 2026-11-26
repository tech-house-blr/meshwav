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
    /** Underrun events (a long stall counts once), unlike [underruns], which counts every empty burst. */
    external fun stalls(): Int
    /** Longest time between two writes since the stream started, in ms. */
    external fun maxGapMs(): Int
    /** Writes more than 20 ms after the previous one. */
    external fun lateWrites(): Int
    /** Stalls that happened right after such a late write (the app fed the stream late). */
    external fun lateStalls(): Int
    external fun bufferMs(): Int
    external fun close()
}
