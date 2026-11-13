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
    /** Number of the current connection attempt; a cancelled attempt's thread sees a newer one. */
    @Volatile private var gen = 0
    /** The attempt this thread works for (set when the thread starts). */
    private val myGen = ThreadLocal<Int>()
    /** False on a cancelled attempt's thread: it must not touch the screen or the settings. */
    private fun alive() = myGen.get() == null || myGen.get() == gen

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
            text = "Ключ"; isAllCaps = false; textSize = 15f; setTextColor(ACCENT)
            setOnClickListener { keyDialog() }
        })
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
        root.addView(buildDash())
        stepsBox = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL; setPadding(0, dp(16), 0, dp(16)) }
        scroll = ScrollView(this).apply { addView(stepsBox) }
        root.addView(scroll, LinearLayout.LayoutParams(MATCH_PARENT, 0, 1f))
        setContentView(root)
        if (Build.VERSION.SDK_INT >= 21) window.statusBarColor = ACCENT
        render()
    }

    /** A server's key: scan its QR code, or type the address and the key. */
    private fun keyDialog() {
        val box = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL; setPadding(dp(20), dp(8), dp(20), 0) }
        box.addView(TextView(this).apply {
            text = "Серверы закрыты: нужен ключ от владельца сервера. Отсканируйте его QR-код или введите адрес и ключ."
            setTextColor(Color.DKGRAY)
        })
        val etAddr = android.widget.EditText(this).apply { hint = "адрес сервера 200:… или ссылка"; textSize = 14f }
        val etKey = android.widget.EditText(this).apply { hint = "ключ доступа"; textSize = 14f }
        box.addView(etAddr); box.addView(etKey)
        android.app.AlertDialog.Builder(this).setTitle("Ключ доступа").setView(box)
            .setPositiveButton("Сохранить") { _, _ -> saveKey(etAddr.text.toString(), etKey.text.toString()) }
            .setNeutralButton("Сканировать QR") { _, _ ->
                @Suppress("DEPRECATION")
                startActivityForResult(Intent(this, ScanActivity::class.java), REQ_SCAN)
            }
            .setNegativeButton("Отмена", null).show()
    }

    private fun saveKey(text: String, typedKey: String) {
        val server = Ygg.serverFromText(text) ?: run { toast("Это не адрес сервера Ratatoskr"); return }
        val key = Ygg.keyFromText(text) ?: Ygg.normKey(typedKey).takeIf { it.isNotEmpty() }
        if (key == null) { toast("Нет ключа: попросите у владельца QR-код или ключ"); return }
        prefs.setKey(server, key)
        prefs.addServer(server)
        Ygg.writeMonitors(filesDir, prefs)
        say("Ключ для $server сохранён — нажмите «Подключиться»", Mark.DONE)
        if (prefs.vpnEnabled && flow == null) startFlow { switchFlow(server) }
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
        Updates.check(this, prefs)
    }

    override fun onDestroy() {
        dashExec.shutdownNow()
        super.onDestroy()
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

    // ------------------------------------------------------------------------------- connected panel

    private lateinit var dash: LinearLayout
    private lateinit var tvNet: TextView
    private lateinit var tvExit: TextView
    private lateinit var tvPing: TextView
    private lateinit var tvNow: TextView
    private lateinit var tvAvg: TextView
    private lateinit var tvTotal: TextView

    /** Fresh node data for the panel, polled off the UI thread once a second. */
    @Volatile private var dPeers = -1
    @Volatile private var dRootOk = false
    @Volatile private var dRtt: Long? = null
    @Volatile private var dAvg: Long? = null
    @Volatile private var dExitMs: Long? = null
    @Volatile private var dExitAt = 0L
    @Volatile private var dRttMap: Map<String, org.json.JSONObject> = emptyMap()
    private val dashExec = java.util.concurrent.Executors.newSingleThreadExecutor()
    private val dashBusy = java.util.concurrent.atomic.AtomicBoolean(false)
    /** Tunnel byte counters, one sample a second: (time ms, rx, tx). */
    private val samples = ArrayDeque<LongArray>()

    private fun buildDash(): LinearLayout {
        fun line(size: Float, bold: Boolean = false) = TextView(this).apply {
            textSize = size; setTextColor(Color.BLACK); setPadding(0, dp(5), 0, dp(1))
            if (bold) setTypeface(typeface, Typeface.BOLD)
        }
        dash = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL; setPadding(0, dp(18), 0, 0); visibility = android.view.View.GONE }
        tvNet = line(15f).also { dash.addView(it) }
        tvExit = line(15f).also { dash.addView(it) }
        tvPing = line(22f, true).also { dash.addView(it) }
        tvNow = line(17f).also { dash.addView(it) }
        tvAvg = line(15f).also { dash.addView(it) }
        tvTotal = line(13.5f).also { it.setTextColor(Color.DKGRAY); dash.addView(it) }
        dash.addView(Button(this).apply {
            text = "Сменить сервер"; isAllCaps = false; textSize = 16f
            setOnClickListener { chooseServer() }
        }, LinearLayout.LayoutParams(MATCH_PARENT, WRAP_CONTENT).apply { topMargin = dp(18) })
        return dash
    }

    private fun pollDash() {
        if (!dashBusy.compareAndSet(false, true)) return
        dashExec.execute {
            try {
                val st = Ygg.fetchStats()?.optJSONObject("node")
                dRootOk = st != null && st.optString("root") != st.optString("public_key")
                dPeers = Ygg.fetchPeers()?.optJSONArray("peers")?.length() ?: if (st == null) -1 else 0
                val byAddr = Ygg.rttByAddr(Ygg.fetchRtt())
                dRttMap = byAddr
                val cur = byAddr.values.firstOrNull { it.optBoolean("connected") }
                dRtt = cur?.takeIf { !it.isNull("rtt_ms") }?.optLong("rtt_ms")
                dAvg = cur?.takeIf { !it.isNull("avg_ms") }?.optLong("avg_ms")
                // The exit itself answers through the tunnel: checked every 10 s.
                if (System.currentTimeMillis() - dExitAt > 10_000) {
                    dExitMs = Ygg.probeExit()
                    dExitAt = System.currentTimeMillis()
                }
            } finally { dashBusy.set(false) }
        }
    }

    private fun mbit(bytesPerSec: Double) = when {
        bytesPerSec * 8 >= 1_000_000 -> "%.1f Мбит/с".format(bytesPerSec * 8 / 1_000_000)
        else -> "%.0f кбит/с".format(bytesPerSec * 8 / 1000)
    }

    /** The panel shown while connected (instead of the steps). */
    private fun renderDash() {
        pollDash()
        val now = System.currentTimeMillis()
        samples.addLast(longArrayOf(now, State.tunRx, State.tunTx))
        while (samples.size > 61) samples.removeFirst()
        fun rate(back: Int): Pair<Double, Double>? {
            if (samples.size < 2) return null
            val a = samples[maxOf(0, samples.size - 1 - back)]
            val b = samples.last()
            val dt = (b[0] - a[0]) / 1000.0
            if (dt <= 0) return null
            return maxOf(0.0, (b[1] - a[1]) / dt) to maxOf(0.0, (b[2] - a[2]) / dt)
        }
        val peers = dPeers
        tvNet.text = "Сеть Yggdrasil: " + when {
            peers < 0 -> "узел не отвечает ✖"
            peers == 0 -> "нет пиров ✖"
            !dRootOk -> "пиров $peers, ищу сеть…"
            else -> "подключено ✔ · пиров $peers"
        }
        tvExit.text = "Сервер выхода: ${prefs.server}\n" + when {
            dExitAt == 0L -> "проверяю…"
            dExitMs != null -> "на связи ✔ (ответ ${dExitMs} мс)"
            else -> "не отвечает ✖"
        }
        tvPing.text = "Пинг: " + (dRtt?.let { "$it мс" } ?: "—") + (dAvg?.let { "  (средний $it мс)" } ?: "")
        val n = rate(1)
        tvNow.text = "Сейчас: " + (n?.let { "↓ ${mbit(it.first)}   ↑ ${mbit(it.second)}" } ?: "—")
        val m = rate(60)
        tvAvg.text = "В среднем за минуту: " + (m?.let { "↓ ${mbit(it.first)}   ↑ ${mbit(it.second)}" } ?: "—")
        tvTotal.text = "Передано: ↓ ${Ygg.human(State.tunRx)}   ↑ ${Ygg.human(State.tunTx)}"
    }

    /** «Сменить сервер»: automatic (best ping) or one from the list, with the node's ping. */
    private fun chooseServer() {
        val cur = Ygg.serverFromText(prefs.server)
        val ping = HashMap<String, Long>()
        for ((addr, o) in dRttMap) if (!o.isNull("avg_ms")) ping[addr] = o.optLong("avg_ms")
        val all = (State.trackerExits.map { it.first } + prefs.serverList()).mapNotNull { Ygg.serverFromText(it) }.distinct()
        val others = all.filter { it != cur }.sortedBy { ping[it] ?: Long.MAX_VALUE }
        if (others.isEmpty()) { toast("Других серверов пока нет"); return }
        val labels = listOf("Автоматически: лучший по пингу") + others.map { s ->
            (if (prefs.keyFor(s) != null) "🔑 " else if (s in State.trackerPublic) "🌐 " else "🔒 ") + s + "  —  " + (ping[s]?.let { "$it мс" } ?: "нет ответа")
        }
        android.app.AlertDialog.Builder(this)
            .setTitle("Сменить сервер")
            .setItems(labels.toTypedArray()) { _, i ->
                val usable = others.filter { prefs.keyFor(it) != null || it in State.trackerPublic }
                val target = if (i == 0) usable.firstOrNull { ping.containsKey(it) } ?: usable.firstOrNull() ?: run { toast("Нет других серверов с доступом"); return@setItems }
                    else others[i - 1]
                startFlow { switchFlow(target) }
            }
            .setNegativeButton("Отмена", null)
            .show()
    }

    /** Switch the exit without touching the node (it keeps its peers and paths). */
    private fun switchFlow(server: String) {
        headline("Переключаю сервер…")
        say("Переключаю на $server")
        say("Проверяю доступ")
        val acc = Ygg.probeAccess(server, prefs.keyFor(server), ask = false)
        if (acc != "ok") throw Stop("$server: ${Ygg.accessText(acc)}")
        finish("доступ есть")
        if (!useServer(server)) throw Stop("VPN к серверу $server не поднялся")
        waitFor(45) { Ygg.probeExit() } ?: throw Stop("сервер $server не отвечает через туннель")
        finish("на связи")
        say("Проверяю выход в интернет")
        val net = Ygg.checkInternet()
        if (!net.startsWith("OK")) throw Stop("интернет через сервер не открывается: $net")
        finish("работает")
        connectedNow()
    }

    /** Connected: the steps go, the panel takes their place. */
    private fun connectedNow() {
        dExitAt = 0L
        samples.clear()
        ui.post { stepsBox.removeAllViews(); current = null }
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
        val connected = !busy && prefs.vpnEnabled && State.vpnActive
        dash.visibility = if (connected) android.view.View.VISIBLE else android.view.View.GONE
        scroll.visibility = if (connected && stepsBox.childCount == 0) android.view.View.GONE else android.view.View.VISIBLE
        if (busy) return
        if (connected) renderDash()
        tvState.text = when {
            prefs.vpnEnabled && State.vpnActive -> "Подключено ✔"
            prefs.vpnEnabled -> "VPN включается…"
            !State.nodeRunning -> "Не подключено"
            else -> "Не подключено · узел Yggdrasil работает"
        }
    }

    private fun onButton() {
        when {
            flow != null -> cancelFlow()
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
        if (requestCode == REQ_SCAN && resultCode == RESULT_OK) {
            data?.getStringExtra(ScanActivity.EXTRA_TEXT)?.let { saveKey(it, "") }
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

    /**
     * Cancel: back to the start at once — VPN off (the attempt may have turned it on to test
     * servers), steps cleared, «Подключиться». The old thread may still sit in a network call;
     * it is detached and changes nothing any more.
     */
    private fun cancelFlow() {
        val t = flow
        gen++
        flow = null
        t?.interrupt()
        prefs.vpnEnabled = false
        YggService.send(this, YggService.ACTION_RECONCILE)
        stepsBox.removeAllViews()
        current = null
        State.log("simple", "подключение отменено")
        render()
    }

    /** A new step (shown as in progress) or a plain line. Safe from any thread. */
    private fun say(text: String, mark: Mark = Mark.WORK) {
        if (alive()) sayNow(text, mark)
    }

    private fun sayNow(text: String, mark: Mark) = ui.post {
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
    private fun finish(result: String, ok: Boolean = true) {
        if (alive()) finishNow(result, ok)
    }

    private fun finishNow(result: String, ok: Boolean) = ui.post {
        val tv = current ?: return@post
        val m = if (ok) Mark.DONE else Mark.FAIL
        tv.text = "${m.sign} $currentText" + (if (result.isNotEmpty()) " — $result" else "")
        tv.setTextColor(m.color)
        State.log("simple", "$currentText: $result")
        current = null
    }

    private fun headline(t: String) {
        if (alive()) ui.post { tvState.text = t }
    }

    // ------------------------------------------------------------------------------- the connection

    private class Stop(msg: String) : Exception(msg)

    private fun startFlow(body: () -> Unit = { connectFlow() }) {
        if (flow != null) return
        stepsBox.removeAllViews()
        current = null
        val g = ++gen
        val t = Thread {
            myGen.set(g)
            try {
                body()
            } catch (e: Stop) {
                finish(e.message ?: "", ok = false)
                say(e.message ?: "не удалось", Mark.FAIL)
                headline("Не подключено")
            } catch (e: InterruptedException) {
                // cancelled: cancelFlow has already reset the screen
            } catch (e: Exception) {
                finish("ошибка: ${e.message}", ok = false)
                headline("Не подключено")
            } finally {
                if (alive()) {
                    flow = null
                    ui.post { render() }
                }
            }
        }
        flow = t
        render()
        t.isDaemon = true
        t.start()
    }

    private fun pause(ms: Long) {
        if (!alive()) throw InterruptedException()
        Thread.sleep(ms)
        if (!alive()) throw InterruptedException()
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
        val public = HashSet<String>()
        var answered: String? = null
        for (t in prefs.trackerList()) {
            if (!alive()) throw InterruptedException()
            say("спрашиваю трекер $t", Mark.INFO)
            val v = Ygg.fetchTracker(t, "/exits") ?: Ygg.fetchTracker(t, "/exits") ?: continue
            answered = answered ?: t
            Ygg.parseExits(v, exits, public)
            if (exits.isNotEmpty()) break
        }
        if (answered == null) finish("трекеры не ответили", ok = false) else finish("ответил $answered")

        say("Получаю список серверов для выхода")
        prefs.serverList().forEach { exits.putIfAbsent(it, -1) }
        if (exits.isEmpty()) throw Stop("серверов нет: трекеры не ответили, сохранённых серверов нет")
        State.trackerExits = exits.map { it.key to it.value }
        if (answered != null) State.trackerPublic = public
        State.trackerAt = System.currentTimeMillis()
        State.trackerStatus = "серверов ${exits.size}"
        finish("серверов: ${exits.size}")

        // 5. Ping to every server (the node pings everything in its monitor list).
        say("Измеряю пинг до серверов")
        Ygg.writeMonitors(filesDir, prefs)
        val ping = HashMap<String, Long>()
        val deadline = System.currentTimeMillis() + 25_000
        while (System.currentTimeMillis() < deadline) {
            for ((addr, o) in Ygg.rttByAddr(Ygg.fetchRtt())) {
                if (addr in exits && !o.isNull("rtt_ms")) ping[addr] = o.optLong("avg_ms").takeIf { it > 0 } ?: o.optLong("rtt_ms")
            }
            if (ping.size == exits.size) break
            pause(2000)
        }
        val pinged = ping.entries.sortedBy { it.value }
        if (pinged.isEmpty()) throw Stop("ни один сервер не ответил на пинг")
        finish("ответили ${pinged.size} из ${exits.size}")
        for ((a, ms) in pinged) say("$a — $ms мс", Mark.INFO)
        for (a in exits.keys.filter { it !in ping }) say("$a — нет ответа", Mark.INFO)

        // 5b. Access: servers are closed — ours are those with our key and the public ones.
        // Nothing is sent to a server we have no key for (no request behind the user's back).
        say("Проверяю доступ к серверам")
        val access = java.util.concurrent.ConcurrentHashMap<String, String>()
        pinged.take(12).map { (a, _) ->
            Thread { access[a] = Ygg.probeAccess(a, prefs.keyFor(a), ask = false) }.apply { start() }
        }.forEach { it.join(45_000) }
        if (!alive()) throw InterruptedException()
        val ranked = pinged.filter { access[it.key] == "ok" }
        for ((a, code) in access) if (code != "ok" && prefs.keyFor(a) != null) say("$a: ${Ygg.accessText(code)}", Mark.INFO)
        if (ranked.isEmpty()) throw Stop("нет серверов, к которым есть доступ. Серверы закрыты: попросите ключ у владельца сервера (кнопка «Ключ» вверху) или запросите доступ в режиме «Эксперт»")
        finish("доступ есть к ${ranked.size} из ${pinged.size}")

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
        connectedNow()
    }

    /**
     * Make [server] the VPN's exit and wait until the VPN points at it (true). The node is not
     * restarted: it reads the server from a file, keeps its peers and paths, and closes the
     * connections through the previous server, so nothing measured afterwards belongs to it.
     */
    private fun useServer(server: String): Boolean {
        if (!alive()) throw InterruptedException()
        prefs.addServer(server)
        prefs.server = server
        prefs.vpnEnabled = true
        YggService.send(this, YggService.ACTION_RECONCILE)
        if (!alive()) {
            // Cancelled right while switching: leave the VPN off as the cancel did.
            prefs.vpnEnabled = false
            YggService.send(this, YggService.ACTION_RECONCILE)
            throw InterruptedException()
        }
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
        private const val REQ_SCAN = 31
        private val ACCENT = Color.parseColor("#1F6F43")
    }
}
