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
import android.view.View
import android.view.ViewGroup
import android.widget.AdapterView
import android.widget.ArrayAdapter
import android.widget.Spinner
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
        val prefs = getSharedPreferences("mw", MODE_PRIVATE)
        Controls.desiredKbps = prefs.getInt("kbps", 0).takeIf { it in Controls.kbpsChoices.toList() } ?: 0
        Controls.desiredFrameMs = prefs.getInt("frame_ms", 0).takeIf { it in Controls.frameChoices.toList() } ?: 0
        root.addView(TextView(this).apply { text = "Encoder (applies live; smaller frames mean lower latency but more packets)"; setTextColor(Color.parseColor("#9aa4ae")); setPadding(0, pad, 0, 0) })
        root.addView(choice("Bitrate", listOf("Agent default") + Controls.kbpsChoices.map { "$it kbit/s" }, Controls.kbpsChoices.indexOf(Controls.desiredKbps) + 1) { i ->
            Controls.desiredKbps = if (i == 0) 0 else Controls.kbpsChoices[i - 1]
            prefs.edit().putInt("kbps", Controls.desiredKbps).apply()
        })
        root.addView(choice("Frame size", listOf("Agent default") + Controls.frameChoices.map { "$it ms" }, Controls.frameChoices.indexOf(Controls.desiredFrameMs) + 1) { i ->
            Controls.desiredFrameMs = if (i == 0) 0 else Controls.frameChoices[i - 1]
            prefs.edit().putInt("frame_ms", Controls.desiredFrameMs).apply()
        })
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

    /** A labelled drop-down. [onPick] fires only for user changes, not for the initial selection. */
    private fun choice(label: String, items: List<String>, selected: Int, onPick: (Int) -> Unit): LinearLayout {
        val row = LinearLayout(this).apply { orientation = LinearLayout.HORIZONTAL; gravity = Gravity.CENTER_VERTICAL }
        row.addView(TextView(this).apply { text = label; setTextColor(Color.WHITE) }, LinearLayout.LayoutParams(0, -2, 1f))
        val adapter = object : ArrayAdapter<String>(this, android.R.layout.simple_spinner_dropdown_item, items) {
            override fun getView(position: Int, v: View?, parent: ViewGroup): View = super.getView(position, v, parent).also { (it as TextView).setTextColor(Color.WHITE) }
        }
        val sp = Spinner(this).apply { this.adapter = adapter; setSelection(selected, false) }
        sp.onItemSelectedListener = object : AdapterView.OnItemSelectedListener {
            var first = true
            override fun onItemSelected(p: AdapterView<*>?, v: View?, pos: Int, id: Long) { if (first) { first = false; return }; onPick(pos) }
            override fun onNothingSelected(p: AdapterView<*>?) {}
        }
        row.addView(sp, LinearLayout.LayoutParams(0, -2, 1f))
        return row
    }

    private fun encoderText(): String = with(Stats) {
        val k = if (kbps > 0) "$kbps kbit/s" else "bitrate: agent default"
        val f = if (frameMs > 0) "$frameMs ms frames" else "frame size unknown"
        "encoder $k, $f" + if (configNote.isNotEmpty()) "  ($configNote)" else ""
    }

    private fun statsText(): String = with(Stats) {
        "$state\n${encoderText()}\nreceived $received  lost $lost  late $late  reordered $reordered  dup $duplicates\n" +
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
