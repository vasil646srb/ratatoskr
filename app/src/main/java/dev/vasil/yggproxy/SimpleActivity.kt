package dev.vasil.yggproxy

import android.app.Activity
import android.content.Intent
import android.graphics.Color
import android.graphics.Typeface
import android.net.VpnService
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.view.Gravity
import android.view.ViewGroup.LayoutParams.MATCH_PARENT
import android.view.ViewGroup.LayoutParams.WRAP_CONTENT
import android.widget.Button
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import android.widget.Toast
import java.io.File

/**
 * Simple mode: one button. Connecting walks through every stage on its own and says what it is
 * doing: the Yggdrasil network, the best peer, the trackers' server list, ping to every server,
 * a short speed test of the best ones, the VPN, the best peer towards the chosen server, and a
 * final internet check. Everything else is in the full app («Расширенный режим»).
 */
class SimpleActivity : Activity() {

    private lateinit var prefs: Prefs
    private val ui = Handler(Looper.getMainLooper())
    private lateinit var tvState: TextView
    private lateinit var btn: Button
    private lateinit var stepsBox: LinearLayout
    private lateinit var scroll: ScrollView

    @Volatile private var flow: Thread? = null
    @Volatile private var cancelled = false

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        prefs = Prefs(this)
        if (prefs.advancedMode) {
            openAdvanced()
            return
        }
        val root = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setBackgroundColor(Color.parseColor("#F4F6F4"))
            setPadding(dp(20), dp(12), dp(20), dp(12))
        }
        // Top bar: the name and the way into the full app.
        val top = LinearLayout(this).apply { orientation = LinearLayout.HORIZONTAL; gravity = Gravity.CENTER_VERTICAL }
        top.addView(TextView(this).apply {
            text = "Ratatoskr"; textSize = 26f; setTypeface(typeface, Typeface.BOLD); setTextColor(ACCENT)
        }, LinearLayout.LayoutParams(0, WRAP_CONTENT, 1f))
        top.addView(Button(this, null, android.R.attr.borderlessButtonStyle).apply {
            text = "Эксперт ›"; isAllCaps = false; textSize = 15f; setTextColor(ACCENT)
            setOnClickListener {
                prefs.advancedMode = true
                openAdvanced()
            }
        })
        root.addView(top)
        root.addView(TextView(this).apply {
            text = "Интернет через Yggdrasil"; textSize = 14f; setTextColor(Color.DKGRAY); setPadding(0, 0, 0, dp(16))
        })
        tvState = TextView(this).apply { textSize = 17f; setTextColor(Color.BLACK); setPadding(0, 0, 0, dp(12)) }
        root.addView(tvState)
        btn = Button(this).apply {
            isAllCaps = false; textSize = 20f; setTextColor(Color.WHITE)
            setBackgroundColor(ACCENT)
            setOnClickListener { onButton() }
        }
        root.addView(btn, LinearLayout.LayoutParams(MATCH_PARENT, dp(64)))
        stepsBox = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL; setPadding(0, dp(16), 0, dp(16)) }
        scroll = ScrollView(this).apply { addView(stepsBox) }
        root.addView(scroll, LinearLayout.LayoutParams(MATCH_PARENT, 0, 1f))
        setContentView(root)
        if (Build.VERSION.SDK_INT >= 21) window.statusBarColor = ACCENT
        render()
    }

    private fun openAdvanced() {
        startActivity(Intent(this, MainActivity::class.java))
        finish()
    }

    override fun onStart() {
        super.onStart()
        State.uiShown()
        State.stoppedByUser = false
        try { YggService.send(this, YggService.ACTION_RECONCILE) } catch (e: Exception) { State.log("simple", "сервис не запустился: ${e.message}") }
        ui.post(tick)
    }

    override fun onStop() {
        super.onStop()
        ui.removeCallbacks(tick)
        State.uiHidden()
        YggService.instance?.requestReconcile()
    }

    private val tick = object : Runnable {
        override fun run() {
            if (flow == null) render()
            ui.postDelayed(this, 1000)
        }
    }

    /** Button and headline when no connection is being set up. */
    private fun render() {
        if (!::btn.isInitialized) return
        val busy = flow != null
        btn.text = when {
            busy -> "Отменить"
            prefs.vpnEnabled -> "Отключиться"
            else -> "Подключиться"
        }
        if (busy) return
        tvState.text = when {
            prefs.vpnEnabled && State.vpnActive -> "Подключено: ${prefs.server}\n" +
                (State.serverRtt?.let { "пинг до сервера $it · " } ?: "") + "↓ ${Ygg.human(State.tunRx)}  ↑ ${Ygg.human(State.tunTx)}"
            prefs.vpnEnabled -> "VPN включается…"
            !State.nodeRunning -> "Не подключено"
            else -> "Не подключено · узел Yggdrasil работает"
        }
    }

    private fun onButton() {
        when {
            flow != null -> {
                cancelled = true
                flow?.interrupt()
                say("Отменено", Mark.FAIL)
            }
            prefs.vpnEnabled -> {
                prefs.vpnEnabled = false
                YggService.send(this, YggService.ACTION_RECONCILE)
                stepsBox.removeAllViews()
                say("VPN отключён", Mark.DONE)
                render()
            }
            else -> {
                val i = VpnService.prepare(this)
                @Suppress("DEPRECATION")
                if (i != null) startActivityForResult(i, REQ_VPN) else startFlow()
            }
        }
    }

    @Deprecated("Activity API")
    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        @Suppress("DEPRECATION")
        super.onActivityResult(requestCode, resultCode, data)
        if (requestCode == REQ_VPN) {
            if (resultCode == RESULT_OK) startFlow() else toast("Без разрешения на VPN подключиться нельзя")
        }
    }

    // ------------------------------------------------------------------------------- steps on screen

    private enum class Mark(val sign: String, val color: Int) {
        WORK("…", Color.parseColor("#B07000")),
        DONE("✓", Color.parseColor("#1F6F43")),
        FAIL("✗", Color.parseColor("#B03020")),
        INFO("·", Color.DKGRAY),
    }

    private var current: TextView? = null
    private var currentText = ""

    /** A new step (shown as in progress) or a plain line. Safe from any thread. */
    private fun say(text: String, mark: Mark = Mark.WORK) = ui.post {
        State.log("simple", text)
        val tv = TextView(this).apply {
            textSize = if (mark == Mark.INFO) 13.5f else 15.5f
            setTextColor(mark.color)
            setPadding(if (mark == Mark.INFO) dp(22) else 0, dp(if (mark == Mark.INFO) 1 else 7), 0, dp(1))
            this.text = "${mark.sign} $text"
        }
        stepsBox.addView(tv)
        if (mark == Mark.WORK) { current = tv; currentText = text } else if (mark != Mark.INFO) current = null
        scroll.post { scroll.fullScroll(ScrollView.FOCUS_DOWN) }
    }

    /** Close the current step: ✓ (or ✗) with a short result. */
    private fun finish(result: String, ok: Boolean = true) = ui.post {
        val tv = current ?: return@post
        val m = if (ok) Mark.DONE else Mark.FAIL
        tv.text = "${m.sign} $currentText" + (if (result.isNotEmpty()) " — $result" else "")
        tv.setTextColor(m.color)
        State.log("simple", "$currentText: $result")
        current = null
    }

    private fun headline(t: String) = ui.post { tvState.text = t }

    // ------------------------------------------------------------------------------- the connection

    private class Stop(msg: String) : Exception(msg)

    private fun startFlow() {
        if (flow != null) return
        cancelled = false
        stepsBox.removeAllViews()
        val t = Thread {
            try {
                connectFlow()
            } catch (e: Stop) {
                finish(e.message ?: "", ok = false)
                say(e.message ?: "не удалось", Mark.FAIL)
                headline("Не подключено")
            } catch (e: InterruptedException) {
                headline("Отменено")
            } catch (e: Exception) {
                finish("ошибка: ${e.message}", ok = false)
                headline("Не подключено")
            } finally {
                flow = null
                ui.post { render() }
            }
        }
        flow = t
        render()
        t.isDaemon = true
        t.start()
    }

    private fun pause(ms: Long) {
        if (cancelled) throw InterruptedException()
        Thread.sleep(ms)
        if (cancelled) throw InterruptedException()
    }

    /** Wait up to [secs] for [f] to give a non-null value, polling every second. */
    private fun <T> waitFor(secs: Int, f: () -> T?): T? {
        repeat(secs) {
            f()?.let { return it }
            pause(1000)
        }
        return f()
    }

    private fun peersList(): List<org.json.JSONObject> {
        val a = Ygg.fetchPeers()?.optJSONArray("peers") ?: return emptyList()
        return (0 until a.length()).map { a.getJSONObject(it) }
    }

    private fun host(p: org.json.JSONObject) = p.optString("uri").substringAfter("://").substringBefore('?').ifEmpty { "входящий" }

    private fun connectFlow() {
        headline("Подключаюсь…")
        // 1. The node.
        say("Запускаю узел Yggdrasil")
        val stats = waitFor(40) { Ygg.fetchStats() } ?: throw Stop("узел не запустился (см. логи в расширенном режиме)")
        finish("адрес ${stats.optJSONObject("node")?.optString("address") ?: "?"}")

        // 2. The network.
        say("Жду подключения к сети Yggdrasil")
        val joined = waitFor(90) {
            val n = Ygg.fetchStats()?.optJSONObject("node") ?: return@waitFor null
            val peers = peersList().size
            if (peers > 0 && n.optString("root") != n.optString("public_key")) peers else null
        } ?: throw Stop("нет связи с сетью: ни один пир не отвечает (проверьте интернет)")
        finish("пиров на связи: $joined")

        // 3. Peers.
        say("Ищу лучший пир для подключения")
        pause(3000) // the node measures fresh peers first
        val peers = peersList().filter { !it.isNull("rtt_ms") }.sortedBy { it.optLong("rtt_ms") }
        if (peers.isEmpty()) finish("пинг пиров ещё измеряется") else {
            finish("лучший ${host(peers[0])} (${peers[0].optLong("rtt_ms")} мс)")
            for (p in peers.take(5)) say("${host(p)} — ${p.optLong("rtt_ms")} мс", Mark.INFO)
        }

        // 4. Trackers.
        say("Жду информацию от трекера")
        val exits = LinkedHashMap<String, Long>()
        var answered: String? = null
        for (t in prefs.trackerList()) {
            if (cancelled) throw InterruptedException()
            say("спрашиваю трекер $t", Mark.INFO)
            val v = Ygg.fetchTracker(t, "/exits") ?: Ygg.fetchTracker(t, "/exits") ?: continue
            answered = answered ?: t
            val a = v.optJSONArray("exits") ?: continue
            for (k in 0 until a.length()) {
                val o = a.getJSONObject(k)
                Ygg.serverFromText(o.optString("address"))?.let { exits.putIfAbsent(it, if (o.isNull("rtt_ms")) -1 else o.optLong("rtt_ms")) }
            }
            if (exits.isNotEmpty()) break
        }
        if (answered == null) finish("трекеры не ответили", ok = false) else finish("ответил $answered")

        say("Получаю список серверов для выхода")
        prefs.serverList().forEach { exits.putIfAbsent(it, -1) }
        if (exits.isEmpty()) throw Stop("серверов нет: трекеры не ответили, сохранённых серверов нет")
        State.trackerExits = exits.map { it.key to it.value }
        State.trackerAt = System.currentTimeMillis()
        State.trackerStatus = "серверов ${exits.size}"
        finish("серверов: ${exits.size}")

        // 5. Ping to every server (the node pings everything in its monitor list).
        say("Измеряю пинг до серверов")
        try { File(filesDir, "monitors.txt").writeText(prefs.monitorList().joinToString("\n")) } catch (_: Exception) {}
        val ping = HashMap<String, Long>()
        val deadline = System.currentTimeMillis() + 25_000
        while (System.currentTimeMillis() < deadline) {
            Ygg.fetchRtt()?.optJSONArray("servers")?.let { a ->
                for (k in 0 until a.length()) {
                    val o = a.getJSONObject(k)
                    val addr = Ygg.serverFromText(o.optString("address")) ?: continue
                    if (addr in exits && !o.isNull("rtt_ms")) ping[addr] = o.optLong("avg_ms").takeIf { it > 0 } ?: o.optLong("rtt_ms")
                }
            }
            if (ping.size == exits.size) break
            pause(2000)
        }
        val ranked = ping.entries.sortedBy { it.value }
        if (ranked.isEmpty()) throw Stop("ни один сервер не ответил на пинг")
        finish("ответили ${ranked.size} из ${exits.size}")
        for ((a, ms) in ranked) say("$a — $ms мс", Mark.INFO)
        for (a in exits.keys.filter { it !in ping }) say("$a — нет ответа", Mark.INFO)

        // 6. Speed of the best ones by ping: connect to each briefly and measure.
        say("Ищу лучший сервер для выхода (пинг и скорость)")
        val candidates = ranked.take(3).map { it.key }
        val speed = HashMap<String, Double>()
        for (a in candidates) {
            headline("Проверяю сервер $a")
            say("подключаюсь к $a", Mark.INFO)
            if (!useServer(a)) { say("$a: VPN к нему не поднялся", Mark.INFO); continue }
            val ok = waitFor(45) { Ygg.probeExit() }
            if (ok == null) { say("$a: через туннель не отвечает", Mark.INFO); continue }
            var mbps = 0.0
            Ygg.speedTest(true, 4) { mbps = it }
            speed[a] = mbps
            say("$a: пинг ${ping[a]} мс, приём %.1f Мбит/с".format(mbps), Mark.INFO)
        }
        val best = candidates.maxByOrNull { (speed[it] ?: -1.0) * 1000 - (ping[it] ?: 9999) } ?: candidates.first()
        finish("$best · ${ping[best]} мс" + (speed[best]?.let { " · %.1f Мбит/с".format(it) } ?: ""))

        // 7. The VPN through the chosen server.
        say("Подключаю VPN через $best")
        headline("Подключаю VPN…")
        if (!useServer(best)) throw Stop("VPN не поднялся (см. логи в расширенном режиме)")
        waitFor(45) { Ygg.probeExit() } ?: throw Stop("сервер $best не отвечает через туннель")
        finish("туннель работает")

        // 8. The best peer towards the server: the node swaps peers while the ping is high.
        say("Подбираю пир с меньшим пингом и лучшей скоростью до сервера")
        val swap0 = State.lastSwap
        val until = System.currentTimeMillis() + 25_000
        var lastRtt: String? = null
        while (System.currentTimeMillis() < until) {
            val r = State.serverRtt
            if (r != null && r != lastRtt) { lastRtt = r; headline("Пинг до сервера: $r") }
            if (State.lastSwap != null && State.lastSwap != swap0) say("замена пира: ${State.lastSwap}", Mark.INFO)
            pause(2500)
        }
        val busiest = peersList().maxByOrNull { it.optLong("rx_bps") + it.optLong("tx_bps") }
        finish("пинг до сервера ${State.serverRtt ?: "?"}" + (busiest?.let { " через ${host(it)}" } ?: ""))

        // 9. Internet.
        say("Проверяю выход в интернет")
        val net = Ygg.checkInternet()
        if (!net.startsWith("OK")) throw Stop("интернет через сервер не открывается: $net")
        finish("работает")
        say("Подключено. Можно пользоваться интернетом.", Mark.DONE)
        headline("Подключено: $best\nпинг ${State.serverRtt ?: "${ping[best]} мс"}" + (speed[best]?.let { " · %.1f Мбит/с".format(it) } ?: ""))
    }

    /**
     * Make [server] the VPN's exit and wait until the VPN points at it (true). The node is not
     * restarted: it reads the server from a file, keeps its peers and paths, and closes the
     * connections through the previous server, so nothing measured afterwards belongs to it.
     */
    private fun useServer(server: String): Boolean {
        prefs.addServer(server)
        prefs.server = server
        prefs.vpnEnabled = true
        YggService.send(this, YggService.ACTION_RECONCILE)
        val ok = waitFor(60) {
            val target = State.vpnTarget?.let { Ygg.serverFromText(it) }
            if (target == server && State.vpnActive) true else null
        } != null
        pause(1500) // the node re-reads the server file twice a second
        return ok
    }

    // ------------------------------------------------------------------------------- helpers

    private fun dp(v: Int) = (v * resources.displayMetrics.density).toInt()
    private fun toast(s: String) = Toast.makeText(this, s, Toast.LENGTH_SHORT).show()

    companion object {
        private const val REQ_VPN = 30
        private val ACCENT = Color.parseColor("#1F6F43")
    }
}
