package com.techhouseblr.meshwav

import android.content.Context
import android.content.res.ColorStateList
import android.graphics.Color
import android.graphics.Typeface
import android.graphics.drawable.GradientDrawable
import android.view.Gravity
import android.view.View
import android.widget.Button
import android.widget.LinearLayout
import android.widget.TextView

/** The graphite and chartreuse look, shared by every screen. The accent marks the one active thing on a screen. */
object Ui {
    val BG = Color.parseColor("#141618")
    val PANEL = Color.parseColor("#1B1E21")
    val LINE = Color.parseColor("#2A2E33")
    val TEXT = Color.parseColor("#E8EAEC")
    val MUTED = Color.parseColor("#8F979F")
    val ACCENT = Color.parseColor("#C8FF2E")
    val INK = Color.parseColor("#0F1400")
    val BAD = Color.parseColor("#FF6B6B")
    val WARN = Color.parseColor("#E0B050")

    fun dp(c: Context, v: Int) = (v * c.resources.displayMetrics.density).toInt()

    fun text(c: Context, t: String, size: Float = 14f, color: Int = MUTED, bold: Boolean = false, mono: Boolean = false) = TextView(c).apply {
        text = t; textSize = size; setTextColor(color)
        if (bold) typeface = Typeface.DEFAULT_BOLD
        if (mono) typeface = Typeface.MONOSPACE
    }

    fun fill(c: Context, color: Int, line: Int = color, radius: Int = 8) = GradientDrawable().apply {
        setColor(color); setStroke(dp(c, 1), line); cornerRadius = dp(c, radius).toFloat()
    }

    /** A button. [accent] fills it with the accent and dark text; otherwise it is an outlined panel. */
    fun button(c: Context, t: String, accent: Boolean = false, onClick: () -> Unit) = Button(c).apply {
        text = t; isAllCaps = false; textSize = 15f; stateListAnimator = null; minHeight = dp(c, 48); minimumHeight = dp(c, 48)
        setTextColor(if (accent) INK else TEXT)
        background = fill(c, if (accent) ACCENT else PANEL, if (accent) ACCENT else LINE)
        setOnClickListener { onClick() }
    }

    fun restyle(b: Button, accent: Boolean) {
        b.setTextColor(if (accent) INK else TEXT)
        b.background = fill(b.context, if (accent) ACCENT else PANEL, if (accent) ACCENT else LINE)
    }

    /** A quiet text link. */
    fun link(c: Context, t: String, onClick: () -> Unit) = text(c, t, 15f, ACCENT).apply {
        setPadding(0, dp(c, 12), 0, dp(c, 12)); setOnClickListener { onClick() }
    }

    fun section(c: Context, t: String) = text(c, t, 20f, TEXT, bold = true).apply { setPadding(0, dp(c, 22), 0, dp(c, 6)) }

    fun tint(box: android.widget.CheckBox) { box.buttonTintList = ColorStateList.valueOf(ACCENT); box.setTextColor(TEXT) }

    /** One list row: name on the left, state on the right. The active row is marked with the accent. */
    fun row(c: Context, name: String, state: String, active: Boolean, onClick: () -> Unit): LinearLayout = LinearLayout(c).apply {
        orientation = LinearLayout.HORIZONTAL; gravity = Gravity.CENTER_VERTICAL
        setPadding(dp(c, 14), dp(c, 14), dp(c, 14), dp(c, 14))
        background = fill(c, PANEL, if (active) ACCENT else LINE)
        addView(text(c, name, 17f, TEXT, bold = true), LinearLayout.LayoutParams(0, -2, 1f))
        addView(text(c, state, 14f, if (active) ACCENT else MUTED))
        setOnClickListener { onClick() }
    }

    fun gap(c: Context, h: Int): View = View(c).apply { layoutParams = LinearLayout.LayoutParams(-1, dp(c, h)) }
}
