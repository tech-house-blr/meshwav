package com.techhouseblr.meshwav

import android.app.Activity
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.graphics.Typeface
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
    companion object { const val REQ = 7 }

    private fun px(v: Int) = Ui.dp(a, v)
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

    private fun label(t: String, size: Float = 14f, color: Int = Ui.MUTED, bold: Boolean = false) = Ui.text(a, t, size, color, bold)

    /** The sending options, shown in the Settings tab. */
    val settings: View = LinearLayout(a).apply {
        orientation = LinearLayout.VERTICAL
        addView(Ui.section(a, "Sending"))
        addView(label("Quality"))
        val q = LinearLayout(a).apply { orientation = LinearLayout.HORIZONTAL; setPadding(0, px(6), 0, px(2)) }
        losslessBtn = Ui.button(a, "Lossless") { pick(true) }
        opusBtn = Ui.button(a, "Opus") { pick(false) }
        q.addView(losslessBtn, LinearLayout.LayoutParams(0, -2, 1f))
        q.addView(opusBtn, LinearLayout.LayoutParams(0, -2, 1f).apply { leftMargin = px(8) })
        addView(q)
        addView(label("Lossless sends the captured audio uncompressed (about 1.5 Mbit/s). It is exact from the point Android captures it, not necessarily identical to the original file.", 12f).apply { setPadding(0, px(4), 0, 0) })
    }

    val view: View = LinearLayout(a).apply {
        orientation = LinearLayout.VERTICAL
        state = label("Not running", 40f, Ui.TEXT, true)
        addView(state)
        detail = label("", 16f).apply { setPadding(0, px(2), 0, px(16)) }
        addView(detail)

        go = Ui.button(a, "Start sending", true) { toggle() }
        addView(go, LinearLayout.LayoutParams(-1, -2))
        addView(label("Whatever plays on this phone goes to a paired receiver. While sending, this phone's media volume is held at 0 so it stays silent, and it is restored when you stop. If your phone pauses the player at volume 0, press play again or use headphones.", 12f).apply { setPadding(0, px(10), 0, 0) })

        linkBox = LinearLayout(a).apply {
            orientation = LinearLayout.VERTICAL; setPadding(px(14), px(12), px(14), px(12)); visibility = View.GONE
            background = Ui.fill(a, Ui.PANEL, Ui.LINE)
        }
        linkBox.addView(label("Pairing code. Valid for 10 minutes, one use."))
        code = label("", 34f, Ui.ACCENT, true).apply { typeface = Typeface.MONOSPACE; setPadding(0, px(2), 0, px(8)) }
        linkBox.addView(code)
        fingerprint = label("", 14f, Ui.ACCENT).apply { typeface = Typeface.MONOSPACE; setPadding(0, 0, 0, px(8)) }
        linkBox.addView(fingerprint)
        link = label("", 12f, Ui.TEXT).apply { typeface = Typeface.MONOSPACE; setTextIsSelectable(true) }
        linkBox.addView(link)
        val row = LinearLayout(a).apply { orientation = LinearLayout.HORIZONTAL; setPadding(0, px(12), 0, 0) }
        row.addView(Ui.button(a, "Copy link") { copy() }, LinearLayout.LayoutParams(0, -2, 1f))
        row.addView(Ui.button(a, "Share link") { share() }, LinearLayout.LayoutParams(0, -2, 1f).apply { leftMargin = px(8) })
        linkBox.addView(row)
        addView(linkBox, LinearLayout.LayoutParams(-1, -2).apply { topMargin = px(16) })
        addView(label("On the receiver, type the code, then check that its key fingerprint matches the one above.", 12f).apply { setPadding(0, px(6), 0, 0) })

        readout = label("", 13f).apply { typeface = Typeface.MONOSPACE; setPadding(0, px(16), 0, 0) }
        addView(readout)
        addView(label("Android asks to share your screen when this starts. That is its only prompt for capturing audio. This app reads no screen content.", 12f).apply { setPadding(0, px(14), 0, 0) })
    }

    init { refreshQuality() }

    private fun running() = SpikeService.link.isNotEmpty()

    private fun pick(l: Boolean) {
        lossless = l
        prefs.edit().putBoolean("src_lossless", l).apply()
        NativeSource.setLossless(l)
        refreshQuality()
    }

    private fun refreshQuality() { Ui.restyle(losslessBtn, lossless); Ui.restyle(opusBtn, !lossless) }

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
            state.text = when { !on -> "Not sending"; connected -> "Sending"; else -> "Waiting for a receiver" }
            state.setTextColor(if (connected) Ui.ACCENT else Ui.TEXT)
            detail.text = when {
                !on -> "Tap Start sending to begin."
                connected -> st.substringAfter("| ").substringBefore(" from")
                else -> "Pair a receiver with the code below."
            }
            go.text = if (on) "Stop sending" else "Start sending"
            Ui.restyle(go, !on)
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
