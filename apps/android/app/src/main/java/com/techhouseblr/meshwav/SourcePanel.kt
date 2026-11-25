package com.techhouseblr.meshwav

import android.app.Activity
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.graphics.Color
import android.graphics.Typeface
import android.graphics.drawable.GradientDrawable
import android.media.projection.MediaProjectionManager
import android.os.Handler
import android.os.Looper
import android.view.Gravity
import android.view.View
import android.widget.Button
import android.widget.LinearLayout
import android.widget.TextView
import android.widget.Toast

/** The Send side of the app: share this phone's playback audio with a paired receiver. */
class SourcePanel(private val a: Activity) {
    companion object {
        const val REQ = 7
        val BG = Color.parseColor("#17191C")
        val PANEL = Color.parseColor("#22262A")
        val TEAL = Color.parseColor("#7FD1AE")
        val TEXT = Color.parseColor("#E6E9EC")
        val MUTED = Color.parseColor("#9AA4AE")
        val LINE = Color.parseColor("#33383D")
    }

    private val dp = a.resources.displayMetrics.density
    private fun px(v: Int) = (v * dp).toInt()
    private val ui = Handler(Looper.getMainLooper())
    private val prefs = a.getSharedPreferences("mw", Context.MODE_PRIVATE)
    private var lossless = prefs.getBoolean("src_lossless", true)

    private lateinit var state: TextView
    private lateinit var detail: TextView
    private lateinit var go: Button
    private lateinit var linkBox: LinearLayout
    private lateinit var link: TextView
    private lateinit var code: TextView
    private lateinit var fingerprint: TextView
    private lateinit var readout: TextView
    private lateinit var losslessBtn: Button
    private lateinit var opusBtn: Button
    @Volatile private var ticking = false

    private fun label(t: String, size: Float = 14f, color: Int = MUTED, bold: Boolean = false) = TextView(a).apply {
        text = t; textSize = size; setTextColor(color); if (bold) typeface = Typeface.DEFAULT_BOLD
    }

    private fun flat(t: String, fill: Int, ink: Int, onClick: () -> Unit) = Button(a).apply {
        text = t; isAllCaps = false; textSize = 15f; setTextColor(ink); stateListAnimator = null
        background = GradientDrawable().apply { setColor(fill); setStroke(px(1), if (fill == PANEL) LINE else fill) }
        setOnClickListener { onClick() }
    }

    private fun style(b: Button, on: Boolean) {
        b.setTextColor(if (on) BG else TEXT)
        (b.background as GradientDrawable).setColor(if (on) TEAL else PANEL)
    }

    val view: View = LinearLayout(a).apply {
        orientation = LinearLayout.VERTICAL
        addView(label("Send this phone's audio", 20f, TEXT, true))
        addView(label("Whatever plays on this phone is sent to a paired receiver. While sending, this phone's media volume is held at 0 so it stays silent; your volume is restored when you stop. Some phones pause the player when the volume reaches 0. If yours does, press play again or use headphones.").apply { setPadding(0, px(4), 0, px(16)) })

        state = label("Not running", 22f, TEXT, true)
        addView(state)
        detail = label("").apply { setPadding(0, px(2), 0, px(14)) }
        addView(detail)

        addView(label("Quality"))
        val q = LinearLayout(a).apply { orientation = LinearLayout.HORIZONTAL; setPadding(0, px(6), 0, px(2)) }
        losslessBtn = flat("Lossless", PANEL, TEXT) { pick(true) }
        opusBtn = flat("Opus", PANEL, TEXT) { pick(false) }
        q.addView(losslessBtn, LinearLayout.LayoutParams(0, px(48), 1f))
        q.addView(opusBtn, LinearLayout.LayoutParams(0, px(48), 1f).apply { leftMargin = px(8) })
        addView(q)
        addView(label("Lossless sends the captured audio uncompressed (about 1.5 Mbit/s). It is exact from the point Android captures it, not necessarily identical to the original file.", 12f).apply { setPadding(0, px(6), 0, px(16)) })

        go = flat("Start", TEAL, BG) { toggle() }
        addView(go, LinearLayout.LayoutParams(-1, px(52)))

        linkBox = LinearLayout(a).apply {
            orientation = LinearLayout.VERTICAL; setPadding(px(14), px(12), px(14), px(12)); visibility = View.GONE
            background = GradientDrawable().apply { setColor(PANEL); setStroke(px(1), LINE) }
        }
        linkBox.addView(label("Pairing code, valid for 10 minutes, one use"))
        code = label("", 30f, TEAL, true).apply { typeface = Typeface.MONOSPACE; setPadding(0, px(2), 0, px(8)) }
        linkBox.addView(code)
        fingerprint = label("", 14f, TEAL).apply { typeface = Typeface.MONOSPACE; setPadding(0, 0, 0, px(8)) }
        linkBox.addView(fingerprint)
        link = label("", 12f, TEXT).apply { typeface = Typeface.MONOSPACE; setTextIsSelectable(true) }
        linkBox.addView(link)
        val row = LinearLayout(a).apply { orientation = LinearLayout.HORIZONTAL; setPadding(0, px(12), 0, 0) }
        row.addView(flat("Copy link", BG, TEXT) { copy() }, LinearLayout.LayoutParams(0, px(44), 1f))
        row.addView(flat("Share link", BG, TEXT) { share() }, LinearLayout.LayoutParams(0, px(44), 1f).apply { leftMargin = px(8) })
        linkBox.addView(row)
        addView(linkBox, LinearLayout.LayoutParams(-1, -2).apply { topMargin = px(16) })
        addView(label("On the receiver, type the code into its pairing field, then confirm the key fingerprint shown above.", 12f).apply { setPadding(0, px(6), 0, 0) }.also { it.tag = "hint" })

        readout = label("", 13f).apply { typeface = Typeface.MONOSPACE; setPadding(0, px(16), 0, 0) }
        addView(readout)
        addView(label("Android asks to share your screen when this starts. That is its only prompt for capturing audio; this app reads no screen content.", 12f).apply { setPadding(0, px(16), 0, 0) })
    }

