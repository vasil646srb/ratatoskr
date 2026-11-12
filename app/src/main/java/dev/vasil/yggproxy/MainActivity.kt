package dev.vasil.yggproxy

import android.Manifest
import android.app.Activity
import android.app.AlertDialog
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Intent
import android.content.pm.PackageManager
import android.graphics.Color
import android.graphics.Typeface
import android.net.Uri
import android.net.VpnService
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.PowerManager
import android.provider.Settings
import android.text.InputType
import android.view.Gravity
import android.view.View
import android.view.WindowInsets
import android.view.ViewGroup.LayoutParams.MATCH_PARENT
import android.view.ViewGroup.LayoutParams.WRAP_CONTENT
import android.widget.Button
import android.widget.CheckBox
import android.widget.EditText
import android.widget.FrameLayout
import android.widget.ImageView
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import android.widget.Toast
import org.json.JSONObject
import java.util.concurrent.Executors

class MainActivity : Activity() {

    private val REQ_VPN = 10
    private val REQ_SCAN = 11
    private val REQ_EXPORT = 12
    private lateinit var prefs: Prefs
    private val ui = Handler(Looper.getMainLooper())
    private val bg = Executors.newSingleThreadExecutor()

    private lateinit var content: FrameLayout
    private val tabButtons = mutableListOf<Button>()
    private val pages = mutableListOf<View>()
    private var current = 0

    // Client
    private lateinit var etServer: EditText
    private lateinit var cbUdp: CheckBox
    private lateinit var cbIpv6: CheckBox
    private lateinit var btnVpn: Button
    private lateinit var btnApps: Button
    private lateinit var serversBox: LinearLayout
    private lateinit var tvRoot: TextView
    private var serversSig: String? = null
    @Volatile private var lastRtt: JSONObject? = null
    private lateinit var tvClientState: TextView

    // Server
    private lateinit var etLimTotalDown: EditText
    private lateinit var etLimTotalUp: EditText
    private lateinit var etLimClientDown: EditText
    private lateinit var etLimClientUp: EditText
    private lateinit var tvClients: TextView
    private val clientPrev = HashMap<String, LongArray>() // addr -> (tx, rx, at ms)
    private lateinit var cbLan: CheckBox
    private lateinit var cbAutostart: CheckBox
    private lateinit var btnExit: Button
    private lateinit var tvIdentity: TextView
    private lateinit var ivQr: ImageView
    private lateinit var qrBox: LinearLayout
    private var qrText: String? = null

    // Network
    private lateinit var cbPublic: CheckBox
    private lateinit var etPeers: EditText
    private lateinit var etTarget: EditText
    private lateinit var cbLanDisc: CheckBox
    private lateinit var etLanTarget: EditText
    private lateinit var etRttLimit: EditText
    private lateinit var etSwapInterval: EditText
    private lateinit var etSwapSettle: EditText
    private lateinit var etExtra: EditText
    private lateinit var cbUseTrackers: CheckBox
    private lateinit var cbKeepNode: CheckBox
    private lateinit var etTrackers: EditText
    private lateinit var tvTrackers: TextView
    @Volatile private var trackerBusy = false

    // Status / logs
    private lateinit var tvStatus: TextView
    private lateinit var peersBox: LinearLayout
    private lateinit var tvCheck: TextView
    private lateinit var tvSpeed: TextView
    @Volatile private var speedRunning = false
    private lateinit var tvLogs: TextView
    private lateinit var logScroll: ScrollView
    // Settings page: 0 = network, 1 = logs
    private var settingsSub = 0
    private lateinit var netPage: View
    private lateinit var logsPage: View
    private val subButtons = mutableListOf<Button>()

    @Volatile private var lastStats: JSONObject? = null
    @Volatile private var lastPeers: JSONObject? = null
    @Volatile private var lastProbe: Long? = null
    @Volatile private var probeDone = false

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        prefs = Prefs(this)

