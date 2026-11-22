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

    private lateinit var store: PairingStore
    private lateinit var pairedInfo: TextView

    /** What the agent prints when pairing: host, port, its public key and the one-time code. */
    private data class Target(val host: String, val port: Int, val pk: String, val code: String)

    private fun parse(text: String): Target? {
        val t = text.trim()
        val get: (String) -> String? = if (t.startsWith("meshwav://")) {
            val u = Uri.parse(t)
            ({ n -> u.getQueryParameter(n) })
        } else {
            ({ n -> Regex("""\b$n=(\S+)""").find(t)?.groupValues?.get(1) })
        }
        val pk = get("pk")?.lowercase() ?: return null
        val code = get("code") ?: return null
        if (!Regex("[0-9a-f]{64}").matches(pk) || !Handshake.validCode(code)) return null
        return Target(get("host") ?: return null, get("port")?.toIntOrNull() ?: 47900, pk, code)
    }

    override fun onCreate(b: Bundle?) {
        super.onCreate(b)
        CrashLog.install(this)
        store = PairingStore(applicationContext)
        getSharedPreferences("mw", MODE_PRIVATE).edit().remove("pairing").apply() // old shared-key line is obsolete
        val pad = (20 * resources.displayMetrics.density).toInt()
        val root = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL; setPadding(pad, pad * 2, pad, pad); setBackgroundColor(Color.parseColor("#101418")) }
        root.addView(TextView(this).apply { text = "Meshwav"; textSize = 28f; setTextColor(Color.WHITE); typeface = Typeface.DEFAULT_BOLD })
        root.addView(TextView(this).apply { text = "Run meshwav-agent --pair on the PC, then paste its pairing line here. It is needed once; the pairing is remembered."; setTextColor(Color.parseColor("#9aa4ae")); setPadding(0, pad / 2, 0, pad / 2) })
        pairing = EditText(this).apply {
            hint = "meshwav://pair?host=...&pk=...&code=123456"; setHintTextColor(Color.GRAY); setTextColor(Color.WHITE)
            inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_FLAG_MULTI_LINE; minLines = 3
        }
        root.addView(pairing)
        pairedInfo = TextView(this).apply { setTextColor(Color.parseColor("#9aa4ae")); setPadding(0, pad / 2, 0, 0); setOnLongClickListener { store.forget(); true } }
        root.addView(pairedInfo)
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
        val text = pairing.text.toString()
        val svc = Intent(this, PlaybackService::class.java)
        if (text.isNotBlank()) {
            val t = parse(text)
            if (t == null) { status.text = "Could not read the pairing line. It needs host, pk (64 hex) and the 6-digit code from the agent."; return }
            svc.putExtra("host", t.host).putExtra("port", t.port).putExtra("pk", t.pk).putExtra("code", t.code)
            pairing.setText("") // the code is single-use; never keep it around
        } else {
            val p = store.paired()
            if (p == null) { status.text = "Not paired yet. Run meshwav-agent --pair on the PC and paste the pairing line above."; return }
            svc.putExtra("host", p.host).putExtra("port", p.port).putExtra("pk", p.agentPublic.joinToString("") { "%02x".format(it) })
        }
        startForegroundService(svc)
    }

    private fun statsText(): String = with(Stats) {
        "$state\nreceived $received  lost $lost  late $late  reordered $reordered  dup $duplicates\n" +
            "buffer $bufferMs ms (target $targetMs ms)  jitter ${"%.1f".format(jitterMs)} ms\n" +
            "trimmed $trimmed  underruns $underruns  resyncs $resyncs  reconnects $reconnects"
    }

    private fun tick() {
        button.text = if (PlaybackService.running) "Disconnect" else "Connect"
        pairedInfo.text = store.paired()?.let { "Paired with ${it.host}:${it.port}  (agent key ${it.agentPublic.take(6).joinToString("") { b -> "%02x".format(b) }}…)  long-press to forget" } ?: "Not paired"
        if (PlaybackService.running || Stats.state.contains("error", ignoreCase = true)) status.text = statsText()
        ui.postDelayed(::tick, 500)
    }
}
