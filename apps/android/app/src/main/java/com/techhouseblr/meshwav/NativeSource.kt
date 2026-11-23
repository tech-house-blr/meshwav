package com.techhouseblr.meshwav

/** Spike: the Rust agent send path. */
object NativeSource {
    init { System.loadLibrary("android_spike") }
    external fun start(ip: String, port: Int, code: String): String
    external fun push(pcm: ShortArray, n: Int)
    external fun setLossless(on: Boolean)
    external fun stats(): String
    external fun stop(): Long
}