        // Content between the system bars; the bars themselves sit on the accent colour.
        // Tabs at the bottom, hidden while the keyboard is open.
        val root = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL; setBackgroundColor(ACCENT) }
        content = FrameLayout(this).apply { setBackgroundColor(Color.parseColor("#F4F6F5")) }
        root.addView(content, LinearLayout.LayoutParams(MATCH_PARENT, 0, 1f))
        val tabs = LinearLayout(this).apply { orientation = LinearLayout.HORIZONTAL; setBackgroundColor(ACCENT) }
        root.addView(tabs, LinearLayout.LayoutParams(MATCH_PARENT, WRAP_CONTENT))
        setContentView(root)
        fitSystemBars(root, tabs)

        pages += scrollPage(buildClient())
        pages += scrollPage(buildServer())
        pages += scrollPage(buildStatus())
        pages += buildSettings()
        listOf("Клиент", "Сервер", "Статус", "Настройки").forEachIndexed { i, t ->
            val b = Button(this, null, android.R.attr.borderlessButtonStyle).apply {
                text = t; setTextColor(Color.WHITE); isAllCaps = false; textSize = 13f
                minHeight = dp(56); minimumHeight = dp(56); setPadding(0, 0, 0, 0)
                setOnClickListener { select(i) }
            }
            tabButtons += b
            tabs.addView(b, LinearLayout.LayoutParams(0, WRAP_CONTENT, 1f))
        }
        pages.forEach { content.addView(it, FrameLayout.LayoutParams(MATCH_PARENT, MATCH_PARENT)) }
        select(if (prefs.serverEnabled && !prefs.vpnEnabled) 1 else 0)
        handleLink(intent)

        if (Build.VERSION.SDK_INT >= 33 && checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED) {
            requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), 1)
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        handleLink(intent)
    }

    /** Edge-to-edge (forced on Android 15): pad the root by the system bars, keyboard included. */
    private fun fitSystemBars(root: View, tabs: View) {
        if (Build.VERSION.SDK_INT < 30) {
            @Suppress("DEPRECATION")
            window.statusBarColor = ACCENT
            @Suppress("DEPRECATION")
            window.navigationBarColor = ACCENT
            return
        }
        window.setDecorFitsSystemWindows(false)
        window.isNavigationBarContrastEnforced = false // no light scrim over the accent tab bar
        root.setOnApplyWindowInsetsListener { v, ins ->
            val bars = ins.getInsets(WindowInsets.Type.systemBars() or WindowInsets.Type.displayCutout())
            val ime = ins.getInsets(WindowInsets.Type.ime())
            tabs.visibility = if (ime.bottom > bars.bottom) View.GONE else View.VISIBLE
            v.setPadding(bars.left, bars.top, bars.right, maxOf(bars.bottom, ime.bottom))
            WindowInsets.CONSUMED
        }
    }

    override fun onStart() {
        super.onStart()
        // The node runs while the app is on screen (pings the servers and the root); this also
        // brings back a service that was killed or stopped by an app update.
        State.uiVisible = true
        State.stoppedByUser = false
        try { YggService.send(this, YggService.ACTION_RECONCILE) } catch (e: Exception) { State.log("ui", "сервис не запустился: ${e.message}") }
    }

    override fun onStop() {
        super.onStop()
        State.uiVisible = false
        YggService.instance?.requestReconcile() // stops the node unless VPN or server is on
    }

    @Deprecated("Activity API")
    override fun onBackPressed() {
        if (current == 3 && settingsSub == 1) { showSettingsSub(0); return }
        if (current != 0) { select(0); return }
        @Suppress("DEPRECATION")
        super.onBackPressed()
    }

    override fun onResume() {
        super.onResume()
        ui.post(tick)
    }

    override fun onPause() {
        super.onPause()
        ui.removeCallbacks(tick)
        saveAll()
    }

    private fun select(i: Int) {
        current = i
        pages.forEachIndexed { j, v -> v.visibility = if (i == j) View.VISIBLE else View.GONE }
        tabButtons.forEachIndexed { j, b ->
            b.alpha = if (i == j) 1f else 0.6f
            b.setTypeface(null, if (i == j) Typeface.BOLD else Typeface.NORMAL)
        }
        refreshNow()
    }

    // --------------------------------------------------------------------------------------- pages

    private fun buildClient(): LinearLayout = page().apply {
        addView(h1("Клиент"))
        addView(bigButton("Сканировать QR сервера") { scanQr() })
        etServer = edit("адрес сервера 200:…", "")
        addView(LinearLayout(this@MainActivity).apply {
            orientation = LinearLayout.HORIZONTAL; gravity = Gravity.CENTER_VERTICAL
            addView(etServer, LinearLayout.LayoutParams(0, WRAP_CONTENT, 1f))
            addView(smallButton("Добавить") { addServerFromInput() })
        })
        addView(h2("Серверы"))
        addView(note("Нажмите на сервер — подключиться к нему, долгое нажатие — меню. Пинг меряет узел Yggdrasil каждые 10 с."))
        serversBox = LinearLayout(this@MainActivity).apply { orientation = LinearLayout.VERTICAL }.also { addView(it) }
        tvRoot = mono("Корень сети: …").also { addView(it) }
        btnVpn = bigButton("") { toggleVpn() }.also { addView(it) }
        btnApps = smallButton("Приложения без VPN") { pickApps() }.also { addView(it) }
        tvClientState = mono("").also { addView(it) }
    }

    private fun buildServer(): LinearLayout = page().apply {
        addView(h1("Сервер выхода"))
        addView(note("Телефон раздаёт свой интернет любому узлу Yggdrasil, который знает адрес, без ключей. Локальные сети и loopback закрыты."))
        tvIdentity = mono("").also { addView(it) }
        qrBox = LinearLayout(this@MainActivity).apply {
            orientation = LinearLayout.VERTICAL; gravity = Gravity.CENTER_HORIZONTAL; visibility = View.GONE
            ivQr = ImageView(this@MainActivity).apply { setBackgroundColor(Color.WHITE) }
            addView(ivQr, LinearLayout.LayoutParams(dp(240), dp(240)).apply { topMargin = dp(8) })
            addView(note("Клиент: «Клиент» → «Сканировать QR сервера». Можно и обычной камерой телефона.").apply { gravity = Gravity.CENTER })
        }
        addView(qrBox)
        addView(row(smallButton("Копировать адрес") { copy(State.address) },
            smallButton("Поделиться") { shareAddress() }))
        addView(h2("Ограничение скорости, Мбит/с (0 — без ограничения)"))
        addView(label("Весь сервер: приём клиентами ↓ и отдача ↑"))
        etLimTotalDown = edit("0", mbitText(prefs.limTotalDown), decimal = true)
        etLimTotalUp = edit("0", mbitText(prefs.limTotalUp), decimal = true)
        addView(row(etLimTotalDown, etLimTotalUp))
        addView(label("Каждый клиент: ↓ и ↑"))
        etLimClientDown = edit("0", mbitText(prefs.limClientDown), decimal = true)
        etLimClientUp = edit("0", mbitText(prefs.limClientUp), decimal = true)
        addView(row(etLimClientDown, etLimClientUp))
        addView(smallButton("Применить ограничения") { applyLimits() })
        addView(h2("Клиенты"))
        tvClients = mono("—").also { addView(it) }
        cbLan = check("Разрешить клиентам доступ в мою LAN", prefs.allowLan).also { addView(it) }
        cbAutostart = check("Автозапуск после перезагрузки", prefs.autostart).also { addView(it) }
        btnExit = bigButton("") { toggleExit() }.also { addView(it) }
        addView(smallButton("Не ограничивать в фоне (батарея)") { askBattery() })
    }

    /** Separate page: network settings and logs, switched at the top. */
    private fun buildSettings(): View {
        val box = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        val sw = LinearLayout(this).apply { orientation = LinearLayout.HORIZONTAL; setPadding(dp(8), dp(4), dp(8), 0) }
        listOf("Сеть", "Логи").forEachIndexed { i, t ->
            val b = Button(this, null, android.R.attr.borderlessButtonStyle).apply {
                text = t; isAllCaps = false; textSize = 15f; setTextColor(ACCENT)
                setOnClickListener { showSettingsSub(i) }
            }
            subButtons += b
            sw.addView(b, LinearLayout.LayoutParams(0, WRAP_CONTENT, 1f))
        }
        box.addView(sw)
        val frame = FrameLayout(this)
        netPage = scrollPage(buildNetwork())
        logsPage = buildLogs()
        frame.addView(netPage, FrameLayout.LayoutParams(MATCH_PARENT, MATCH_PARENT))
        frame.addView(logsPage, FrameLayout.LayoutParams(MATCH_PARENT, MATCH_PARENT))
        box.addView(frame, LinearLayout.LayoutParams(MATCH_PARENT, 0, 1f))
        showSettingsSub(0)
        return box
    }

    private fun showSettingsSub(i: Int) {
        settingsSub = i
        netPage.visibility = if (i == 0) View.VISIBLE else View.GONE
        logsPage.visibility = if (i == 1) View.VISIBLE else View.GONE
        subButtons.forEachIndexed { j, b -> b.alpha = if (i == j) 1f else 0.5f; b.setTypeface(null, if (i == j) Typeface.BOLD else Typeface.NORMAL) }
        if (::tvStatus.isInitialized) refreshNow()
    }

    private fun buildNetwork(): LinearLayout = page().apply {
        addView(h1("VPN-клиент"))
        cbUdp = check("UDP через udpgw (QUIC, игры, звонки)", prefs.udp).also { addView(it) }
        cbIpv6 = check("IPv6-сайты через сервер", prefs.ipv6).also { addView(it) }
        addView(h1("Узел Yggdrasil"))
        cbKeepNode = check("Работать в фоне: пинговать серверы и корень, когда приложение свёрнуто", prefs.keepNode).also { addView(it) }
        addView(note("Без VPN и сервера узел работает в фоне с уведомлением. Выключите, чтобы экономить батарею: тогда он работает только при открытом приложении."))
        addView(h1("Трекеры"))
        addView(note("Трекеры обходят сеть Yggdrasil и собирают работающие серверы выхода. Их список появляется на странице «Клиент»; новые трекеры добавляются сами."))
        cbUseTrackers = check("Получать серверы от трекеров", prefs.useTrackers).also { addView(it) }
        addView(label("Трекеры, по одному в строке (адрес Yggdrasil, порт ${Ygg.TRACKER_PORT})"))
        etTrackers = edit("200:…", prefs.trackers, lines = 3).also { addView(it) }
        tvTrackers = mono("").also { addView(it) }
        addView(smallButton("Обновить список серверов сейчас") { saveAll(); refreshTrackers(force = true) })
        addView(h1("Сеть Yggdrasil"))
        cbPublic = check("Публичный список пиров (publicpeers.neilalexander.dev)", prefs.usePublicPeers).also { addView(it) }
        addView(label("Свои пиры, по одному в строке (tls://host:port, tcp://…, ?key=…)"))
        etPeers = edit("tls://213.21.229.37:443", prefs.customPeers, lines = 4).also { addView(it) }
        addView(label("Сколько пиров держать (1–32)"))
        etTarget = edit("3", prefs.target.toString(), number = true).also { addView(it) }
        cbLanDisc = check("Искать пиры в локальной сети (yggdrasil-go, TLS)", prefs.lanDiscovery).also { addView(it) }
        addView(label("Слотов для LAN-пиров (0 = как выше)"))
        etLanTarget = edit("0", prefs.lanTarget.toString(), number = true).also { addView(it) }
        addView(label("Клиент: менять пиров, пока пинг до сервера выше, мс (10–5000)"))
        etRttLimit = edit("100", prefs.rttLimit.toString(), number = true).also { addView(it) }
        addView(label("Пауза между заменами пиров, с (5–3600)"))
        etSwapInterval = edit("30", prefs.swapInterval.toString(), number = true).also { addView(it) }
        addView(label("Сколько проверять нового пира до решения, с (10–3600)"))
        etSwapSettle = edit("60", prefs.swapSettle.toString(), number = true).also { addView(it) }
        addView(note("Узел пингует сервер каждые 10 с. Пока пинг выше порога, он заменяет самого медленного пира, затем проверяет нового заданное время: если пинг не упал хотя бы на 10 %, следующая замена ждёт вдвое дольше паузы (до 10 мин или до паузы, если она больше)."))
        addView(label("Доп. аргументы tcp_node (для отладки)"))
        etExtra = edit("--quality-interval 120", prefs.extraArgs).also { addView(it) }
        addView(bigButton("Сохранить и применить") {
            saveAll()
            if (State.serviceRunning) YggService.send(this@MainActivity, YggService.ACTION_RECONCILE)
            toast("Сохранено")
        })
        addView(smallButton("Обновить публичный список и перезапустить узел") {
            saveAll()
            if (State.serviceRunning) YggService.send(this@MainActivity, YggService.ACTION_REFRESH_PEERS) else toast("Сервис не запущен")
        })
    }

    private fun buildStatus(): LinearLayout = page().apply {
        addView(h1("Статус"))
        tvStatus = mono("").also { addView(it) }
        addView(smallButton("Копировать адрес") { copy(State.address) })
        addView(h2("Пиры"))
        addView(note("Нажмите на пира — подробности. ★ — через него сейчас идёт больше всего трафика."))
        peersBox = LinearLayout(this@MainActivity).apply { orientation = LinearLayout.VERTICAL }.also { addView(it) }
        addView(h2("Проверка выхода"))
        tvCheck = mono("—").also { addView(it) }
        addView(smallButton("Проверить интернет через сервер") {
            tvCheck.text = "проверяю…"
            bg.execute { val r = Ygg.checkInternet(); ui.post { tvCheck.text = r } }
        })
        addView(h2("Тест скорости до сервера (iperf)"))
        addView(note("Клиент ↔ сервер выхода через Yggdrasil, без интернета: 10 с приём, затем 10 с отдача."))
        tvSpeed = mono("—").also { addView(it) }
        addView(smallButton("Запустить тест скорости") { runSpeedTest() })
    }

    private fun buildLogs(): View {
        val box = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL; setPadding(dp(8), dp(8), dp(8), dp(8)) }
        box.addView(row(smallButton("Копировать") { copy(State.logsText()) },
            smallButton("Экспорт в файл") { exportLogs() },
            smallButton("Очистить") { State.clearLogs(); tvLogs.text = "" }))
        tvLogs = mono("").apply { textSize = 11f; setTextIsSelectable(true) }
        logScroll = ScrollView(this).apply { addView(tvLogs) }
        box.addView(logScroll, LinearLayout.LayoutParams(MATCH_PARENT, 0, 1f))
        return box
    }

    // --------------------------------------------------------------------------------------- actions

    private fun saveAll() {
        if (!::etServer.isInitialized) return
        prefs.udp = cbUdp.isChecked
        prefs.useTrackers = cbUseTrackers.isChecked
        prefs.keepNode = cbKeepNode.isChecked
        prefs.trackers = etTrackers.text.toString()
        prefs.ipv6 = cbIpv6.isChecked
        prefs.limTotalDown = kbit(etLimTotalDown)
        prefs.limTotalUp = kbit(etLimTotalUp)
        prefs.limClientDown = kbit(etLimClientDown)
        prefs.limClientUp = kbit(etLimClientUp)
        prefs.allowLan = cbLan.isChecked
        prefs.autostart = cbAutostart.isChecked
        prefs.usePublicPeers = cbPublic.isChecked
        prefs.customPeers = etPeers.text.toString()
        prefs.target = etTarget.text.toString().toIntOrNull() ?: 3
        prefs.lanDiscovery = cbLanDisc.isChecked
        prefs.lanTarget = etLanTarget.text.toString().toIntOrNull() ?: 0
        prefs.rttLimit = etRttLimit.text.toString().toIntOrNull() ?: 100
        prefs.swapInterval = etSwapInterval.text.toString().toIntOrNull() ?: 30
        prefs.swapSettle = etSwapSettle.text.toString().toIntOrNull() ?: 60
        prefs.extraArgs = etExtra.text.toString()
    }

    private fun toggleVpn() {
        saveAll()
        if (prefs.vpnEnabled) {
            State.log("ui", "кнопка «Отключить VPN»")
            prefs.vpnEnabled = false
            YggService.send(this, YggService.ACTION_RECONCILE)
            return
        }
        if (Ygg.parseServer(prefs.server) == null) {
            // Nothing selected yet: take what was typed, else the first saved server.
            val typed = Ygg.serverFromText(etServer.text.toString())
            val pick = typed ?: prefs.serverList().firstOrNull()
            if (pick == null) { toast("Добавьте сервер: отсканируйте QR или введите адрес 200:…"); return }
            prefs.addServer(pick); prefs.server = pick; writeMonitors()
        }
        val i = VpnService.prepare(this)
        if (i != null) startActivityForResult(i, REQ_VPN) else startVpn()
    }

    private fun startVpn() {
        prefs.vpnEnabled = true
        YggService.send(this, YggService.ACTION_RECONCILE)
        refreshNow()
    }

    @Deprecated("Activity API")
    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        @Suppress("DEPRECATION")
        super.onActivityResult(requestCode, resultCode, data)
        if (requestCode == REQ_VPN) {
            if (resultCode == RESULT_OK) startVpn() else toast("VPN не разрешён")
        }
        if (requestCode == REQ_SCAN && resultCode == RESULT_OK) {
            val text = data?.getStringExtra(ScanActivity.EXTRA_TEXT) ?: return
            val server = Ygg.serverFromText(text)
            if (server == null) toast("Это не адрес сервера YggProxy") else connectTo(server)
        }
        if (requestCode == REQ_EXPORT && resultCode == RESULT_OK) {
            val uri = data?.data ?: return
            try {
                contentResolver.openOutputStream(uri)?.use { it.write(State.logsText().toByteArray()) }
                toast("Логи сохранены")
            } catch (e: Exception) { toast("Не удалось сохранить: ${e.message}") }
        }
    }

    private fun runSpeedTest() {
        if (!prefs.vpnEnabled) { toast("Сначала подключите VPN к серверу"); return }
        if (speedRunning) return
        speedRunning = true
        Thread {
            val done = mutableListOf<String>()
            fun show(extra: String?) = ui.post { tvSpeed.text = (done + listOfNotNull(extra)).joinToString("\n") }
            for (download in listOf(true, false)) {
                val dir = if (download) "↓ приём" else "↑ отдача"
                show("$dir: …")
                done += Ygg.speedTest(download, 10) { show("$dir: %.1f Мбит/с …".format(it)) }
                show(null)
            }
            speedRunning = false
        }.apply { isDaemon = true; start() }
    }

    private fun scanQr() {
        @Suppress("DEPRECATION")
        startActivityForResult(Intent(this, ScanActivity::class.java), REQ_SCAN)
    }

    /** Quick connect: remember the server and bring the VPN up (or switch it to this server). */
    private fun connectTo(server: String) {
        saveAll()
        prefs.addServer(server)
        prefs.server = server
        writeMonitors()
        serversSig = null
        if (prefs.vpnEnabled) {
            YggService.send(this, YggService.ACTION_RECONCILE)
            toast("Переключено на $server")
            refreshNow()
            return
        }
        val i = VpnService.prepare(this)
        @Suppress("DEPRECATION")
        if (i != null) startActivityForResult(i, REQ_VPN) else startVpn()
    }

    /** yggproxy://c?a=…&p=… opened from outside (e.g. the system camera): ask before connecting. */
    private fun handleLink(i: Intent?) {
        val data = i?.data ?: return
        if (i.action != Intent.ACTION_VIEW || data.scheme != "yggproxy") return
        i.data = null // handle once (not again on recreate)
        val server = Ygg.serverFromText(data.toString())
        if (server == null) { toast("Неверная ссылка YggProxy"); return }
        select(0)
        AlertDialog.Builder(this)
            .setTitle("Подключиться к серверу?")
            .setMessage("Весь трафик пойдёт через сервер выхода\n$server")
            .setPositiveButton("Подключить") { _, _ -> connectTo(server) }
            .setNegativeButton("Отмена", null)
            .show()
    }

    /** The node re-reads this ping list on its own (no restart, the VPN stays up). */
    private fun writeMonitors() {
        try { java.io.File(filesDir, "monitors.txt").writeText(prefs.monitorList().joinToString("\n")) } catch (_: Exception) {}
    }

    private fun addServerFromInput() {
        val s = Ygg.serverFromText(etServer.text.toString())
        if (s == null) { toast("Адрес сервера: IPv6 из 200::/7, например 200:…"); return }
        prefs.addServer(s)
        if (Ygg.parseServer(prefs.server) == null) prefs.server = s
        writeMonitors()
        etServer.setText("")
        serversSig = null
        refreshNow()
        toast("Сервер добавлен")
    }

    /**
     * Ask the trackers (through the node's SOCKS) for exit servers and for more trackers.
     * Every 2 min while the app is open; [force] right now.
     */
    private fun refreshTrackers(force: Boolean = false) {
        if (!prefs.useTrackers || trackerBusy) return
        if (!force && System.currentTimeMillis() - State.trackerAt < 120_000) return
        if (!State.nodeRunning) { if (force) toast("Узел ещё не запущен"); return }
        trackerBusy = true
        State.trackerAt = System.currentTimeMillis()
        State.trackerStatus = "запрашиваю…"
        bg.execute {
            val exits = LinkedHashMap<String, Long>()
            val newTrackers = mutableListOf<String>()
            val ok = mutableListOf<String>()
            for (t in prefs.trackerList()) {
                val e = Ygg.fetchTracker(t, "/exits") ?: continue
                ok += t
                e.optJSONArray("exits")?.let { a ->
                    for (k in 0 until a.length()) a.getJSONObject(k).let { o ->
                        Ygg.serverFromText(o.optString("address"))?.let { addr -> exits.putIfAbsent(addr, if (o.isNull("rtt_ms")) -1 else o.optLong("rtt_ms")) }
                    }
                }
                Ygg.fetchTracker(t, "/trackers")?.optJSONArray("trackers")?.let { a ->
                    for (k in 0 until a.length()) Ygg.serverFromText(a.getJSONObject(k).optString("address"))?.let { newTrackers += it }
                }
            }
            // Public peers checked by a tracker, hourly: the node uses them on its next start.
            var peersNote = ""
            if (ok.isNotEmpty() && System.currentTimeMillis() - State.trackerPeersAt > 3_600_000) {
                for (t in ok) {
                    val p = Ygg.fetchTracker(t, "/peers") ?: continue
                    val n = p.optJSONArray("peers")?.length() ?: 0
                    if (n > 0) {
                        java.io.File(filesDir, "tracker-peers.json").writeText(p.toString())
                        State.trackerPeersAt = System.currentTimeMillis()
                        peersNote = ", пиров $n"
                        break
                    }
                }
            }
            val known = prefs.trackerList()
            val add = newTrackers.distinct().filter { it !in known }
            if (add.isNotEmpty()) prefs.trackers = (known + add).joinToString("\n")
            if (ok.isNotEmpty()) State.trackerExits = exits.map { it.key to it.value }.take(50)
            val time = java.text.SimpleDateFormat("HH:mm", java.util.Locale.US).format(java.util.Date())
            State.trackerStatus = if (ok.isEmpty()) "$time: трекеры недоступны"
                else "$time: серверов ${exits.size} от ${ok.size} трекер(ов)$peersNote" + if (add.isNotEmpty()) ", новых трекеров ${add.size}" else ""
            State.log("trackers", State.trackerStatus)
            writeMonitors()
            trackerBusy = false
            ui.post {
                serversSig = null
                if (add.isNotEmpty() && ::etTrackers.isInitialized) etTrackers.setText(prefs.trackers)
                refreshNow()
            }
        }
    }

    private fun serverMenu(addr: String) {
        val items = arrayOf("Подключиться", "Копировать адрес", "Удалить из списка")
        AlertDialog.Builder(this).setTitle(addr).setItems(items) { _, w ->
            when (w) {
                0 -> connectTo(addr)
                1 -> copy(addr)
                2 -> {
                    if (prefs.vpnEnabled && prefs.server == addr) { toast("Сначала отключите VPN от этого сервера"); return@setItems }
                    prefs.removeServer(addr)
                    if (prefs.server == addr) prefs.server = prefs.serverList().firstOrNull() ?: ""
                    writeMonitors(); serversSig = null; refreshNow()
                }
            }
        }.show()
    }

    /** Server list with ping (from tcp_node /api/rtt); the selected one is marked. */
    private fun renderServers() {
        val rtt = lastRtt
        val byAddr = HashMap<String, JSONObject>()
        rtt?.optJSONArray("servers")?.let { a -> for (k in 0 until a.length()) a.getJSONObject(k).let { byAddr[it.optString("address")] = it } }
        val list = prefs.serverList()
        val fromTrackers = State.trackerExits.filter { it.first !in list }
        val sig = list.joinToString() + "|" + fromTrackers.joinToString { it.first } + "|" + prefs.server + "|" + prefs.vpnEnabled + State.vpnActive + "|" +
            list.joinToString { byAddr[it]?.let { o -> o.opt("rtt_ms").toString() + o.opt("avg_ms") } ?: "-" }
        if (sig != serversSig) {
            serversSig = sig
            serversBox.removeAllViews()
            if (list.isEmpty()) serversBox.addView(note("Список пуст: отсканируйте QR сервера или введите адрес."))
            for (a in list) {
                val o = byAddr[a]
                val ping = when {
                    !State.nodeRunning -> "узел не запущен"
                    o == null || o.isNull("rtt_ms") -> "нет ответа"
                    else -> "${o.optLong("rtt_ms")} мс (сред. ${o.optLong("avg_ms")} мс)"
                }
                val sel = a == prefs.server
                val mark = when { sel && prefs.vpnEnabled && State.vpnActive -> "● подключено"; sel && prefs.vpnEnabled -> "◐ подключается"; sel -> "○ выбран"; else -> "" }
                serversBox.addView(mono("$a\n    пинг: $ping  $mark").apply {
                    setTextIsSelectable(false)
                    if (sel) setTypeface(Typeface.MONOSPACE, Typeface.BOLD)
                    setBackgroundResource(android.R.drawable.list_selector_background)
                    setPadding(dp(4), dp(8), dp(4), dp(8))
                    setOnClickListener { connectTo(a) }
                    setOnLongClickListener { serverMenu(a); true }
                })
            }
            if (fromTrackers.isNotEmpty()) {
                serversBox.addView(h2("От трекеров"))
                for ((a, trackerRtt) in fromTrackers) {
                    val o = byAddr[a]
                    val ping = when {
                        o != null && !o.isNull("rtt_ms") -> "${o.optLong("rtt_ms")} мс"
                        trackerRtt >= 0 -> "нет ответа (у трекера $trackerRtt мс)"
                        else -> "нет ответа"
                    }
                    serversBox.addView(mono("$a\n    пинг: $ping").apply {
                        setTextIsSelectable(false)
                        setTextColor(Color.DKGRAY)
                        setBackgroundResource(android.R.drawable.list_selector_background)
                        setPadding(dp(4), dp(8), dp(4), dp(8))
                        setOnClickListener { connectTo(a) }
                        setOnLongClickListener { copy(a); true }
                    })
                }
            }
        }
        if (::tvTrackers.isInitialized) tvTrackers.text = "Трекеры: " + (if (prefs.useTrackers) State.trackerStatus else "выключены")
        val root = rtt?.optJSONObject("root")
        tvRoot.text = "Корень сети: " + when {
            !State.nodeRunning -> "узел не запущен"
            root == null || root.isNull("rtt_ms") -> "нет ответа" + (root?.optString("key")?.take(16)?.let { " ($it…)" } ?: "")
            else -> "${root.optLong("rtt_ms")} мс (сред. ${root.optLong("avg_ms")} мс)"
        }
        btnApps.text = "Приложения без VPN (${prefs.excludedAppList().size})"
    }

    /** Multi-choice list of installed apps whose traffic bypasses the VPN. */
    private fun pickApps() {
        toast("Загружаю список приложений…")
        bg.execute {
            val pm = packageManager
            val excluded = prefs.excludedAppList().toSet()
            val apps = pm.getInstalledApplications(0)
                .filter { it.packageName != packageName && (it.flags and android.content.pm.ApplicationInfo.FLAG_SYSTEM == 0 || pm.getLaunchIntentForPackage(it.packageName) != null) }
                .map { it.packageName to (pm.getApplicationLabel(it).toString()) }
                .sortedWith(compareBy({ it.first !in excluded }, { it.second.lowercase() }))
            ui.post {
                val checked = BooleanArray(apps.size) { apps[it].first in excluded }
                AlertDialog.Builder(this)
                    .setTitle("Не пускать в VPN")
                    .setMultiChoiceItems(apps.map { "${it.second}\n${it.first}" }.toTypedArray(), checked) { _, w, on -> checked[w] = on }
                    .setPositiveButton("Сохранить") { _, _ ->
                        prefs.excludedApps = apps.filterIndexed { i, _ -> checked[i] }.joinToString(",") { it.first }
                        if (prefs.vpnEnabled) YggService.send(this, YggService.ACTION_RECONCILE)
                        serversSig = null; refreshNow()
                        toast("Сохранено: ${prefs.excludedAppList().size}")
                    }
                    .setNegativeButton("Отмена", null)
                    .show()
            }
        }
    }

    private fun exportLogs() {
        val stamp = java.text.SimpleDateFormat("yyyyMMdd-HHmm", java.util.Locale.US).format(java.util.Date())
        val i = Intent(Intent.ACTION_CREATE_DOCUMENT).addCategory(Intent.CATEGORY_OPENABLE).setType("text/plain")
            .putExtra(Intent.EXTRA_TITLE, "ratatoskr-log-$stamp.txt")
        @Suppress("DEPRECATION")
        try { startActivityForResult(i, REQ_EXPORT) } catch (e: Exception) { toast("Нет приложения для сохранения файлов") }
    }

    private fun shareAddress() {
        val addr = State.address ?: run { toast("Пока нет данных"); return }
        val text = "Сервер YggProxy: $addr\n${Ygg.connectLink(addr)}"
        startActivity(Intent.createChooser(Intent(Intent.ACTION_SEND).setType("text/plain").putExtra(Intent.EXTRA_TEXT, text), "Адрес сервера"))
    }

    private fun kbit(e: EditText) = ((e.text.toString().replace(',', '.').toDoubleOrNull() ?: 0.0) * 1000).toInt().coerceAtLeast(0)
    private fun mbitText(kbit: Int) = if (kbit % 1000 == 0) (kbit / 1000).toString() else "%.1f".format(kbit / 1000.0)

    /** Save the limits; a running exit re-reads its limits file within 2 s (no reconnects). */
    private fun applyLimits() {
        saveAll()
        try { java.io.File(filesDir, "limits.txt").writeText(prefs.limitsText()) } catch (_: Exception) {}
        toast(if (prefs.serverEnabled) "Ограничения применены" else "Сохранено")
    }

    private fun toggleExit() {
        saveAll()
        prefs.serverEnabled = !prefs.serverEnabled
        State.log("ui", if (prefs.serverEnabled) "кнопка «Запустить сервер»" else "кнопка «Остановить сервер»")
        YggService.send(this, YggService.ACTION_RECONCILE)
        refreshNow()
        // A server must keep running with the screen off: offer the battery exemption once.
        val pm = getSystemService(POWER_SERVICE) as PowerManager
        if (prefs.serverEnabled && !pm.isIgnoringBatteryOptimizations(packageName)) {
            AlertDialog.Builder(this)
                .setTitle("Работа с выключенным экраном")
                .setMessage("Чтобы сервер не засыпал, разрешите приложению работать в фоне без ограничений батареи.")
                .setPositiveButton("Разрешить") { _, _ -> askBattery() }
                .setNegativeButton("Не сейчас", null)
                .show()
        }
    }

    private fun askBattery() {
        val pm = getSystemService(POWER_SERVICE) as PowerManager
        if (pm.isIgnoringBatteryOptimizations(packageName)) { toast("Уже без ограничений"); return }
        try {
            startActivity(Intent(Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS, Uri.parse("package:$packageName")))
        } catch (e: Exception) {
            startActivity(Intent(Settings.ACTION_IGNORE_BATTERY_OPTIMIZATION_SETTINGS))
        }
    }

    // --------------------------------------------------------------------------------------- refresh

    private val tick = object : Runnable {
        override fun run() {
            refreshNow()
            ui.postDelayed(this, 2000)
        }
    }

    private var probeCounter = 0

    private fun refreshNow() {
        if (!::tvStatus.isInitialized) return
        renderButtons()
        refreshTrackers()
        if (current <= 2) {
            val doProbe = current == 2 && prefs.vpnEnabled && (probeCounter++ % 3 == 0)
            bg.execute {
                lastRtt = Ygg.fetchRtt()
                if (current != 0) {
                    lastStats = Ygg.fetchStats()
                    lastPeers = Ygg.fetchPeers()
                }
                if (doProbe) { lastProbe = Ygg.probeExit(); probeDone = true }
                ui.post { renderServers(); renderStatus() }
            }
        }
        if (current == 3 && settingsSub == 1) {
            val atBottom = !logScroll.canScrollVertically(1)
            tvLogs.text = State.logsText()
            if (atBottom) logScroll.post { logScroll.fullScroll(View.FOCUS_DOWN) }
        }
    }

    private fun renderButtons() {
        renderClients()
        btnVpn.text = if (prefs.vpnEnabled) "Отключить VPN" else "Подключить VPN"
        btnExit.text = if (prefs.serverEnabled) "Остановить сервер" else "Запустить сервер"
        tvClientState.text = when {
            !prefs.vpnEnabled -> "VPN выключен"
            State.vpnActive -> "VPN работает → ${State.vpnTarget}\n↓ ${Ygg.human(State.tunRx)}  ↑ ${Ygg.human(State.tunTx)}"
            else -> "VPN запускается…" + (State.lastError?.let { "\n$it" } ?: "")
        }
        val addr = State.address
        val link = addr?.let { Ygg.connectLink(it) }
        if (link != qrText) {
            qrText = link
            ivQr.setImageBitmap(link?.let { Qr.bitmap(it, dp(240)) })
            qrBox.visibility = if (link == null) View.GONE else View.VISIBLE
        }
        tvIdentity.text = if (addr == null) "Адрес появится после запуска узла."
        else "Адрес для клиентов: $addr" +
            if (prefs.serverEnabled) "\n${if (State.exitActive) "Работает" else "Запускается…"} · соединений ${State.srvConns} (всего ${State.srvTotal}, отказов ${State.srvDenied})\n" +
                "отдано ${Ygg.human(State.srvTx)} · принято ${Ygg.human(State.srvRx)}" else ""
    }

    /** Exit clients seen in the last 10 s, with their current rate. */
    private fun renderClients() {
        if (!prefs.serverEnabled) { tvClients.text = "сервер выключен"; return }
        val now = System.currentTimeMillis()
        val rows = State.clients.entries.filter { now - it.value[3] < 10_000 }.sortedByDescending { it.value[1] + it.value[2] }
        if (rows.isEmpty()) { tvClients.text = "нет подключённых клиентов"; return }
        tvClients.text = rows.joinToString("\n") { (addr, v) ->
            val prev = clientPrev[addr]
            val dt = if (prev != null) (v[3] - prev[2]).coerceAtLeast(1) else 0
            val down = if (prev != null && dt > 0) (v[1] - prev[0]) * 1000 / dt else 0
            val up = if (prev != null && dt > 0) (v[2] - prev[1]) * 1000 / dt else 0
            if (prev == null || prev[2] != v[3]) clientPrev[addr] = longArrayOf(v[1], v[2], v[3])
            "$addr\n  соединений ${v[0]} · ↓ ${Ygg.mbit(down.coerceAtLeast(0))} ↑ ${Ygg.mbit(up.coerceAtLeast(0))} · всего ↓ ${Ygg.human(v[1])} ↑ ${Ygg.human(v[2])}"
        }
    }

    private fun renderStatus() {
        val st = lastStats
        val sb = StringBuilder()
        sb.append("Сервис: ").append(if (State.serviceRunning) "работает" else "остановлен").append('\n')
        sb.append("Узел Yggdrasil: ").append(
            when {
                st != null -> "на связи (аптайм ${Ygg.dur(st.optLong("uptime_s"))})"
                State.nodeRunning -> "запущен, API не отвечает"
                else -> "не запущен"
            }).append('\n')
        if (st != null) {
            val n = st.getJSONObject("node")
            val key = n.optString("public_key")
            val root = n.optString("root")
            val coords = n.optJSONArray("coords")
            val c = (0 until (coords?.length() ?: 0)).joinToString(" ") { coords!!.get(it).toString() }
            val peersUp = n.optInt("peers_up")
            sb.append("Сеть: ").append(
                when {
                    peersUp == 0 -> "нет пиров ✖"
                    root == key -> "только свой корень (сеть не видна) ⚠"
                    else -> "подключено ✔"
                }).append('\n')
            sb.append("Адрес:  ").append(n.optString("address")).append('\n')
            sb.append("Ключ:   ").append(key).append('\n')
            sb.append("Корень: ").append(root.take(16)).append("…\n")
            sb.append("Коорд.: [").append(c).append("]\n")
            lastRtt?.optJSONObject("root")?.let { r ->
                sb.append("Пинг до корня: ").append(if (r.isNull("rtt_ms")) "нет ответа" else "${r.optLong("rtt_ms")} мс (сред. ${r.optLong("avg_ms")} мс)").append('\n')
            }
            sb.append("Поиск в LAN: ").append(
                when {
                    !prefs.lanDiscovery -> "выключен"
                    State.lanStatus == null -> "запускается…"
                    State.lanStatus!!.startsWith("joined") -> "✔ " + State.lanStatus
                    else -> "✖ " + State.lanStatus
                }).append('\n')
            sb.append("Пиров: ").append(peersUp).append(" · сессий ").append(n.optInt("sessions")).append(" · путей ").append(n.optInt("paths")).append('\n')
        } else if (State.address != null) {
            sb.append("Адрес:  ").append(State.address).append('\n')
        }
        sb.append('\n')
        if (prefs.vpnEnabled) {
            sb.append("VPN: ").append(if (State.vpnActive) "работает" else "запускается").append(" → ").append(State.vpnTarget ?: "?").append('\n')
            sb.append("Пинг до сервера: ").append(State.serverRtt ?: "измеряю…").append('\n')
            State.lastSwap?.let { sb.append("Замена пира: ").append(it).append('\n') }
            sb.append("Связь с сервером выхода: ").append(
                when {
                    !probeDone -> "проверяю…"
                    lastProbe != null -> "✔ ${lastProbe} мс"
                    else -> "✖ нет ответа"
                }).append('\n')
            sb.append("Трафик: ↓ ").append(Ygg.human(State.tunRx)).append("  ↑ ").append(Ygg.human(State.tunTx)).append('\n')
        } else sb.append("VPN: выключен\n")
        if (prefs.serverEnabled) {
            sb.append("Сервер выхода: ").append(if (State.exitActive) "работает" else "запускается")
                .append(" · открыт всем узлам Yggdrasil").append('\n')
            sb.append("  соединений ").append(State.srvConns).append(" (всего ").append(State.srvTotal).append(", отказов ").append(State.srvDenied).append(")\n")
            sb.append("  отдано ").append(Ygg.human(State.srvTx)).append(" · принято ").append(Ygg.human(State.srvRx)).append('\n')
        } else sb.append("Сервер выхода: выключен\n")
        State.lastError?.let { sb.append("\nПоследняя ошибка: ").append(it) }
        tvStatus.text = sb.toString()

        renderPeers()
    }

    /** Connected peers with live traffic; the one carrying most of it now is starred. */
    private fun renderPeers() {
        peersBox.removeAllViews()
        val arr = lastPeers?.optJSONArray("peers")
        if (arr == null || arr.length() == 0) {
            peersBox.addView(mono("нет данных"))
            return
        }
        val list = (0 until arr.length()).map { arr.getJSONObject(it) }
        val busiest = list.maxByOrNull { it.optLong("rx_bps") + it.optLong("tx_bps") }
            ?.takeIf { it.optLong("rx_bps") + it.optLong("tx_bps") > 500 }
        lastPeers?.let { t ->
            peersBox.addView(mono("Всего сейчас: ↓ ${Ygg.mbit(t.optLong("rx_bps"))}  ↑ ${Ygg.mbit(t.optLong("tx_bps"))}"))
        }
        for (p in list) {
            val host = p.optString("uri").substringAfter("://").substringBefore('?').ifEmpty { "входящий " + p.optString("address") }
            val rtt = if (p.isNull("rtt_ms")) "" else "  ${p.optLong("rtt_ms")} мс"
            val star = if (p === busiest) "★ " else "  "
            val line = "$star${host}$rtt\n    ↓ ${Ygg.mbit(p.optLong("rx_bps"))}  ↑ ${Ygg.mbit(p.optLong("tx_bps"))}  · ${p.optInt("share")} %"
            peersBox.addView(mono(line).apply {
                setTextIsSelectable(false)
                setBackgroundResource(android.R.drawable.list_selector_background)
                setPadding(dp(4), dp(8), dp(4), dp(8))
                setOnClickListener { showPeer(p) }
            })
        }
    }

    private fun showPeer(p: JSONObject) {
        val text = buildString {
            append("Адрес пира: ").append(p.optString("uri").ifEmpty { "входящее соединение" }).append("\n\n")
            append("Направление: ").append(if (p.optString("dir") == "out") "исходящий" else "входящий")
            if (p.optBoolean("lan")) append(" · найден в LAN")
            append("\nПинг: ").append(if (p.isNull("rtt_ms")) "—" else "${p.optLong("rtt_ms")} мс")
            append("\nСессия: ").append(Ygg.dur(p.optLong("up_s")))
            append("\n\nСейчас: ↓ ").append(Ygg.mbit(p.optLong("rx_bps"))).append("  ↑ ").append(Ygg.mbit(p.optLong("tx_bps")))
            append("\nДоля трафика узла: ").append(p.optInt("share")).append(" %")
            append("\nЗа сессию: ↓ ").append(Ygg.human(p.optLong("rx"))).append("  ↑ ").append(Ygg.human(p.optLong("tx")))
            append("\n\nYggdrasil-адрес пира:\n").append(p.optString("address"))
            append("\n\nКлюч:\n").append(p.optString("key"))
        }
        AlertDialog.Builder(this)
            .setTitle("Пир")
            .setMessage(text)
            .setPositiveButton("Закрыть", null)
            .setNeutralButton("Копировать") { _, _ -> copy(p.optString("uri").ifEmpty { p.optString("address") }) }
            .show()
    }

    // --------------------------------------------------------------------------------------- widgets

    private fun dp(v: Int) = (v * resources.displayMetrics.density).toInt()

    private fun page() = LinearLayout(this).apply {
        orientation = LinearLayout.VERTICAL
        setPadding(dp(16), dp(12), dp(16), dp(24))
    }

    private fun scrollPage(v: View) = ScrollView(this).apply { isFillViewport = true; addView(v) }

    private fun h1(t: String) = TextView(this).apply { text = t; textSize = 20f; setTypeface(typeface, Typeface.BOLD); setTextColor(ACCENT); setPadding(0, dp(4), 0, dp(6)) }
    private fun h2(t: String) = TextView(this).apply { text = t; textSize = 16f; setTypeface(typeface, Typeface.BOLD); setTextColor(ACCENT); setPadding(0, dp(14), 0, dp(4)) }
    private fun note(t: String) = TextView(this).apply { text = t; textSize = 13f; setTextColor(Color.DKGRAY); setPadding(0, 0, 0, dp(8)) }
    private fun label(t: String) = TextView(this).apply { text = t; textSize = 13f; setTextColor(Color.GRAY); setPadding(0, dp(10), 0, 0) }
    private fun mono(t: String) = TextView(this).apply { text = t; textSize = 12.5f; typeface = Typeface.MONOSPACE; setTextColor(Color.BLACK); setTextIsSelectable(true); setPadding(0, dp(6), 0, dp(6)) }

    private fun edit(hint: String, value: String, number: Boolean = false, lines: Int = 1, decimal: Boolean = false) = EditText(this).apply {
        this.hint = hint
        setText(value)
        textSize = 14f
        if (decimal) inputType = InputType.TYPE_CLASS_NUMBER or InputType.TYPE_NUMBER_FLAG_DECIMAL
        else if (number) inputType = InputType.TYPE_CLASS_NUMBER
        else if (lines > 1) {
            inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_FLAG_MULTI_LINE or InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS
            minLines = lines; gravity = Gravity.TOP or Gravity.START; typeface = Typeface.MONOSPACE
        } else inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS
    }

    private fun check(t: String, v: Boolean) = CheckBox(this).apply { text = t; isChecked = v; textSize = 14f }

    private fun bigButton(t: String, f: () -> Unit) = Button(this).apply {
        text = t; isAllCaps = false; textSize = 16f
        setOnClickListener { f() }
        layoutParams = LinearLayout.LayoutParams(MATCH_PARENT, WRAP_CONTENT).apply { topMargin = dp(16) }
    }

    private fun smallButton(t: String, f: () -> Unit) = Button(this, null, android.R.attr.borderlessButtonStyle).apply {
        text = t; isAllCaps = false; textSize = 13f; setTextColor(ACCENT)
        setOnClickListener { f() }
    }

    private fun row(vararg v: View) = LinearLayout(this).apply {
        orientation = LinearLayout.HORIZONTAL
        v.forEach { addView(it, LinearLayout.LayoutParams(0, WRAP_CONTENT, 1f)) }
    }

    private fun copy(s: String?) {
        if (s.isNullOrEmpty()) { toast("Пока нет данных"); return }
        (getSystemService(CLIPBOARD_SERVICE) as ClipboardManager).setPrimaryClip(ClipData.newPlainText("ygg", s))
        toast("Скопировано")
    }

    private fun toast(s: String) = Toast.makeText(this, s, Toast.LENGTH_SHORT).show()

    companion object {
        private val ACCENT = Color.parseColor("#1F6F43")
    }
}
