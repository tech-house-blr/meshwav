package com.techhouseblr.meshwav

import android.app.*
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.media.*
import android.media.projection.MediaProjection
import android.media.projection.MediaProjectionManager
import android.os.Build
import android.os.IBinder
import java.net.NetworkInterface

/** Spike: capture the phone's playback audio and hand it to the Rust source. */
class SpikeService : Service() {
    @Volatile private var running = false
    private var projection: MediaProjection? = null
    private var thread: Thread? = null

    override fun onBind(i: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == "stop") { shutdown(); stopSelf(); return START_NOT_STICKY }
        val nm = getSystemService(NotificationManager::class.java)
        nm.createNotificationChannel(NotificationChannel("spike", "Meshwav source", NotificationManager.IMPORTANCE_LOW))
        val n = Notification.Builder(this, "spike").setSmallIcon(R.drawable.ic_stat_meshwav).setContentTitle("Meshwav: sending this phone's audio").build()
        startForeground(2, n, ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PROJECTION)
        val code = intent!!.getIntExtra("code", 0)
        val data = intent.getParcelableExtra<Intent>("data")!!
        val pairCode = intent.getStringExtra("pair")!!
        val mpm = getSystemService(Context.MEDIA_PROJECTION_SERVICE) as MediaProjectionManager
        projection = mpm.getMediaProjection(code, data)
        projection!!.registerCallback(object : MediaProjection.Callback() { override fun onStop() { shutdown() } }, null)
        val ip = tailnetIp() ?: "127.0.0.1"
        link = NativeSource.start(ip, 47901, pairCode)
        NativeSource.setLossless(intent.getBooleanExtra("lossless", true))
        val cfg = AudioPlaybackCaptureConfiguration.Builder(projection!!)
            .addMatchingUsage(AudioAttributes.USAGE_MEDIA).addMatchingUsage(AudioAttributes.USAGE_GAME).addMatchingUsage(AudioAttributes.USAGE_UNKNOWN).build()
        val fmt = AudioFormat.Builder().setEncoding(AudioFormat.ENCODING_PCM_16BIT).setSampleRate(48000).setChannelMask(AudioFormat.CHANNEL_IN_STEREO).build()
        val rec = AudioRecord.Builder().setAudioPlaybackCaptureConfig(cfg).setAudioFormat(fmt).setBufferSizeInBytes(48000 * 4 / 5).build()
        muteLocal()
        running = true
        thread = Thread {
            val buf = ShortArray(960)
            rec.startRecording()
            val am = getSystemService(AudioManager::class.java)
            var tick = 0
            while (running) {
                // keep the phone silent: undo any volume change within about 200 ms
                if (++tick % 20 == 0 && am.getStreamVolume(AudioManager.STREAM_MUSIC) != 0) am.setStreamVolume(AudioManager.STREAM_MUSIC, 0, 0)
                val got = rec.read(buf, 0, buf.size)
                if (got > 0) { peak = buf.take(got).maxOf { kotlin.math.abs(it.toInt()) }; NativeSource.push(buf, got) }
            }
            rec.stop(); rec.release()
        }.also { it.start() }
        return START_NOT_STICKY
    }

    /** Silence the phone itself while sending; playback capture is unaffected by media volume. */
    private fun muteLocal() {
        val am = getSystemService(AudioManager::class.java)
        val prefs = getSharedPreferences("source", MODE_PRIVATE)
        if (!prefs.contains("saved_volume")) prefs.edit().putInt("saved_volume", am.getStreamVolume(AudioManager.STREAM_MUSIC)).apply()
        am.setStreamVolume(AudioManager.STREAM_MUSIC, 0, 0)
    }

    private fun restoreLocal() {
        val prefs = getSharedPreferences("source", MODE_PRIVATE)
        if (!prefs.contains("saved_volume")) return
        getSystemService(AudioManager::class.java).setStreamVolume(AudioManager.STREAM_MUSIC, prefs.getInt("saved_volume", 0), 0)
        prefs.edit().remove("saved_volume").apply()
    }

    private fun shutdown() {
        restoreLocal()
        running = false
        NativeSource.stop()
        projection?.stop(); projection = null
        link = ""
    }

    override fun onDestroy() { shutdown(); super.onDestroy() }

    private fun tailnetIp(): String? = NetworkInterface.getNetworkInterfaces().toList().flatMap { it.inetAddresses.toList() }
        .map { it.hostAddress ?: "" }.firstOrNull { val p = it.split("."); p.size == 4 && p[0] == "100" && (p[1].toIntOrNull() ?: 0) in 64..127 }

    companion object { @Volatile var link = ""; @Volatile var peak = 0 }
}
