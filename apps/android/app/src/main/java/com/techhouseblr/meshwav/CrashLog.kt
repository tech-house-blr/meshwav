package com.techhouseblr.meshwav

import android.content.Context

/** Saves the last uncaught exception so the next launch can show it (no adb needed). */
object CrashLog {
    fun install(ctx: Context) {
        val app = ctx.applicationContext
        val prev = Thread.getDefaultUncaughtExceptionHandler()
        Thread.setDefaultUncaughtExceptionHandler { t, e ->
            try {
                val text = "${t.name}: " + android.util.Log.getStackTraceString(e).lines().take(12).joinToString("\n")
                app.getSharedPreferences("mw", Context.MODE_PRIVATE).edit().putString("crash", text).commit()
            } catch (_: Throwable) {}
            prev?.uncaughtException(t, e)
        }
    }

    fun last(ctx: Context): String? = ctx.getSharedPreferences("mw", Context.MODE_PRIVATE).getString("crash", null)
}
