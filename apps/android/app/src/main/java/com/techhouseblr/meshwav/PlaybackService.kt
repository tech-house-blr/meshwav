package com.techhouseblr.meshwav

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.net.wifi.WifiManager
import android.os.IBinder
import android.os.PowerManager

class PlaybackService : Service() {
    companion object {
        const val ACTION_STOP = "stop"
        const val CHANNEL = "playback"
        @Volatile var running = false
        /** The user pressed Disconnect or Stop: do not reconnect by itself until they ask again. */
        @Volatile var userStopped = false
        /** Asks the running receiver to take the audio from the sender; null while nothing is running. */
        @Volatile var playHere: (() -> Unit)? = null
        /** Asks the device that is playing to use this level (0..100); null while nothing is running. */
        @Volatile var setRemoteVolume: ((Int) -> Unit)? = null
    }

    private var receiver: StreamReceiver? = null
    private var wake: PowerManager.WakeLock? = null
    private var wifi: WifiManager.WifiLock? = null

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        CrashLog.install(this)
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val prefs = getSharedPreferences("mw", MODE_PRIVATE)
        if (intent?.action == ACTION_STOP) { userStopped = true; teardown(); return START_NOT_STICKY }
        // Restarted by the system without an intent: reconnect to the paired sender.
        val saved = if (intent == null) PairingStore(applicationContext).paired() else null
        if (intent == null && (saved == null || !prefs.getBoolean("auto", true))) { teardown(); return START_NOT_STICKY }
        userStopped = false
        val host = intent?.getStringExtra("host") ?: saved?.host ?: ""
        val port = intent?.getIntExtra("port", 47900) ?: saved?.port ?: 47900
        val nm = getSystemService(NotificationManager::class.java)
        nm.createNotificationChannel(NotificationChannel(CHANNEL, "Playback", NotificationManager.IMPORTANCE_LOW))
        val open = PendingIntent.getActivity(this, 0, Intent(this, MainActivity::class.java), PendingIntent.FLAG_IMMUTABLE)
        val stop = PendingIntent.getService(this, 1, Intent(this, PlaybackService::class.java).setAction(ACTION_STOP), PendingIntent.FLAG_IMMUTABLE)
        val n = Notification.Builder(this, CHANNEL)
            .setContentTitle("Meshwav").setContentText("Streaming from $host")
            .setSmallIcon(R.drawable.ic_stat_meshwav).setContentIntent(open)
            .addAction(Notification.Action.Builder(android.graphics.drawable.Icon.createWithResource(this, android.R.drawable.ic_media_pause), "Stop", stop).build())
            .setOngoing(true).build()
        startForeground(1, n)
        val pkHex = intent?.getStringExtra("pk") ?: saved?.agentPublic?.joinToString("") { "%02x".format(it) } ?: ""
        if (host.isEmpty() || !Regex("[0-9a-f]{64}").matches(pkHex)) { Stats.state = "Pairing error: bad agent key"; teardown(); return START_NOT_STICKY }
        if (!Handshake.tailnetHost(host)) { Stats.state = "Refused: $host is not a Tailscale address"; teardown(); return START_NOT_STICKY }
        val agentPk = Wire.hex(pkHex)
        val code = intent?.getStringExtra("code")?.takeIf { Handshake.validCode(it) }
        val store = PairingStore(applicationContext)
        wake = (getSystemService(Context.POWER_SERVICE) as PowerManager).newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "meshwav:play").apply { acquire() }
        wifi = (applicationContext.getSystemService(Context.WIFI_SERVICE) as WifiManager).createWifiLock(WifiManager.WIFI_MODE_FULL_LOW_LATENCY, "meshwav:wifi").apply { acquire() }
        receiver?.stop()
        receiver = StreamReceiver(
            host, port, store.identityPrivate(), agentPk, code, android.os.Build.MODEL ?: "Android",
            applicationContext,
            onPaired = { store.savePaired(host, port, agentPk, makeDefault = code != null); store.setName(pkHex, Stats.agentName) },
            onName = { store.setName(pkHex, it) },
        ).also { r -> r.start(); playHere = { r.playHere() }; setRemoteVolume = { p -> r.setRemoteVolume(p) } }
        if (code == null) store.savePaired(host, port, agentPk) // already paired: keep it current
        running = true
        return START_STICKY
    }

    private fun teardown() {
        playHere = null; setRemoteVolume = null
        receiver?.stop(); receiver = null
        wake?.let { if (it.isHeld) it.release() }; wifi?.let { if (it.isHeld) it.release() }
        running = false
        stopForeground(STOP_FOREGROUND_REMOVE)
        stopSelf()
    }

    override fun onDestroy() { teardown(); super.onDestroy() }
}