    init { refreshQuality() }

    private fun running() = SpikeService.link.isNotEmpty()

    private fun pick(l: Boolean) {
        lossless = l
        prefs.edit().putBoolean("src_lossless", l).apply()
        NativeSource.setLossless(l)
        refreshQuality()
    }

    private fun refreshQuality() { style(losslessBtn, lossless); style(opusBtn, !lossless) }

    private fun toggle() {
        if (running()) {
            a.startService(Intent(a, SpikeService::class.java).setAction("stop"))
        } else {
            val mpm = a.getSystemService(Context.MEDIA_PROJECTION_SERVICE) as MediaProjectionManager
            a.startActivityForResult(mpm.createScreenCaptureIntent(), REQ)
        }
    }

    fun onResult(req: Int, res: Int, data: Intent?) {
        if (req != REQ || res != Activity.RESULT_OK || data == null) return
        val code = "%06d".format(java.security.SecureRandom().nextInt(1_000_000))
        a.startForegroundService(Intent(a, SpikeService::class.java).putExtra("code", res).putExtra("data", data).putExtra("pair", code).putExtra("lossless", lossless))
    }

    private fun copy() {
        (a.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager).setPrimaryClip(ClipData.newPlainText("Meshwav pairing link", SpikeService.link))
        Toast.makeText(a, "Link copied", Toast.LENGTH_SHORT).show()
    }

    private fun share() {
        a.startActivity(Intent.createChooser(Intent(Intent.ACTION_SEND).setType("text/plain").putExtra(Intent.EXTRA_TEXT, SpikeService.link), "Share pairing link"))
    }

    private val tick = object : Runnable {
        override fun run() {
            if (!ticking) return
            val on = running()
            val st = NativeSource.stats()
            val connected = on && (st.contains("connected ") || st.contains("paired "))
            state.text = when { !on -> "Not running"; connected -> "Streaming"; else -> "Waiting for a receiver" }
            state.setTextColor(if (connected) TEAL else TEXT)
            detail.text = when {
                !on -> "Tap Start to begin."
                connected -> st.substringAfter("| ").substringBefore(" from")
                else -> "Pair a receiver with the link below."
            }
            go.text = if (on) "Stop" else "Start"
            style(go, true)
            linkBox.visibility = if (on) View.VISIBLE else View.GONE
            if (on) {
                val l = SpikeService.link
                link.text = l
                code.text = l.substringAfter("code=", "").chunked(3).joinToString(" ")
                fingerprint.text = Handshake.fingerprintOfHex(l.substringAfter("pk=", "").substringBefore("&"))?.let { "Key fingerprint $it" } ?: ""
                val s = st.substringBefore(" |").split(" ")
                val sent = s.getOrNull(1) ?: "0"; val dropped = s.getOrNull(3) ?: "0"
                readout.text = "sent $sent   dropped $dropped   level ${SpikeService.peak * 100 / 32768}%"
            } else readout.text = ""
            ui.postDelayed(this, 500)
        }
    }

    fun start() { if (!ticking) { ticking = true; ui.post(tick) } }
    fun stop() { ticking = false; ui.removeCallbacks(tick) }
}
