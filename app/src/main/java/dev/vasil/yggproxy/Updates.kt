package dev.vasil.yggproxy

import android.app.Activity
import android.app.AlertDialog
import android.content.Intent
import android.net.Uri
import org.json.JSONObject
import java.net.URL
import javax.net.ssl.HttpsURLConnection

/**
 * New versions from the project's GitHub releases: checked when the app opens (at most every
 * 12 h) or by the button in «Настройки». A newer one is offered with its notes; «Скачать» opens
 * the APK link in the browser and Android installs it over the old one (same signature).
 */
object Updates {
    private const val API = "https://api.github.com/repos/vasil646srb/ratatoskr/releases/latest"
    private const val APK = "ratatoskr-android.apk"
    private const val EVERY_MS = 12 * 3600 * 1000L

    class Release(val version: String, val apkUrl: String?, val page: String, val notes: String)

    @Volatile private var busy = false

    fun current(a: Activity): String = try {
        a.packageManager.getPackageInfo(a.packageName, 0).versionName ?: "0"
    } catch (_: Exception) { "0" }

    /** "0.1.12" > "0.1.9": numbers compared one by one. */
    fun newer(a: String, b: String): Boolean {
        val x = a.trimStart('v').split('.').map { it.takeWhile(Char::isDigit).toIntOrNull() ?: 0 }
        val y = b.trimStart('v').split('.').map { it.takeWhile(Char::isDigit).toIntOrNull() ?: 0 }
        for (i in 0 until maxOf(x.size, y.size)) {
            val d = x.getOrElse(i) { 0 } - y.getOrElse(i) { 0 }
            if (d != 0) return d > 0
        }
        return false
    }

    fun latest(): Release? = try {
        val c = URL(API).openConnection() as HttpsURLConnection
        c.connectTimeout = 15000; c.readTimeout = 20000
        c.setRequestProperty("Accept", "application/vnd.github+json")
        c.setRequestProperty("User-Agent", "Ratatoskr")
        val o = JSONObject(c.inputStream.bufferedReader().readText())
        val assets = o.optJSONArray("assets")
        var apk: String? = null
        if (assets != null) for (i in 0 until assets.length()) {
            val a = assets.getJSONObject(i)
            if (a.optString("name") == APK) apk = a.optString("browser_download_url")
        }
        Release(o.optString("tag_name").trimStart('v'), apk, o.optString("html_url"), o.optString("body"))
    } catch (e: Exception) { null }

    /** Look for a new version; [manual] = the user pressed the button (always asks, says «нет»). */
    fun check(a: Activity, prefs: Prefs, manual: Boolean = false) {
        val now = System.currentTimeMillis()
        if (busy || (!manual && now - prefs.updateCheckedAt < EVERY_MS)) return
        busy = true
        prefs.updateCheckedAt = now
        Thread {
            val r = latest()
            busy = false
            a.runOnUiThread {
                if (a.isFinishing) return@runOnUiThread
                val cur = current(a)
                when {
                    r == null -> if (manual) toast(a, "Не удалось проверить обновления (нет связи с GitHub)")
                    !newer(r.version, cur) -> if (manual) toast(a, "У вас последняя версия ($cur)")
                    !manual && r.version == prefs.updateSkipped -> {}
                    else -> offer(a, prefs, r, cur)
                }
            }
        }.apply { isDaemon = true; start() }
    }

    private fun offer(a: Activity, prefs: Prefs, r: Release, cur: String) {
        val notes = r.notes.substringBefore("## Установка").replace(Regex("[*`#]"), "").trim().take(1500)
        AlertDialog.Builder(a)
            .setTitle("Доступна версия ${r.version}")
            .setMessage("Установлена $cur.\n\n$notes")
            .setPositiveButton("Скачать") { _, _ ->
                try { a.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(r.apkUrl ?: r.page))) }
                catch (_: Exception) { toast(a, "Нет браузера: ${r.page}") }
            }
            .setNegativeButton("Позже", null)
            .setNeutralButton("Пропустить версию") { _, _ -> prefs.updateSkipped = r.version }
            .show()
    }

    private fun toast(a: Activity, s: String) = android.widget.Toast.makeText(a, s, android.widget.Toast.LENGTH_LONG).show()
}
