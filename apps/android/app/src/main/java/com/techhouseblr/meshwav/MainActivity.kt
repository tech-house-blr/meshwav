package com.techhouseblr.meshwav

import android.Manifest
import android.app.Activity
import android.content.Intent
import android.graphics.Color
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
    private lateinit var action: Button
    private lateinit var playHereBtn: Button
    private var remoteVolTouched = 0L
    private lateinit var remoteVolLabel: TextView
    private lateinit var remoteVolBar: android.widget.SeekBar
    private lateinit var status: TextView
    private lateinit var title: TextView
    private lateinit var subtitle: TextView
    private lateinit var statsView: TextView
    private val ui = Handler(Looper.getMainLooper())

    private lateinit var store: PairingStore
    private lateinit var sendersBox: LinearLayout
    private lateinit var sendersLabel: TextView
    private lateinit var pairBox: LinearLayout
    private lateinit var pairToggle: TextView
    private lateinit var recvMain: LinearLayout
    private lateinit var recvDetail: LinearLayout
    private lateinit var quality: LinearLayout
    private var sendersShown = ""
    private var selected: String? = null
    private var tab = 0

    private val dp get() = resources.displayMetrics.density
    private fun px(v: Int) = (v * dp).toInt()

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
        val prefs = getSharedPreferences("mw", MODE_PRIVATE)
        prefs.edit().remove("pairing").apply() // old shared-key line is obsolete
        val pad = px(20)
        val real = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL; setPadding(pad, pad * 2, pad, pad); setBackgroundColor(Ui.BG) }
        real.addView(Ui.text(this, "Meshwav", 26f, Ui.TEXT, bold = true))

        val tabs = LinearLayout(this).apply { orientation = LinearLayout.HORIZONTAL; setPadding(0, px(12), 0, px(12)) }
        for ((i, name) in listOf("Receive", "Send", "Settings").withIndex()) {
            val btn = Ui.button(this, name) { setTab(i) }
            tabBtns.add(btn)
            tabs.addView(btn, LinearLayout.LayoutParams(0, -2, 1f).apply { if (i > 0) leftMargin = px(8) })
        }
        real.addView(tabs)
        lockNote = Ui.text(this, "", 13f, Ui.MUTED).apply { visibility = View.GONE; setPadding(0, 0, 0, px(8)) }
        real.addView(lockNote)

        source = SourcePanel(this)
        recvBox = buildReceive()
        settingsBox = buildSettings(prefs)
        real.addView(recvBox)
        real.addView(source.view.also { it.visibility = View.GONE })
        real.addView(settingsBox.also { it.visibility = View.GONE })
        setContentView(android.widget.ScrollView(this).apply { setBackgroundColor(Ui.BG); isFillViewport = true; addView(real) })
        setTab(prefs.getInt("tab", if (prefs.getBoolean("send_mode", false)) 1 else 0))
        CrashLog.last(this)?.let { status.text = "Last crash:\n$it" }
        requestPermissions(if (Build.VERSION.SDK_INT >= 33) arrayOf(Manifest.permission.POST_NOTIFICATIONS, Manifest.permission.RECORD_AUDIO) else arrayOf(Manifest.permission.RECORD_AUDIO), 1)
        handleIntent(intent)
        tick()
    }

    private lateinit var source: SourcePanel
    private lateinit var recvBox: View
    private lateinit var settingsBox: View
    private val tabBtns = ArrayList<Button>()
    private lateinit var lockNote: TextView

    /** Receiving and sending exclude each other: the other tab stays disabled until this side is stopped. */
    private fun updateTabLocks() {
        val sending = SpikeService.link.isNotEmpty()
        val receiving = PlaybackService.running
        val lockRecv = sending && tab != 0
        val lockSend = receiving && tab != 1
        tabBtns[0].isEnabled = !lockRecv; tabBtns[0].alpha = if (lockRecv) 0.4f else 1f
        tabBtns[1].isEnabled = !lockSend; tabBtns[1].alpha = if (lockSend) 0.4f else 1f
        lockNote.text = when {
            lockRecv -> "Stop sending to use Receive."
            lockSend -> "Stop receiving to use Send."
            else -> ""
        }
        lockNote.visibility = if (lockNote.text.isEmpty()) View.GONE else View.VISIBLE
    }

    private fun setTab(want: Int) {
        // Refuse a tab whose other side is busy (also when restoring the last tab at start).
        val i = when {
            want == 0 && SpikeService.link.isNotEmpty() -> 1
            want == 1 && PlaybackService.running -> 0
            else -> want
        }
        tab = i
        getSharedPreferences("mw", MODE_PRIVATE).edit().putInt("tab", i).putBoolean("send_mode", i == 1).apply()
        recvBox.visibility = if (i == 0) View.VISIBLE else View.GONE
        source.view.visibility = if (i == 1) View.VISIBLE else View.GONE
        settingsBox.visibility = if (i == 2) View.VISIBLE else View.GONE
        for ((n, b) in tabBtns.withIndex()) Ui.restyle(b, n == i)
        if (i == 1) source.start() else { source.stop(); if (i == 0) autoConnect() }
    }

    /** What the Receive tab shows: a status, the senders, and one button. The pairing steps appear when there is no sender. */
    private fun buildReceive(): View {
        val root = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        recvMain = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        title = Ui.text(this, "", 40f, Ui.TEXT, bold = true)
        subtitle = Ui.text(this, "", 16f, Ui.MUTED).apply { setPadding(0, px(2), 0, px(8)) }
        status = Ui.text(this, "", 14f, Ui.WARN)
        recvMain.addView(title); recvMain.addView(subtitle); recvMain.addView(status)
        action = Ui.button(this, "Start receiving", true) { toggle() }
        recvMain.addView(action, LinearLayout.LayoutParams(-1, -2).apply { topMargin = px(12) })
        playHereBtn = Ui.button(this, "Play here", true) { PlaybackService.playHere?.invoke() }
        recvMain.addView(playHereBtn, LinearLayout.LayoutParams(-1, -2).apply { topMargin = px(8) })
        remoteVolLabel = Ui.text(this, "", 14f, Ui.MUTED).apply { setPadding(0, px(16), 0, px(4)) }
        remoteVolBar = android.widget.SeekBar(this).apply {
            max = 100
            setOnSeekBarChangeListener(object : android.widget.SeekBar.OnSeekBarChangeListener {
                override fun onProgressChanged(s: android.widget.SeekBar, p: Int, fromUser: Boolean) {
                    if (fromUser) { remoteVolTouched = android.os.SystemClock.elapsedRealtime(); PlaybackService.setRemoteVolume?.invoke(p); remoteVolLabel.text = "Volume on the playing device  $p%" }
                }
                override fun onStartTrackingTouch(s: android.widget.SeekBar) { remoteVolTouched = android.os.SystemClock.elapsedRealtime() }
                override fun onStopTrackingTouch(s: android.widget.SeekBar) { remoteVolTouched = android.os.SystemClock.elapsedRealtime() }
            })
        }
        recvMain.addView(remoteVolLabel); recvMain.addView(remoteVolBar)
        sendersLabel = Ui.section(this, "Senders")
        recvMain.addView(sendersLabel)
        sendersBox = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        recvMain.addView(sendersBox)
        pairToggle = Ui.link(this, "Pair another sender") { pairBox.visibility = if (pairBox.visibility == View.GONE) View.VISIBLE else View.GONE }
        recvMain.addView(pairToggle)

        pairBox = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        pairBox.addView(Ui.text(this, "Pair a sender", 20f, Ui.TEXT, bold = true).apply { setPadding(0, px(6), 0, px(6)) })
        for ((n, t) in listOf(
            "1" to "On the sender, open Send and choose Show pairing code. On a computer, run meshwav pair.",
            "2" to "Type the 6-digit code here and tap Pair.",
            "3" to "Check the key fingerprint shown here against the sender's, then confirm.",
        )) {
            pairBox.addView(LinearLayout(this).apply {
                orientation = LinearLayout.HORIZONTAL; setPadding(0, px(4), 0, px(4))
                addView(Ui.text(this@MainActivity, n, 18f, Ui.ACCENT, bold = true), LinearLayout.LayoutParams(px(24), -2))
                addView(Ui.text(this@MainActivity, t, 15f, Ui.TEXT), LinearLayout.LayoutParams(0, -2, 1f))
            })
        }
        codeField = EditText(this).apply {
            hint = "123456"; setHintTextColor(Ui.LINE); setTextColor(Ui.TEXT); textSize = 32f; typeface = android.graphics.Typeface.MONOSPACE
            inputType = InputType.TYPE_CLASS_NUMBER; filters = arrayOf(android.text.InputFilter.LengthFilter(6)); letterSpacing = 0.3f
            backgroundTintList = android.content.res.ColorStateList.valueOf(Ui.ACCENT)
        }
        pairBox.addView(codeField, LinearLayout.LayoutParams(-1, -2).apply { topMargin = px(10) })
        pairBtn = Ui.button(this, "Pair", true) { pairByCode() }
        pairBox.addView(pairBtn, LinearLayout.LayoutParams(-1, -2).apply { topMargin = px(8) })
        pairing = EditText(this).apply {
            hint = "meshwav://pair?host=...&pk=...&code=123456"; setHintTextColor(Ui.MUTED); setTextColor(Ui.TEXT)
            inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_FLAG_MULTI_LINE; minLines = 3; visibility = View.GONE
        }
        pairBox.addView(Ui.link(this, "Use a pairing link instead") { pairing.visibility = if (pairing.visibility == View.GONE) View.VISIBLE else View.GONE })
        pairBox.addView(pairing)
        recvMain.addView(pairBox)

        recvDetail = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL; visibility = View.GONE }
        root.addView(recvMain); root.addView(recvDetail)
        return root
    }

    private fun buildSettings(prefs: android.content.SharedPreferences): View {
        val root = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        fun box(text: String, note: String, on: Boolean, set: (Boolean) -> Unit) {
            root.addView(android.widget.CheckBox(this).apply {
                this.text = text; isChecked = on; Ui.tint(this); textSize = 16f
                setOnCheckedChangeListener { _, v -> set(v) }
            })
            root.addView(Ui.text(this, note, 12f).apply { setPadding(px(32), 0, 0, px(8)) })
        }
        root.addView(Ui.section(this, "Receiving"))
        box("Reconnect automatically", "Keeps this phone connected to the default sender so it shows as online.", prefs.getBoolean("auto", true)) { prefs.edit().putBoolean("auto", it).apply() }
        Controls.lowLatency = prefs.getBoolean("low_latency", false)
        box("Low latency", "Smaller buffer. May crackle on weak Wi-Fi. Applies on the next start.", Controls.lowLatency) { Controls.lowLatency = it; prefs.edit().putBoolean("low_latency", it).apply() }
        Controls.nativeOutput = prefs.getBoolean("native_output", false)
        box("Native audio output", "AAudio, experimental. Applies on the next start.", Controls.nativeOutput) { Controls.nativeOutput = it; prefs.edit().putBoolean("native_output", it).apply() }
        Controls.desiredKbps = prefs.getInt("kbps", 0).takeIf { it in Controls.kbpsChoices.toList() } ?: 0
        Controls.desiredFrameMs = prefs.getInt("frame_ms", 0).takeIf { it in Controls.frameChoices.toList() } ?: 0
        root.addView(Ui.section(this, "Encoder"))
        root.addView(Ui.text(this, "Applies live. Smaller frames mean lower latency but more packets.", 12f))
        root.addView(choice("Bitrate", listOf("Agent default") + Controls.kbpsChoices.map { "$it kbit/s" }, Controls.kbpsChoices.indexOf(Controls.desiredKbps) + 1) { i ->
            Controls.desiredKbps = if (i == 0) 0 else Controls.kbpsChoices[i - 1]
            prefs.edit().putInt("kbps", Controls.desiredKbps).apply()
        })
        root.addView(choice("Frame size", listOf("Agent default") + Controls.frameChoices.map { "$it ms" }, Controls.frameChoices.indexOf(Controls.desiredFrameMs) + 1) { i ->
            Controls.desiredFrameMs = if (i == 0) 0 else Controls.frameChoices[i - 1]
            prefs.edit().putInt("frame_ms", Controls.desiredFrameMs).apply()
        })
        Controls.desiredCodec = prefs.getInt("codec", 1).takeIf { it == 1 || it == 2 } ?: 1
        root.addView(Ui.text(this, "Lossless sends the PC audio uncompressed, about 1.7 Mbit/s, always in 5 ms packets. It drops back to Opus if the link cannot keep up.", 12f).apply { setPadding(0, px(10), 0, 0) })
        quality = choice("Quality", listOf("Opus (compressed)", "Lossless PCM"), Controls.desiredCodec - 1) { i ->
            Controls.desiredCodec = i + 1
            if (i == 1) Stats.qualityNote = ""
            prefs.edit().putInt("codec", Controls.desiredCodec).apply()
        }
        root.addView(quality)
        root.addView(source.settings)
        root.addView(Ui.section(this, "Details"))
        statsView = Ui.text(this, "", 12f, Ui.TEXT, mono = true).apply { visibility = View.GONE }
        root.addView(Ui.link(this, "Show or hide live statistics") { statsView.visibility = if (statsView.visibility == View.GONE) View.VISIBLE else View.GONE })
        root.addView(statsView)
        return root
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
            val r = l.wait(60_000) { sender -> confirmSender(l, sender) }
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

    @Volatile private var confirmDialog: android.app.AlertDialog? = null

    /**
     * A sender found by code alone is not authenticated: any device on the tailnet can claim to be it.
     * So nothing is sent until the person has compared its key fingerprint with the one on the sender's
     * screen. Blocks the calling (worker) thread until they answer or the pairing is cancelled.
     */
    private fun confirmSender(l: InviteListener, sender: InviteListener.Sender): Boolean {
        val answer = java.util.concurrent.ArrayBlockingQueue<Boolean>(1)
        runOnUiThread {
            if (invites !== l) { answer.offer(false); return@runOnUiThread }
            status.text = "Found a sender. Check its key fingerprint."
            confirmDialog = android.app.AlertDialog.Builder(this)
                .setTitle("Is this your sender?")
                .setMessage("A sender at ${sender.host} answered. Its key fingerprint is\n\n${Handshake.fingerprint(sender.publicKey)}\n\nDoes it match the fingerprint shown on the sender next to the code? If not, another device may be answering. Choose No.")
                .setPositiveButton("Yes, it matches") { _, _ -> answer.offer(true) }
                .setNegativeButton("No") { _, _ -> answer.offer(false) }
                .setOnCancelListener { answer.offer(false) }
                .show()
        }
        while (!l.cancelled) {
            val r = answer.poll(200, java.util.concurrent.TimeUnit.MILLISECONDS)
            if (r != null) { runOnUiThread { confirmDialog = null }; return r }
        }
        runOnUiThread { confirmDialog?.dismiss(); confirmDialog = null }
        return false
    }

    override fun onPause() { super.onPause(); source.stop() }

    override fun onNewIntent(i: Intent) { super.onNewIntent(i); handleIntent(i) }

    private fun handleIntent(i: Intent?) {
        val d = i?.dataString ?: return
        // A link can come from any web page or chat, so it never pairs by itself: it must name a
        // tailnet sender, and the user confirms the sender before the code is used.
        val t = parse(d)
        if (t == null) { status.text = "Could not read the pairing link. It needs host, pk (64 hex) and the 6-digit code from the agent."; return }
        if (!Handshake.tailnetHost(t.host)) { status.text = "Refused: ${t.host} is not a Tailscale address, so this link was not used."; return }
        android.app.AlertDialog.Builder(this)
            .setTitle("Pair with this sender?")
            .setMessage("A pairing link asks this phone to receive audio from ${t.host}.\n\nSender key fingerprint: ${Handshake.fingerprintOfHex(t.pk)}\n\nOnly continue if you started pairing on that computer just now.")
            .setPositiveButton("Pair") { _, _ -> startReceiving(t.host, t.port, t.pk, t.code) }
            .setNegativeButton("Cancel", null)
            .show()
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
            if (!Handshake.tailnetHost(t.host)) { status.text = "Refused: ${t.host} is not a Tailscale address."; return }
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
        row.addView(TextView(this).apply { text = label; setTextColor(Ui.TEXT) }, LinearLayout.LayoutParams(0, -2, 1f))
        val adapter = object : ArrayAdapter<String>(this, android.R.layout.simple_spinner_dropdown_item, items) {
            override fun getView(position: Int, v: View?, parent: ViewGroup): View = super.getView(position, v, parent).also { (it as TextView).setTextColor(Ui.TEXT) }
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

    /** Measured from the agent's capture stamps; the agent's own capture buffer before the stamp is not included. */
    private fun e2eText(): String = with(Stats) {
        if (e2eMs < 0) "end to end: measuring (needs a sender that supports it)\n"
        else "end to end ${"%.0f".format(e2eMs)} ms = network ${"%.0f".format(e2eNetMs)} + buffer ${"%.0f".format(e2eBufMs)} + output ${"%.0f".format(trackQueueMs)}" +
            (if (e2eErrMs >= 0) "  (clock +-${"%.1f".format(e2eErrMs)} ms)" else "") + "\n"
    }

    private fun statsText(): String = with(Stats) {
        "$state\n${encoderText()}\nreceived $received  lost $lost  late $late  reordered $reordered  dup $duplicates\n" +
            "buffer $bufferMs ms (target $targetMs ms)  jitter ${"%.1f".format(jitterMs)} ms\n" +
            "decode ${"%.2f".format(decodeUs / 1000)} ms  output queue ${if (trackQueueMs < 0) "?" else "%.1f".format(trackQueueMs)} ms (buffer $trackBufferMs ms, underruns $trackUnderruns${if (trackStalls >= 0) ", stalls $trackStalls ($trackLateStalls after a late write), writes over 20 ms $trackLateWrites, longest write gap $trackMaxGapMs ms" else ""})\n" +
            e2eText() +
            "trimmed $trimmed  underruns $underruns  resyncs $resyncs  reconnects $reconnects\n" +
            "packets over 20 ms apart $recvLateGaps  longest packet gap $recvMaxGapMs ms"
    }

    private fun activeKey(): String? {
        if (!PlaybackService.running) return null
        val k = Stats.agentKey
        return store.all().firstOrNull { k.isNotEmpty() && it.keyHex.startsWith(k) }?.keyHex ?: store.paired()?.keyHex
    }

    private fun stateWord(): String {
        val s = Stats.state
        return when {
            !PlaybackService.running -> "Stopped"
            s == "Playing" -> "Playing"
            s.startsWith("Standby") -> "Standby"
            s.startsWith("No audio") -> "Waiting for audio"
            s.contains("error", true) || s.startsWith("Refused") -> "Problem"
            else -> "Connecting"
        }
    }

    /** Names and state only. Tapping a row opens its details. Rebuilt only when something shown has changed. */
    private fun showSenders() {
        val list = store.all()
        val def = store.defaultKey()
        val act = activeKey()
        val word = stateWord()
        val sig = list.joinToString(";") { "${it.keyHex}${it.name}" } + def + act + word + selected
        if (sig == sendersShown) return
        sendersShown = sig
        sendersBox.removeAllViews()
        val sel = list.firstOrNull { it.keyHex == selected }
        if (sel == null) selected = null
        recvMain.visibility = if (sel == null) View.VISIBLE else View.GONE
        recvDetail.visibility = if (sel == null) View.GONE else View.VISIBLE
        sendersLabel.visibility = if (list.isEmpty()) View.GONE else View.VISIBLE
        pairToggle.visibility = if (list.isEmpty()) View.GONE else View.VISIBLE
        if (list.isEmpty()) pairBox.visibility = View.VISIBLE
        for (p in list) {
            val on = p.keyHex == act
            sendersBox.addView(Ui.row(this, p.label, if (on) word else if (p.keyHex == def) "Default" else "", on) { selected = p.keyHex; sendersShown = ""; scrollTop() },
                LinearLayout.LayoutParams(-1, -2).apply { bottomMargin = px(8) })
        }
        if (sel != null) showDetail(sel, sel.keyHex == act, word)
    }

    private fun scrollTop() { (recvBox.parent.parent as? android.widget.ScrollView)?.scrollTo(0, 0) }

    private fun kv(box: LinearLayout, k: String, v: String, mono: Boolean = false) {
        box.addView(Ui.text(this, k, 13f, Ui.MUTED).apply { setPadding(0, px(12), 0, 0) })
        box.addView(Ui.text(this, v, 17f, Ui.TEXT, mono = mono))
    }

    private fun showDetail(p: PairingStore.Paired, on: Boolean, word: String) {
        val d = recvDetail
        d.removeAllViews()
        d.addView(Ui.link(this, "< Senders") { selected = null; sendersShown = "" })
        d.addView(Ui.text(this, p.label, 34f, Ui.TEXT, bold = true))
        d.addView(Ui.text(this, if (on) word else "Not in use", 16f, if (on && word == "Playing") Ui.ACCENT else Ui.MUTED))
        kv(d, "Address", "${p.host}:${p.port}", true)
        kv(d, "Key fingerprint", Handshake.fingerprintOfHex(p.keyHex) ?: p.keyHex.take(16), true)
        kv(d, "Default sender", if (p.keyHex == store.defaultKey()) "Yes" else "No")
        if (!on) d.addView(Ui.button(this, "Listen to this sender", true) { startReceiving(p.host, p.port, p.keyHex, null); selected = null; sendersShown = "" }, LinearLayout.LayoutParams(-1, -2).apply { topMargin = px(20) })
        if (p.keyHex != store.defaultKey()) d.addView(Ui.button(this, "Make default") { store.setDefault(p.keyHex); sendersShown = "" }, LinearLayout.LayoutParams(-1, -2).apply { topMargin = px(8) })
        d.addView(Ui.button(this, "Forget sender") {
            android.app.AlertDialog.Builder(this).setTitle("Forget ${p.label}?")
                .setMessage("This phone will stop receiving from it. To use it again, pair it again.")
                .setPositiveButton("Forget") { _, _ ->
                    store.remove(p.keyHex)
                    if (PlaybackService.running && activeKey() == p.keyHex) startService(Intent(this, PlaybackService::class.java).setAction(PlaybackService.ACTION_STOP))
                    selected = null; sendersShown = ""
                }.setNegativeButton("Keep", null).show()
        }.also { it.setTextColor(Ui.BAD) }, LinearLayout.LayoutParams(-1, -2).apply { topMargin = px(8) })
    }

    private fun tick() {
        val wantIdx = Controls.desiredCodec - 1
        if (quality.getChildAt(1).let { (it as Spinner).selectedItemPosition } != wantIdx) (quality.getChildAt(1) as Spinner).setSelection(wantIdx, false)
        val running = PlaybackService.running
        val word = stateWord()
        val act = activeKey()?.let { k -> store.all().firstOrNull { it.keyHex == k } }
        title.text = if (store.all().isEmpty()) "Not paired" else word
        title.setTextColor(when (word) { "Playing" -> Ui.ACCENT; "Problem" -> Ui.BAD; else -> Ui.TEXT })
        subtitle.text = when {
            store.all().isEmpty() -> "Pair a sender to start receiving."
            !running -> "Not receiving."
            word == "Standby" -> if (Stats.canPlayHere) "The sender is playing on another device. Tap Play here to move it to this phone." else "The sender is playing on another device. Choose this one on the sender."
            word == "Waiting for audio" -> "Nothing is playing on the sender."
            else -> "from ${act?.label ?: "Sender"}"
        }
        val showVol = running && word == "Standby" && Stats.remoteVolume >= 0
        remoteVolLabel.visibility = if (showVol) View.VISIBLE else View.GONE
        remoteVolBar.visibility = if (showVol) View.VISIBLE else View.GONE
        if (showVol && android.os.SystemClock.elapsedRealtime() - remoteVolTouched > 800) {
            remoteVolBar.progress = Stats.remoteVolume
            remoteVolLabel.text = "Volume on the playing device  ${Stats.remoteVolume}%"
        }
        playHereBtn.visibility = if (running && word == "Standby" && Stats.canPlayHere) View.VISIBLE else View.GONE
        action.visibility = if (store.all().isEmpty()) View.GONE else View.VISIBLE
        action.text = if (running) "Stop receiving" else "Start receiving"
        Ui.restyle(action, !running)
        showSenders()
        if (Stats.state.contains("error", ignoreCase = true) || Stats.state.startsWith("Refused")) { if (running || status.text.isEmpty()) status.text = Stats.state }
        else if (running && status.text.toString().let { it.startsWith("Last crash") || it == Stats.state }) status.text = ""
        if (statsView.visibility == View.VISIBLE) statsView.text = if (running) statsText() else Stats.state
        updateTabLocks()
        ui.postDelayed(::tick, 500)
    }
}
