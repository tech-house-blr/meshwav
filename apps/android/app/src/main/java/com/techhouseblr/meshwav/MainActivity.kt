package com.techhouseblr.meshwav

import android.Manifest
import android.app.Activity
import android.content.Intent
import android.graphics.Color
import android.graphics.Typeface
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.text.InputType
import android.view.Gravity
import android.widget.Button
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.TextView

class MainActivity : Activity() {
    private lateinit var pairing: EditText
    private lateinit var button: Button
    private lateinit var status: TextView
    private val ui = Handler(Looper.getMainLooper())

    private data class Target(val host: String, val port: Int, val key: String)

    private fun parse(text: String): Target? {
        val t = text.trim()
        if (t.startsWith("meshwav://")) {
            val u = Uri.parse(t)
            val key = u.getQueryParameter("key") ?: return null
            return Target(u.getQueryParameter("host") ?: return null, u.getQueryParameter("port")?.toIntOrNull() ?: 47900, key)
        }
        fun field(n: String) = Regex("""\b$n=(\S+)""").find(t)?.groupValues?.get(1)
        val key = field("key") ?: return null
        if (key.length != 64) return null
        return Target(field("host") ?: return null, field("port")?.toIntOrNull() ?: 47900, key)
    }

    override fun onCreate(b: Bundle?) {
        super.onCreate(b)
        CrashLog.install(this)
        val pad = (20 * resources.displayMetrics.density).toInt()
        val root = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL; setPadding(pad, pad * 2, pad, pad); setBackgroundColor(Color.parseColor("#101418")) }
        root.addView(TextView(this).apply { text = "Meshwav"; textSize = 28f; setTextColor(Color.WHITE); typeface = Typeface.DEFAULT_BOLD })
        root.addView(TextView(this).apply { text = "Paste the agent's pairing line once; it is remembered."; setTextColor(Color.parseColor("#9aa4ae")); setPadding(0, pad / 2, 0, pad / 2) })
        pairing = EditText(this).apply {
            hint = "host=100.x.y.z port=47900 key=..."; setHintTextColor(Color.GRAY); setTextColor(Color.WHITE)
            inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_FLAG_MULTI_LINE; minLines = 3
            setText(getSharedPreferences("mw", MODE_PRIVATE).getString("pairing", ""))
        }
        root.addView(pairing)
        button = Button(this).apply { setOnClickListener { toggle() } }
        root.addView(button, LinearLayout.LayoutParams(-1, -2).apply { topMargin = pad })
        status = TextView(this).apply { setTextColor(Color.parseColor("#7fd1ae")); textSize = 16f; gravity = Gravity.START; setPadding(0, pad, 0, 0) }
        root.addView(status)
        setContentView(root)
        CrashLog.last(this)?.let { status.text = "Last crash:\n$it" }
        if (Build.VERSION.SDK_INT >= 33) requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), 1)
        handleIntent(intent)
        tick()
    }

    override fun onNewIntent(i: Intent) { super.onNewIntent(i); handleIntent(i) }

    private fun handleIntent(i: Intent?) {
        val d = i?.dataString ?: return
        pairing.setText(d)
        toggle(forceStart = true)
    }

    private fun toggle(forceStart: Boolean = false) {
        if (PlaybackService.running && !forceStart) {
            startService(Intent(this, PlaybackService::class.java).setAction(PlaybackService.ACTION_STOP))
            return
        }
        val t = parse(pairing.text.toString())
        if (t == null) { status.text = "Could not read the pairing line (need host, port, 64-hex key)."; return }
        getSharedPreferences("mw", MODE_PRIVATE).edit().putString("pairing", pairing.text.toString()).apply()
        startForegroundService(Intent(this, PlaybackService::class.java).putExtra("host", t.host).putExtra("port", t.port).putExtra("key", t.key))
    }

    private fun statsText(): String = with(Stats) {
        "$state\nreceived $received  lost $lost  late $late  reordered $reordered  dup $duplicates\n" +
            "buffer $bufferMs ms (target $targetMs ms)  jitter ${"%.1f".format(jitterMs)} ms\n" +
            "trimmed $trimmed  underruns $underruns  resyncs $resyncs  reconnects $reconnects"
    }

    private fun tick() {
        button.text = if (PlaybackService.running) "Disconnect" else "Connect"
        if (PlaybackService.running || Stats.state.contains("error", ignoreCase = true)) status.text = statsText()
        ui.postDelayed(::tick, 500)
    }
}
