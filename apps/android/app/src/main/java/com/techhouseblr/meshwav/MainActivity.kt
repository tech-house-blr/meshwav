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
    private lateinit var codeField: EditText
    private lateinit var pairBtn: Button
    private var invites: InviteListener? = null
    private lateinit var button: Button
    private lateinit var status: TextView
    private val ui = Handler(Looper.getMainLooper())

    private lateinit var store: PairingStore
    private lateinit var sendersBox: LinearLayout
    private var sendersShown = ""
    private lateinit var quality: LinearLayout

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
        var root = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL; setPadding(pad, pad * 2, pad, pad); setBackgroundColor(SourcePanel.BG) }
        root.addView(TextView(this).apply { text = "Meshwav"; textSize = 28f; setTextColor(Color.WHITE); typeface = Typeface.DEFAULT_BOLD })
        source = SourcePanel(this)
        val modeRow = LinearLayout(this).apply { orientation = LinearLayout.HORIZONTAL; setPadding(0, pad / 2, 0, pad / 2) }
        fun seg(t: String, on: () -> Unit) = Button(this).apply {
            text = t; isAllCaps = false; textSize = 15f; stateListAnimator = null
            background = android.graphics.drawable.GradientDrawable().apply { setColor(SourcePanel.PANEL); setStroke((1 * resources.displayMetrics.density).toInt(), SourcePanel.LINE) }
            setOnClickListener { on() }
        }
        recvBtn = seg("Receive") { setMode(false) }
        sendBtn = seg("Send") { setMode(true) }
        modeRow.addView(recvBtn, LinearLayout.LayoutParams(0, (46 * resources.displayMetrics.density).toInt(), 1f))
        modeRow.addView(sendBtn, LinearLayout.LayoutParams(0, (46 * resources.displayMetrics.density).toInt(), 1f).apply { leftMargin = pad / 2 })
        root.addView(modeRow)
        val real = root
        root = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        root.addView(TextView(this).apply { text = "On the computer choose Send and Show pairing code (or run meshwav pair), then type the 6-digit code here. It is needed once; the pairing is remembered."; setTextColor(Color.parseColor("#9aa4ae")); setPadding(0, pad / 2, 0, pad / 2) })
        codeField = EditText(this).apply {
            hint = "123456"; setHintTextColor(Color.GRAY); setTextColor(Color.WHITE); textSize = 30f
            inputType = InputType.TYPE_CLASS_NUMBER; filters = arrayOf(android.text.InputFilter.LengthFilter(6)); letterSpacing = 0.3f
        }
        root.addView(codeField)
        pairBtn = Button(this).apply { text = "Pair"; setOnClickListener { pairByCode() } }
        root.addView(pairBtn, LinearLayout.LayoutParams(-1, -2).apply { topMargin = pad / 2 })
        pairing = EditText(this).apply {
            hint = "meshwav://pair?host=...&pk=...&code=123456"; setHintTextColor(Color.GRAY); setTextColor(Color.WHITE)
            inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_FLAG_MULTI_LINE; minLines = 3; visibility = View.GONE
        }
        root.addView(TextView(this).apply {
            text = "Use a pairing link instead"; setTextColor(Color.parseColor("#7fd1ae")); setPadding(0, pad / 2, 0, pad / 2)
            setOnClickListener { pairing.visibility = if (pairing.visibility == View.GONE) View.VISIBLE else View.GONE }
        })
        root.addView(pairing)
        val prefs = getSharedPreferences("mw", MODE_PRIVATE)
        root.addView(TextView(this).apply { text = "Paired senders"; setTextColor(Color.WHITE); textSize = 16f; setPadding(0, pad, 0, 0) })
        sendersBox = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        root.addView(sendersBox)
        root.addView(android.widget.CheckBox(this).apply {
            text = "Reconnect automatically to the default sender"
            setTextColor(Color.WHITE); isChecked = prefs.getBoolean("auto", true)
            setOnCheckedChangeListener { _, on -> prefs.edit().putBoolean("auto", on).apply() }
        })
        Controls.lowLatency = prefs.getBoolean("low_latency", false)
        root.addView(android.widget.CheckBox(this).apply {
            text = "Low latency (smaller buffer; may crackle on weak Wi-Fi; applies on next connect)"
            setTextColor(Color.WHITE); isChecked = Controls.lowLatency
            setOnCheckedChangeListener { _, on -> Controls.lowLatency = on; prefs.edit().putBoolean("low_latency", on).apply() }
        })
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
        Controls.desiredCodec = prefs.getInt("codec", 1).takeIf { it == 1 || it == 2 } ?: 1
        root.addView(TextView(this).apply { text = "Quality (lossless sends the PC audio uncompressed, about 1.7 Mbit/s, always in 5 ms packets; it drops back to Opus if the connection cannot carry it)"; setTextColor(Color.parseColor("#9aa4ae")); setPadding(0, pad / 2, 0, 0) })
        quality = choice("Quality", listOf("Opus (compressed)", "Lossless PCM"), Controls.desiredCodec - 1) { i ->
            Controls.desiredCodec = i + 1
            if (i == 1) Stats.qualityNote = ""
            prefs.edit().putInt("codec", Controls.desiredCodec).apply()
        }
        root.addView(quality)
        button = Button(this).apply { setOnClickListener { toggle() } }
        root.addView(button, LinearLayout.LayoutParams(-1, -2).apply { topMargin = pad })
        status = TextView(this).apply { setTextColor(Color.parseColor("#7fd1ae")); textSize = 16f; gravity = Gravity.START; setPadding(0, pad, 0, 0) }
        root.addView(status)
        recvBox = root
        real.addView(recvBox)
        real.addView(source.view.also { it.visibility = View.GONE })
        setContentView(android.widget.ScrollView(this).apply { setBackgroundColor(SourcePanel.BG); isFillViewport = true; addView(real) })
        setMode(getSharedPreferences("mw", MODE_PRIVATE).getBoolean("send_mode", false))
        CrashLog.last(this)?.let { status.text = "Last crash:\n$it" }
        requestPermissions(if (Build.VERSION.SDK_INT >= 33) arrayOf(Manifest.permission.POST_NOTIFICATIONS, Manifest.permission.RECORD_AUDIO) else arrayOf(Manifest.permission.RECORD_AUDIO), 1)
        handleIntent(intent)
        tick()
    }

    private lateinit var source: SourcePanel
    private lateinit var recvBox: View
    private lateinit var recvBtn: Button
    private lateinit var sendBtn: Button

    private fun setMode(send: Boolean) {
        getSharedPreferences("mw", MODE_PRIVATE).edit().putBoolean("send_mode", send).apply()
        recvBox.visibility = if (send) View.GONE else View.VISIBLE
        source.view.visibility = if (send) View.VISIBLE else View.GONE
        for ((b, on) in listOf(recvBtn to !send, sendBtn to send)) {
            b.setTextColor(if (on) SourcePanel.BG else SourcePanel.TEXT)
            (b.background as android.graphics.drawable.GradientDrawable).setColor(if (on) SourcePanel.TEAL else SourcePanel.PANEL)
        }
        if (send) source.start() else source.stop()
    }

    override fun onActivityResult(req: Int, res: Int, data: Intent?) { source.onResult(req, res, data) }

    override fun onResume() {
        super.onResume()
        if (source.view.visibility == View.VISIBLE) source.start() else autoConnect()
    }

    /** Stay connected to the paired sender so it shows this phone as online without tapping Connect. */
    private fun autoConnect() {
        if (PlaybackService.running || PlaybackService.userStopped || invites != null) return
        if (!getSharedPreferences("mw", MODE_PRIVATE).getBoolean("auto", true)) return
        val p = store.paired() ?: return
        startReceiving(p.host, p.port, p.agentPublic.joinToString("") { "%02x".format(it) }, null)
    }

    private fun startReceiving(host: String, port: Int, pkHex: String, code: String?) {
        val svc = Intent(this, PlaybackService::class.java).putExtra("host", host).putExtra("port", port).putExtra("pk", pkHex)
        if (code != null) svc.putExtra("code", code)
        startForegroundService(svc)
    }

    /** Wait for a sender that is showing a pairing code (it announces itself), then pair with the typed code. */
    private fun pairByCode() {
        invites?.let { it.cancel(); invites = null; pairBtn.text = "Pair"; status.text = ""; return }
        val code = codeField.text.toString().trim()
        if (!Handshake.validCode(code)) { status.text = "The pairing code is 6 digits."; return }
        val l = InviteListener()
        invites = l
        if (PlaybackService.running) startService(Intent(this, PlaybackService::class.java).setAction(PlaybackService.ACTION_STOP))
        pairBtn.text = "Cancel"
        status.text = "Looking for a sender showing this code. Open pairing on the computer now (waits about a minute)."
        Thread {
            val r = l.wait(60_000)
            runOnUiThread {
                if (invites !== l) return@runOnUiThread // cancelled
                invites = null
                PlaybackService.userStopped = false
                pairBtn.text = "Pair"
                r.onFailure { status.text = it.message }.onSuccess { sender ->
                    if (sender == null) {
                        status.text = "No sender showing a pairing code was found. On the computer choose Send and Show pairing code, then try again."
                    } else {
                        codeField.setText("")
                        startReceiving(sender.host, sender.port, sender.publicKey.joinToString("") { "%02x".format(it) }, code)
                    }
                }
            }
        }.start()
    }
    override fun onPause() { super.onPause(); source.stop() }

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
        if (text.isNotBlank()) {
            val t = parse(text)
            if (t == null) { status.text = "Could not read the pairing line. It needs host, pk (64 hex) and the 6-digit code from the agent."; return }
            pairing.setText("") // the code is single-use; never keep it around
            startReceiving(t.host, t.port, t.pk, t.code)
        } else {
            val p = store.paired()
            if (p == null) { status.text = "Not paired yet. Type the pairing code above."; return }
            startReceiving(p.host, p.port, p.agentPublic.joinToString("") { "%02x".format(it) }, null)
        }
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
        val head = if (lossless) "lossless PCM to this app, $f" else "encoder Opus $k, $f"
        head + (if (configNote.isNotEmpty()) "  ($configNote)" else "") +
            (if (output.isNotEmpty()) "\nAndroid output: $output" + (if (lossless) " (the app cannot tell whether Android resamples after this)" else "") else "") +
            (if (qualityNote.isNotEmpty()) "\n$qualityNote" else "")
    }

    private fun statsText(): String = with(Stats) {
        "$state\n${encoderText()}\nreceived $received  lost $lost  late $late  reordered $reordered  dup $duplicates\n" +
            "buffer $bufferMs ms (target $targetMs ms)  jitter ${"%.1f".format(jitterMs)} ms\n" +
            "decode ${"%.2f".format(decodeUs / 1000)} ms  output queue ${if (trackQueueMs < 0) "?" else "%.1f".format(trackQueueMs)} ms (buffer $trackBufferMs ms, underruns $trackUnderruns)\n" +
            "trimmed $trimmed  underruns $underruns  resyncs $resyncs  reconnects $reconnects"
    }

    /** One row per paired sender: connect, make it the automatic one, or forget it. Rebuilt only when the list changes. */
    private fun showSenders() {
        val list = store.all()
        val def = store.defaultKey()
        val sig = list.joinToString(";") { "${it.keyHex}${it.host}${it.port}" } + def
        if (sig == sendersShown) return
        sendersShown = sig
        sendersBox.removeAllViews()
        val dp = resources.displayMetrics.density
        if (list.isEmpty()) {
            sendersBox.addView(TextView(this).apply { text = "None yet. Type a pairing code above."; setTextColor(Color.parseColor("#9aa4ae")) })
            return
        }
        for (p in list) {
            sendersBox.addView(TextView(this).apply {
                text = "${p.host}  (key ${p.keyHex.take(8)}…)" + if (p.keyHex == def) "  - default" else ""
                setTextColor(Color.WHITE); setPadding(0, (8 * dp).toInt(), 0, 0)
            })
            val row = LinearLayout(this).apply { orientation = LinearLayout.HORIZONTAL }
            fun btn(t: String, on: () -> Unit) = Button(this).apply { text = t; isAllCaps = false; setOnClickListener { on() } }
                .also { row.addView(it, LinearLayout.LayoutParams(0, -2, 1f)) }
            btn("Connect") { startReceiving(p.host, p.port, p.keyHex, null) }
            if (p.keyHex != def) btn("Make default") { store.setDefault(p.keyHex) }
            btn("Forget") {
                store.remove(p.keyHex)
                if (PlaybackService.running) startService(Intent(this, PlaybackService::class.java).setAction(PlaybackService.ACTION_STOP))
            }
            sendersBox.addView(row)
        }
    }

    private fun tick() {
        val wantIdx = Controls.desiredCodec - 1
        if (quality.getChildAt(1).let { (it as Spinner).selectedItemPosition } != wantIdx) (quality.getChildAt(1) as Spinner).setSelection(wantIdx, false)
        button.text = if (PlaybackService.running) "Disconnect" else "Connect"
        showSenders()
        if (PlaybackService.running || Stats.state.contains("error", ignoreCase = true)) status.text = statsText()
        ui.postDelayed(::tick, 500)
    }
}
