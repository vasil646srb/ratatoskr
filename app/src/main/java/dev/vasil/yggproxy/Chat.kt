package dev.vasil.yggproxy

import android.app.Activity
import android.app.AlertDialog
import android.content.Context
import android.graphics.Color
import android.graphics.drawable.GradientDrawable
import android.os.Handler
import android.os.Looper
import android.view.Gravity
import android.view.ViewGroup.LayoutParams.MATCH_PARENT
import android.view.ViewGroup.LayoutParams.WRAP_CONTENT
import android.widget.Button
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import java.io.File
import java.io.InputStream
import java.net.Inet6Address
import java.net.InetAddress
import java.net.InetSocketAddress
import java.net.Socket
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale

/** One chat message; [owner]: the server's owner wrote it (else the client). */
data class ChatLine(val t: Long, val owner: Boolean, val text: String)

/**
 * Chat between a closed exit's owner and its clients (ygghelper chat.rs).
 * Client side: through the node's SOCKS to the server's port, method 0x80 with `[2, op, …]`.
 * Owner side (this phone is the exit): `libygghelper.so chat FILE …` on our access.json.
 */
object Chat {
    class ChatError(msg: String) : Exception(msg)

    private fun readN(i: InputStream, n: Int): ByteArray {
        val b = ByteArray(n)
        var o = 0
        while (o < n) {
            val r = i.read(b, o, n - o)
            if (r < 0) throw ChatError(tr("сервер закрыл соединение"))
            o += r
        }
        return b
    }

    /** Open the exit's access port through the node and pick method 0x80; then [body] talks. */
    private fun <T> exchange(server: String, body: (java.io.OutputStream, InputStream) -> T): T {
        val a = (InetAddress.getByName(server) as Inet6Address).address
        Socket().use { s ->
            s.connect(InetSocketAddress("127.0.0.1", Ygg.NODE_SOCKS), 1000)
            s.soTimeout = 30000
            val o = s.getOutputStream()
            val i = s.getInputStream()
            o.write(byteArrayOf(5, 1, 0)); readN(i, 2)
            o.write(byteArrayOf(5, 1, 0, 4) + a + byteArrayOf((Ygg.PORT shr 8).toByte(), (Ygg.PORT and 0xff).toByte()))
            if (readN(i, 22)[1].toInt() != 0) throw ChatError(tr("сервер недоступен"))
            o.write(byteArrayOf(5, 1, 0x80.toByte()))
            if (readN(i, 2)[1].toInt() and 0xff != 0x80) throw ChatError(tr("сервер не поддерживает чат"))
            return body(o, i)
        }
    }

    private fun status(i: InputStream) {
        val r = readN(i, 2)
        if (r[0].toInt() != 2) throw ChatError(tr("сервер не поддерживает чат — его нужно обновить"))
        when (r[1].toInt()) {
            0 -> {}
            1 -> throw ChatError(tr("чат открыт тем, у кого есть доступ или запрос доступа"))
            else -> throw ChatError(tr("сообщение слишком длинное или их слишком много — подождите минуту"))
        }
    }

    private fun u16(b: ByteArray, at: Int) = ((b[at].toInt() and 0xff) shl 8) or (b[at + 1].toInt() and 0xff)

    /** Client: the conversation with [server] after [since] (ms). */
    fun sync(server: String, since: Long): List<ChatLine> = exchange(server) { o, i ->
        o.write(byteArrayOf(2, 1) + java.nio.ByteBuffer.allocate(8).putLong(since).array())
        status(i)
        val n = u16(readN(i, 2), 0)
        (0 until n).map {
            val h = readN(i, 11)
            val t = java.nio.ByteBuffer.wrap(h, 0, 8).long
            ChatLine(t, h[8].toInt() == 1, String(readN(i, u16(h, 9)), Charsets.UTF_8))
        }
    }

    /** Client: send [text] to [server]'s owner. */
    fun send(server: String, text: String) {
        val b = text.trim().toByteArray(Charsets.UTF_8)
        if (b.isEmpty()) return
        if (b.size > 1000) throw ChatError(tr("сообщение длиннее 1000 байт"))
        exchange(server) { o, i ->
            o.write(byteArrayOf(2, 2, (b.size shr 8).toByte(), b.size.toByte()) + b)
            status(i)
            readN(i, 8)
        }
    }

    private fun helper(ctx: Context, args: List<String>): String {
        val p = ProcessBuilder(listOf("${ctx.applicationInfo.nativeLibraryDir}/libygghelper.so", "chat", File(ctx.filesDir, "access.json").path) + args)
            .redirectErrorStream(true).start()
        val out = p.inputStream.bufferedReader().readText().trim()
        if (p.waitFor() != 0) throw ChatError(out.removePrefix("ygghelper: ").ifEmpty { tr("ошибка") })
        return out
    }

    /** Owner: conversations as JSON objects `{addr, last: {t, owner, text}, new}` (newest first). */
    fun ownerList(ctx: Context, seen: Long): List<org.json.JSONObject> {
        val a = org.json.JSONArray(helper(ctx, listOf("list", seen.toString())))
        return (0 until a.length()).map { a.getJSONObject(it) }
    }

    fun ownerHistory(ctx: Context, addr: String, since: Long): List<ChatLine> {
        val a = org.json.JSONArray(helper(ctx, listOf("history", addr, since.toString())))
        return (0 until a.length()).map { a.getJSONObject(it).let { l -> ChatLine(l.optLong("t"), l.optBoolean("owner"), l.optString("text")) } }
    }

    fun ownerSend(ctx: Context, addr: String, text: String) {
        helper(ctx, listOf("send", addr, text.trim()))
    }

    private val timeFmt = SimpleDateFormat("dd.MM HH:mm", Locale.US)

    /**
     * The chat window. [mineIsOwner]: this side is the owner (its lines on the right). [load]
     * and [send] run off the UI thread; [seen] gets the newest time shown.
     */
    fun open(act: Activity, title: String, hint: String, mineIsOwner: Boolean,
             load: (Long) -> List<ChatLine>, send: (String) -> Unit, seen: (Long) -> Unit = {}) {
        val ui = Handler(Looper.getMainLooper())
        fun dp(v: Int) = (v * act.resources.displayMetrics.density).toInt()
        val list = LinearLayout(act).apply { orientation = LinearLayout.VERTICAL; setPadding(dp(8), dp(4), dp(8), dp(4)) }
        val scroll = ScrollView(act).apply { addView(list) }
        val status = TextView(act).apply { textSize = 12.5f; setTextColor(Color.DKGRAY); setPadding(dp(12), dp(2), dp(12), dp(2)); text = hint }
        val input = EditText(act).apply {
            this.hint = tr("сообщение"); textSize = 15f; maxLines = 4
            filters = arrayOf(android.text.InputFilter.LengthFilter(500))
        }
        val btn = Button(act).apply { text = tr("Отправить"); isAllCaps = false }
        val bottom = LinearLayout(act).apply {
            orientation = LinearLayout.HORIZONTAL; gravity = Gravity.CENTER_VERTICAL; setPadding(dp(8), 0, dp(8), 0)
            addView(input, LinearLayout.LayoutParams(0, WRAP_CONTENT, 1f))
            addView(btn)
        }
        val root = LinearLayout(act).apply {
            orientation = LinearLayout.VERTICAL
            addView(scroll, LinearLayout.LayoutParams(MATCH_PARENT, dp(360)))
            addView(status)
            addView(bottom)
        }
        var last = 0L
        val shown = HashSet<Pair<Long, Boolean>>()
        fun add(l: ChatLine) {
            if (!shown.add(l.t to l.owner)) return
            val mine = l.owner == mineIsOwner
            val bubble = TextView(act).apply {
                text = l.text + "\n" + timeFmt.format(Date(l.t))
                textSize = 15f; setTextColor(Color.BLACK); setTextIsSelectable(true)
                setPadding(dp(10), dp(6), dp(10), dp(6))
                background = GradientDrawable().apply {
                    cornerRadius = dp(10).toFloat()
                    setColor(if (mine) Color.parseColor("#D6EEDB") else Color.parseColor("#ECECEC"))
                }
            }
            list.addView(bubble, LinearLayout.LayoutParams(WRAP_CONTENT, WRAP_CONTENT).apply {
                gravity = if (mine) Gravity.END else Gravity.START
                topMargin = dp(4); if (mine) leftMargin = dp(40) else rightMargin = dp(40)
            })
            last = maxOf(last, l.t)
        }
        var open = true
        val busy = java.util.concurrent.atomic.AtomicBoolean(false)
        fun refresh() {
            if (!busy.compareAndSet(false, true)) return
            Thread {
                try {
                    val lines = load(last)
                    ui.post {
                        if (!open) return@post
                        val before = shown.size
                        lines.forEach { add(it) }
                        if (shown.size != before) scroll.post { scroll.fullScroll(ScrollView.FOCUS_DOWN) }
                        // The hint only while the conversation is empty; a past error goes too.
                        status.text = if (shown.isEmpty()) hint else ""
                        if (last > 0) seen(last)
                    }
                } catch (e: Exception) {
                    ui.post { if (open) status.text = e.message ?: e.javaClass.simpleName }
                } finally { busy.set(false) }
            }.apply { isDaemon = true; start() }
        }
        val poll = object : Runnable {
            override fun run() { if (open) { refresh(); ui.postDelayed(this, 4000) } }
        }
        btn.setOnClickListener {
            val text = input.text.toString().trim()
            if (text.isEmpty()) return@setOnClickListener
            btn.isEnabled = false
            status.text = tr("отправляю…")
            Thread {
                try {
                    send(text)
                    ui.post { input.setText(""); btn.isEnabled = true; status.text = ""; refresh() }
                } catch (e: Exception) {
                    ui.post { btn.isEnabled = true; status.text = tr("не отправлено: ") + (e.message ?: e.javaClass.simpleName) }
                }
            }.apply { isDaemon = true; start() }
        }
        val d = AlertDialog.Builder(act).setTitle(title).setView(root).setNegativeButton(tr("Закрыть"), null).create()
        d.setOnDismissListener { open = false; ui.removeCallbacks(poll) }
        d.show()
        ui.post(poll)
    }
}
