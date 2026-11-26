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
    /** Node statistics only: never queued behind tracker requests (up to 40 s) or exit checks. */
    private val statsBg = Executors.newSingleThreadExecutor()
    /** One statistics poll at a time: a slow one is skipped over, not stacked up. */
    private val statsBusy = java.util.concurrent.atomic.AtomicBoolean(false)

    private lateinit var content: FrameLayout
    private val tabButtons = mutableListOf<Button>()
    private val pages = mutableListOf<View>()
    private var current = 0

    // Client
    private lateinit var etServer: EditText
    private lateinit var etKey: EditText
    private lateinit var cbUdp: CheckBox
    private lateinit var cbIpv6: CheckBox
    private lateinit var cbYggSites: CheckBox
    private lateinit var cbYggOnly: CheckBox
    private lateinit var cbCkr: CheckBox
    private lateinit var cbCkrAuto: CheckBox
    private lateinit var etCkrAddress: EditText
    private lateinit var etCkrDns: EditText
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
    private lateinit var clientsBox: LinearLayout
    private val clientRows = HashMap<String, TextView>()
    private val clientPrev = HashMap<String, LongArray>() // addr -> (tx, rx, at ms)
    private lateinit var cbLan: CheckBox
    private lateinit var cbPeer: CheckBox
    private lateinit var etPeerPort: EditText
    private lateinit var etPeerHost: EditText
    private lateinit var tvPeer: TextView

    /** Where clients reach our peer port (it goes into the server's links and QR codes). */
    private fun peerNote(): String = if (prefs.listenPort <= 0)
        tr("Адрес пира попадает в ссылки и QR-коды сервера: клиент добавит его в свои пиры и пойдёт к серверу коротким путём. Транзита нет.")
    else Ygg.peerUris(prefs).let { u ->
        (if (u.isEmpty()) tr("Адрес не найден: укажите внешний адрес.") else tr("Пир для клиентов:\n") + u.joinToString("\n")) +
            tr("\nВ мобильной сети входящие подключения обычно закрыты оператором — надёжнее Wi-Fi с пробросом порта.")
    }
    private lateinit var cbAutostart: CheckBox
    private lateinit var btnExit: Button
    private lateinit var tvIdentity: TextView
    private lateinit var ivQr: ImageView
    private lateinit var qrBox: LinearLayout
    private var qrText: String? = null
    // Server: access keys and requests (access.json through libygghelper.so access …).
    private lateinit var accessBox: LinearLayout
    private lateinit var tvAccess: TextView
    private lateinit var etKeyName: EditText
    private lateinit var etKeyDays: EditText
    @Volatile private var lastAccess: JSONObject? = null
    private var accessSig: String? = null
    /** The key whose QR code is shown ("" = the newest unused one). */
    private var qrKeyId: String? = null
    private var accessCounter = 0

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
        I18n.init(this)
        // Simple mode unless the full app was chosen: the launcher icon (and old home-screen
        // shortcuts) open this activity, so it hands over to the one-button screen here.
        // yggproxy:// links stay in the full app.
        if (!prefs.advancedMode && intent?.action != Intent.ACTION_VIEW) {
            startActivity(Intent(this, SimpleActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NO_ANIMATION))
            finish()
            return
        }

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
        listOf(tr("Клиент"), tr("Сервер"), tr("Статус"), tr("Настройки")).forEachIndexed { i, t ->
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
        handleChat(intent)
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        handleLink(intent)
        handleChat(intent)
    }

    /** A chat notification was tapped. */
    private fun handleChat(i: Intent?) {
        i?.getStringExtra(YggService.EXTRA_CHAT)?.let { openClientChat(it) }
        i?.getStringExtra(YggService.EXTRA_OWNER_CHAT)?.let { openOwnerChat(it, it) }
    }

    /** Client: chat with [server]'s owner. */
    private fun openClientChat(server: String) {
        Chat.open(this, tr("Чат: %1\$s", prefs.label(server)),
            tr("Напишите владельцу сервера — ответ придёт сюда и уведомлением."), mineIsOwner = false,
            load = { Chat.sync(server, it) }, send = { Chat.send(server, it) },
            seen = { prefs.setChatSeen(server, it); prefs.setChatNotified(server, maxOf(it, prefs.chatNotified(server))) })
    }

    /** Exit mode: chat with the client at [addr]. */
    private fun openOwnerChat(addr: String, title: String) {
        Chat.open(this, title, tr("Сообщений пока нет."), mineIsOwner = true,
            load = { Chat.ownerHistory(this, addr, it) }, send = { Chat.ownerSend(this, addr, it) },
            seen = { prefs.ownerChatSeen = maxOf(prefs.ownerChatSeen, it) })
    }

    /** Exit mode: conversations with clients, newest first; a tap opens one. */
    private fun ownerChats() {
        Thread {
            val chats = try { Chat.ownerList(this, prefs.ownerChatSeen) } catch (e: Exception) {
                runOnUiThread { toast(tr("Чат: %1\$s", e.message)) }; return@Thread
            }
            // Clients by name: their key's name and device, or their request's device name.
            val names = HashMap<String, String>()
            Ygg.accessCmd(this, "list").getOrNull()?.let { a ->
                a.optJSONArray("keys")?.let { k -> for (j in 0 until k.length()) k.getJSONObject(j).let { o ->
                    val ad = o.optString("addr"); if (ad.isNotEmpty()) names[ad] = listOf(o.optString("name"), o.optString("device")).filter { it.isNotEmpty() }.distinct().joinToString(" — ")
                } }
                a.optJSONArray("requests")?.let { r -> for (j in 0 until r.length()) r.getJSONObject(j).let { o -> names.putIfAbsent(o.optString("addr"), o.optString("name") + tr(" (ждёт доступа)")) } }
            }
            runOnUiThread {
                if (chats.isEmpty()) { toast(tr("Сообщений от клиентов пока нет")); return@runOnUiThread }
                val labels = chats.map { c ->
                    val addr = c.optString("addr")
                    val last = c.optJSONObject("last")
                    val new = c.optInt("new")
                    (if (new > 0) "● " else "") + (names[addr]?.ifEmpty { null } ?: addr) + "\n" +
                        (if (last?.optBoolean("owner") == true) tr("вы: ") else "") + (last?.optString("text") ?: "").take(60).replace('\n', ' ')
                }
                android.app.AlertDialog.Builder(this).setTitle(tr("Чат с клиентами"))
                    .setItems(labels.toTypedArray()) { _, i ->
                        val addr = chats[i].optString("addr")
                        openOwnerChat(addr, names[addr]?.ifEmpty { null } ?: addr)
                    }
                    .setNegativeButton(tr("Закрыть"), null).show()
            }
        }.apply { isDaemon = true; start() }
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
        Updates.check(this, prefs)
        // The node runs while the app is on screen (pings the servers and the root); this also
        // brings back a service that was killed or stopped by an app update.
        State.uiShown()
        State.stoppedByUser = false
        try { YggService.send(this, YggService.ACTION_RECONCILE) } catch (e: Exception) { State.log("ui", tr("сервис не запустился: %1\$s", e.message)) }
    }

    override fun onStop() {
        super.onStop()
        State.uiHidden()
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
        addView(h1(tr("Клиент")))
        addView(bigButton(tr("Сканировать QR сервера")) { scanQr() })
        etServer = edit(tr("адрес сервера 200:…"), "")
        addView(etServer)
        etKey = edit(tr("ключ доступа (от владельца сервера)"), "")
        addView(LinearLayout(this@MainActivity).apply {
            orientation = LinearLayout.HORIZONTAL; gravity = Gravity.CENTER_VERTICAL
            addView(etKey, LinearLayout.LayoutParams(0, WRAP_CONTENT, 1f))
            addView(smallButton(tr("Добавить")) { addServerFromInput() })
        })
        addView(h2(tr("Серверы")))
        addView(note(tr("Серверы закрыты: нужен ключ от владельца (QR, ссылка или адрес + ключ). Без ключа при подключении владельцу уйдёт запрос доступа. 🔑 — ключ есть, 🌐 — публичный. Нажмите на сервер — подключиться, долгое нажатие — меню.")))
        serversBox = LinearLayout(this@MainActivity).apply { orientation = LinearLayout.VERTICAL }.also { addView(it) }
        tvRoot = mono(tr("Корень сети: …")).also { addView(it) }
        btnVpn = bigButton("") { toggleVpn() }.also { addView(it) }
        btnApps = smallButton(tr("Приложения без VPN")) { pickApps() }.also { addView(it) }
        addView(smallButton(tr("Чат с сервером")) { Ygg.serverFromText(prefs.server)?.let { openClientChat(it) } ?: toast(tr("Сервер не выбран")) })
        tvClientState = mono("").also { addView(it) }
    }

    private fun buildServer(): LinearLayout = page().apply {
        addView(h1(tr("Сервер выхода")))
        addView(note(tr("Телефон раздаёт свой интернет через Yggdrasil. Сервер закрыт: пользоваться им могут только устройства с ключом — у каждого устройства свой. Локальные сети и loopback закрыты.")))
        tvIdentity = mono("").also { addView(it) }
        addView(row(smallButton(tr("Копировать адрес")) { copy(State.address) }))
        addView(label(tr("Название сервера: клиенты видят его вместо адреса")))
        val etName = edit(tr("например, «Сервер Маши»"), prefs.serverName)
        addView(LinearLayout(this@MainActivity).apply {
            orientation = LinearLayout.HORIZONTAL; gravity = Gravity.CENTER_VERTICAL
            addView(etName, LinearLayout.LayoutParams(0, WRAP_CONTENT, 1f))
            addView(smallButton(tr("Сохранить")) {
                prefs.serverName = etName.text.toString()
                etName.setText(prefs.serverName)
                if (State.serviceRunning) YggService.send(this@MainActivity, YggService.ACTION_RECONCILE)
                toast(if (prefs.serverName.isEmpty()) tr("Название убрано") else tr("Название сохранено"))
            })
        })
        addView(smallButton(tr("Чат с клиентами")) { ownerChats() })
        addView(h2(tr("Доступ")))
        tvAccess = mono("…").also { addView(it) }
        addView(label(tr("Новый ключ: для кого и на сколько дней (1–365)")))
        etKeyName = edit(tr("чьё устройство"), "")
        etKeyDays = edit("30", "30", number = true)
        addView(LinearLayout(this@MainActivity).apply {
            orientation = LinearLayout.HORIZONTAL; gravity = Gravity.CENTER_VERTICAL
            addView(etKeyName, LinearLayout.LayoutParams(0, WRAP_CONTENT, 2f))
            addView(etKeyDays, LinearLayout.LayoutParams(0, WRAP_CONTENT, 1f))
            addView(smallButton(tr("Создать")) { newKey() })
        })
        qrBox = LinearLayout(this@MainActivity).apply {
            orientation = LinearLayout.VERTICAL; gravity = Gravity.CENTER_HORIZONTAL; visibility = View.GONE
            ivQr = ImageView(this@MainActivity).apply { setBackgroundColor(Color.WHITE) }
            addView(ivQr, LinearLayout.LayoutParams(dp(240), dp(240)).apply { topMargin = dp(8) })
            addView(note(tr("На устройстве: Ratatoskr → «Сканировать QR». Ключ подходит одному устройству.")).apply { gravity = Gravity.CENTER })
        }
        addView(qrBox)
        accessBox = LinearLayout(this@MainActivity).apply { orientation = LinearLayout.VERTICAL }.also { addView(it) }
        addView(h2(tr("Ограничение скорости, Мбит/с (0 — без ограничения)")))
        addView(label(tr("Весь сервер: приём клиентами ↓ и отдача ↑")))
        etLimTotalDown = edit("0", mbitText(prefs.limTotalDown), decimal = true)
        etLimTotalUp = edit("0", mbitText(prefs.limTotalUp), decimal = true)
        addView(row(etLimTotalDown, etLimTotalUp))
        addView(label(tr("Каждый клиент: ↓ и ↑")))
        etLimClientDown = edit("0", mbitText(prefs.limClientDown), decimal = true)
        etLimClientUp = edit("0", mbitText(prefs.limClientUp), decimal = true)
        addView(row(etLimClientDown, etLimClientUp))
        addView(smallButton(tr("Применить ограничения")) { applyLimits() })
        addView(h2(tr("Клиенты")))
        addView(note(tr("Нажмите на клиента — написать ему.")))
        tvClients = mono("—").also { addView(it) }
        clientsBox = LinearLayout(this@MainActivity).apply { orientation = LinearLayout.VERTICAL }.also { addView(it) }
        cbLan = check(tr("Разрешить клиентам доступ в мою LAN"), prefs.allowLan).also { addView(it) }
        cbPeer = check(tr("Принимать пиров: клиенты подключаются к серверу напрямую (TCP)"), prefs.listenPort > 0).also { addView(it) }
        etPeerPort = edit(tr("порт (%1\$s)", Ygg.PEER_PORT), if (prefs.listenPort > 0) prefs.listenPort.toString() else "", number = true).also { addView(it) }
        etPeerHost = edit(tr("внешний адрес (пусто — адреса телефона)"), prefs.peerHost).also { addView(it) }
        tvPeer = note(peerNote()).also { addView(it) }
        cbAutostart = check(tr("Автозапуск после перезагрузки"), prefs.autostart).also { addView(it) }
        btnExit = bigButton("") { toggleExit() }.also { addView(it) }
        addView(smallButton(tr("Не ограничивать в фоне (батарея)")) { askBattery() })
    }

    /** Separate page: network settings and logs, switched at the top. */
    private fun buildSettings(): View {
        val box = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        val sw = LinearLayout(this).apply { orientation = LinearLayout.HORIZONTAL; setPadding(dp(8), dp(4), dp(8), 0) }
        listOf(tr("Сеть"), tr("Логи")).forEachIndexed { i, t ->
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
        // Both languages' names, so whoever reads the other one finds it.
        addView(label("Язык / Language"))
        addView(android.widget.RadioGroup(this@MainActivity).apply {
            orientation = LinearLayout.HORIZONTAL
            listOf("system" to tr("Системный"), "ru" to "Русский", "en" to "English").forEachIndexed { i, (code, name) ->
                addView(android.widget.RadioButton(this@MainActivity).apply {
                    id = 9100 + i; text = name; isChecked = prefs.language == code
                    setOnClickListener {
                        if (prefs.language == code) return@setOnClickListener
                        saveAll()
                        prefs.language = code
                        I18n.apply(code)
                        recreate()
                    }
                })
            }
        })
        addView(smallButton(tr("Простой режим: одна кнопка «Подключиться»")) {
            saveAll()
            prefs.advancedMode = false
            startActivity(Intent(this@MainActivity, SimpleActivity::class.java))
            finish()
        })
        addView(smallButton(tr("Проверить обновления (версия %1\$s)", Updates.current(this@MainActivity))) { Updates.check(this@MainActivity, prefs, manual = true) })
        addView(h1(tr("VPN-клиент")))
        cbUdp = check(tr("UDP через udpgw (QUIC, игры, звонки)"), prefs.udp).also { addView(it) }
        cbIpv6 = check(tr("IPv6-сайты через сервер"), prefs.ipv6).also { addView(it) }
        cbYggSites = check(tr("Сайты Yggdrasil и ALFIS (.ygg, .anon…) — через свой узел, рядом с интернетом"), prefs.yggSites).also { addView(it) }
        cbYggOnly = check(tr("Только Yggdrasil: VPN без сервера выхода (интернет как обычно)"), prefs.yggOnly).also { addView(it) }
        addView(note(tr("Адреса 200::/7 и имена ALFIS идут в сеть Yggdrasil напрямую через узел телефона. Только TCP: сайты, SSH, чаты; UDP и ping — нет.")))
        cbCkrAuto = check(tr("Туннель CKR сам, если сервер его предлагает; такие серверы — первыми"), prefs.ckrAuto).also { addView(it) }
        cbCkr = check(tr("Туннель CKR всегда (нужно для серверов Yggdrasil-ng)"), prefs.ckr).also { addView(it) }
        addView(note(tr("CKR: весь IP-трафик (UDP, игры, звонки, ping) идёт к серверу выхода пакетами по его ключу Yggdrasil, как в Yggdrasil-ng. Без CKR — прокси. Имена ALFIS в режиме CKR не работают, адреса Yggdrasil — работают.")))
        etCkrAddress = edit(tr("адрес в туннеле CKR (10.99.0.2)"), prefs.ckrAddress).also { addView(it) }
        etCkrDns = edit(tr("DNS через туннель CKR (1.1.1.1)"), prefs.ckrDns).also { addView(it) }
        addView(h1(tr("Узел Yggdrasil")))
        cbKeepNode = check(tr("Работать в фоне: пинговать серверы и корень, когда приложение свёрнуто"), prefs.keepNode).also { addView(it) }
        addView(note(tr("Без VPN и сервера узел работает в фоне с уведомлением. Выключите, чтобы экономить батарею: тогда он работает только при открытом приложении.")))
        addView(h1(tr("Трекеры")))
        addView(note(tr("Трекеры обходят сеть Yggdrasil и собирают работающие серверы выхода. Их список появляется на странице «Клиент»; новые трекеры добавляются сами.")))
        cbUseTrackers = check(tr("Получать серверы от трекеров"), prefs.useTrackers).also { addView(it) }
        addView(label(tr("Трекеры, по одному в строке (адрес Yggdrasil, порт %1\$s)", Ygg.TRACKER_PORT)))
        etTrackers = edit("200:…", prefs.trackers, lines = 3).also { addView(it) }
        tvTrackers = mono("").also { addView(it) }
        addView(smallButton(tr("Обновить список серверов сейчас")) { saveAll(); refreshTrackers(force = true) })
        addView(h1(tr("Сеть Yggdrasil")))
        cbPublic = check(tr("Публичный список пиров (publicpeers.neilalexander.dev)"), prefs.usePublicPeers).also { addView(it) }
        addView(label(tr("Свои пиры, по одному в строке (tls://host:port, tcp://…, ?key=…)")))
        etPeers = edit("tls://213.21.229.37:443", prefs.customPeers, lines = 4).also { addView(it) }
        addView(label(tr("Сколько пиров держать (1–32)")))
        etTarget = edit("3", prefs.target.toString(), number = true).also { addView(it) }
        cbLanDisc = check(tr("Искать пиры в локальной сети (yggdrasil-go, TLS)"), prefs.lanDiscovery).also { addView(it) }
        addView(label(tr("Слотов для LAN-пиров (0 = как выше)")))
        etLanTarget = edit("0", prefs.lanTarget.toString(), number = true).also { addView(it) }
        addView(label(tr("Клиент: менять пиров, пока пинг до сервера выше, мс (10–5000)")))
        etRttLimit = edit("250", prefs.rttLimit.toString(), number = true).also { addView(it) }
        addView(label(tr("Пауза между заменами пиров, с (5–3600)")))
        etSwapInterval = edit("30", prefs.swapInterval.toString(), number = true).also { addView(it) }
        addView(label(tr("Сколько проверять нового пира до решения, с (10–3600)")))
        etSwapSettle = edit("60", prefs.swapSettle.toString(), number = true).also { addView(it) }
        addView(note(tr("Узел пингует сервер каждые 10 с. Пока пинг выше порога, он заменяет самого медленного пира, затем проверяет нового заданное время: если пинг не упал хотя бы на 10 %, следующая замена ждёт вдвое дольше паузы (до 10 мин или до паузы, если она больше).")))
        addView(label(tr("Доп. аргументы tcp_node (для отладки)")))
        etExtra = edit("--quality-interval 120", prefs.extraArgs).also { addView(it) }
        addView(bigButton(tr("Сохранить и применить")) {
            saveAll()
            if (State.serviceRunning) YggService.send(this@MainActivity, YggService.ACTION_RECONCILE)
            toast(tr("Сохранено"))
        })
        addView(smallButton(tr("Обновить публичный список и перезапустить узел")) {
            saveAll()
            if (State.serviceRunning) YggService.send(this@MainActivity, YggService.ACTION_REFRESH_PEERS) else toast(tr("Сервис не запущен"))
        })
    }

    private fun buildStatus(): LinearLayout = page().apply {
        addView(h1(tr("Статус")))
        tvStatus = mono("").also { addView(it) }
        addView(smallButton(tr("Копировать адрес")) { copy(State.address) })
        addView(h2(tr("Пиры")))
        addView(note(tr("Нажмите на пира — подробности. ★ — через него сейчас идёт больше всего трафика.")))
        peersBox = LinearLayout(this@MainActivity).apply { orientation = LinearLayout.VERTICAL }.also { addView(it) }
        addView(h2(tr("Проверка выхода")))
        tvCheck = mono("—").also { addView(it) }
        addView(smallButton(tr("Проверить интернет через сервер")) {
            tvCheck.text = tr("проверяю…")
            bg.execute { val r = Ygg.checkInternet(); ui.post { tvCheck.text = r } }
        })
        addView(h2(tr("Тест скорости до сервера (iperf)")))
        addView(note(tr("Клиент ↔ сервер выхода через Yggdrasil, без интернета: 10 с приём, затем 10 с отдача.")))
        tvSpeed = mono("—").also { addView(it) }
        addView(smallButton(tr("Запустить тест скорости")) { runSpeedTest() })
    }

    private fun buildLogs(): View {
        val box = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL; setPadding(dp(8), dp(8), dp(8), dp(8)) }
        box.addView(row(smallButton(tr("Копировать")) { copy(State.logsText()) },
            smallButton(tr("Экспорт в файл")) { exportLogs() },
            smallButton(tr("Очистить")) { State.clearLogs(); tvLogs.text = "" }))
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
        prefs.yggSites = cbYggSites.isChecked
        prefs.yggOnly = cbYggOnly.isChecked
        prefs.ckr = cbCkr.isChecked
        prefs.ckrAuto = cbCkrAuto.isChecked
        etCkrAddress.text.toString().trim().let { if (Regex("^\\d{1,3}(\\.\\d{1,3}){3}$").matches(it)) prefs.ckrAddress = it }
        etCkrDns.text.toString().trim().let { if (it.isEmpty() || Regex("^[0-9a-fA-F:.]+$").matches(it)) prefs.ckrDns = it }
        prefs.limTotalDown = kbit(etLimTotalDown)
        prefs.limTotalUp = kbit(etLimTotalUp)
        prefs.limClientDown = kbit(etLimClientDown)
        prefs.limClientUp = kbit(etLimClientUp)
        prefs.allowLan = cbLan.isChecked
        prefs.listenPort = if (cbPeer.isChecked) etPeerPort.text.toString().toIntOrNull()?.takeIf { it in 1..65535 } ?: Ygg.PEER_PORT else 0
        prefs.peerHost = etPeerHost.text.toString()
        tvPeer.text = peerNote()
        prefs.autostart = cbAutostart.isChecked
        prefs.usePublicPeers = cbPublic.isChecked
        prefs.customPeers = etPeers.text.toString()
        prefs.target = etTarget.text.toString().toIntOrNull() ?: 3
        prefs.lanDiscovery = cbLanDisc.isChecked
        prefs.lanTarget = etLanTarget.text.toString().toIntOrNull() ?: 0
        prefs.rttLimit = etRttLimit.text.toString().toIntOrNull() ?: 250
        prefs.swapInterval = etSwapInterval.text.toString().toIntOrNull() ?: 30
        prefs.swapSettle = etSwapSettle.text.toString().toIntOrNull() ?: 60
        prefs.extraArgs = etExtra.text.toString()
    }

    private fun toggleVpn() {
        saveAll()
        if (prefs.vpnEnabled) {
            State.log("ui", tr("кнопка «Отключить VPN»"))
            prefs.vpnEnabled = false
            YggService.send(this, YggService.ACTION_RECONCILE)
            return
        }
        if (Ygg.parseServer(prefs.server) == null && !prefs.yggOnly) {
            // Nothing selected yet: take what was typed, else the first saved server.
            val typed = Ygg.serverFromText(etServer.text.toString())
            val pick = typed ?: prefs.serverList().firstOrNull()
            if (pick == null) { toast(tr("Добавьте сервер: отсканируйте QR или введите адрес 200:…")); return }
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
            if (resultCode == RESULT_OK) startVpn() else toast(tr("VPN не разрешён"))
        }
        if (requestCode == REQ_SCAN && resultCode == RESULT_OK) {
            val text = data?.getStringExtra(ScanActivity.EXTRA_TEXT) ?: return
            val server = Ygg.serverFromText(text)
            if (server == null) toast(tr("Это не адрес сервера Ratatoskr")) else {
                Ygg.keyFromText(text)?.let { prefs.setKey(server, it) }
                prefs.setServerPeers(server, Ygg.peersFromText(text))
                connectTo(server)
            }
        }
        if (requestCode == REQ_EXPORT && resultCode == RESULT_OK) {
            val uri = data?.data ?: return
            try {
                contentResolver.openOutputStream(uri)?.use { it.write(State.logsText().toByteArray()) }
                toast(tr("Логи сохранены"))
            } catch (e: Exception) { toast(tr("Не удалось сохранить: %1\$s", e.message)) }
        }
    }

    private fun runSpeedTest() {
        if (!prefs.vpnEnabled) { toast(tr("Сначала подключите VPN к серверу")); return }
        if (speedRunning) return
        speedRunning = true
        Thread {
            val done = mutableListOf<String>()
            fun show(extra: String?) = ui.post { tvSpeed.text = (done + listOfNotNull(extra)).joinToString("\n") }
            for (download in listOf(true, false)) {
                val dir = if (download) tr("↓ приём") else tr("↑ отдача")
                show("$dir: …")
                done += Ygg.speedTest(download, 10) { show(tr("%1\$s: %%.1f Мбит/с …", dir).format(it)) }
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
        if (prefs.yggOnly) { prefs.yggOnly = false; cbYggOnly.isChecked = false }
        prefs.addServer(server)
        prefs.server = server
        writeMonitors()
        serversSig = null
        if (prefs.vpnEnabled) {
            YggService.send(this, YggService.ACTION_RECONCILE)
            toast(tr("Переключено на %1\$s", server))
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
        if (server == null) { toast(tr("Неверная ссылка Ratatoskr")); return }
        val key = Ygg.keyFromText(data.toString())
        select(0)
        AlertDialog.Builder(this)
            .setTitle(tr("Подключиться к серверу?"))
            .setMessage(tr("Весь трафик пойдёт через сервер выхода\n%1\$s", server) + if (key != null) tr("\n(с ключом доступа)") else "")
            .setPositiveButton(tr("Подключить")) { _, _ -> key?.let { prefs.setKey(server, it) }; prefs.setServerPeers(server, Ygg.peersFromText(data.toString())); connectTo(server) }
            .setNegativeButton(tr("Отмена"), null)
            .show()
    }

    /** The node re-reads this ping list on its own (no restart, the VPN stays up). */
    private fun writeMonitors() {
        Ygg.writeMonitors(filesDir, prefs)
    }

    private fun addServerFromInput() {
        val s = Ygg.serverFromText(etServer.text.toString())
        if (s == null) { toast(tr("Адрес сервера: IPv6 из 200::/7, например 200:…")); return }
        val key = Ygg.keyFromText(etServer.text.toString()) ?: Ygg.normKey(etKey.text.toString()).takeIf { it.isNotEmpty() }
        if (key != null) prefs.setKey(s, key)
        prefs.setServerPeers(s, Ygg.peersFromText(etServer.text.toString()))
        etKey.setText("")
        prefs.addServer(s)
        if (Ygg.parseServer(prefs.server) == null) prefs.server = s
        writeMonitors()
        etServer.setText("")
        serversSig = null
        refreshNow()
        toast(tr("Сервер добавлен"))
    }

    /**
     * Ask the trackers (through the node's SOCKS) for exit servers and for more trackers.
     * Every 2 min while the app is open; [force] right now.
     */
    private fun refreshTrackers(force: Boolean = false) {
        if (!prefs.useTrackers || trackerBusy) return
        if (!force && System.currentTimeMillis() - State.trackerAt < 120_000) return
        if (!State.nodeRunning) { if (force) toast(tr("Узел ещё не запущен")); return }
        trackerBusy = true
        State.trackerAt = System.currentTimeMillis()
        State.trackerStatus = tr("запрашиваю…")
        bg.execute {
            val exits = LinkedHashMap<String, Long>()
            val newTrackers = mutableListOf<String>()
            val ok = mutableListOf<String>()
            val public = HashSet<String>()
            for (t in prefs.trackerList()) {
                val e = Ygg.fetchTracker(t, "/exits") ?: continue
                ok += t
                Ygg.parseExits(e, exits, public)
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
                        peersNote = tr(", пиров %1\$s", n)
                        break
                    }
                }
            }
            val known = prefs.trackerList()
            val add = newTrackers.distinct().filter { it !in known }
            if (add.isNotEmpty()) prefs.trackers = (known + add).joinToString("\n")
            if (ok.isNotEmpty()) { State.trackerExits = exits.map { it.key to it.value }.take(50); State.trackerPublic = public }
            val time = java.text.SimpleDateFormat("HH:mm", java.util.Locale.US).format(java.util.Date())
            State.trackerStatus = if (ok.isEmpty()) tr("%1\$s: трекеры недоступны", time)
                else tr("%1\$s: серверов %2\$s от %3\$s трекер(ов)%4\$s", time, exits.size, ok.size, peersNote) + if (add.isNotEmpty()) tr(", новых трекеров %1\$s", add.size) else ""
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
        val items = arrayOf(tr("Подключиться"), tr("Копировать адрес"), tr("Ключ доступа…"), tr("Удалить из списка"), tr("Запросить доступ у владельца…"))
        AlertDialog.Builder(this).setTitle(addr).setItems(items) { _, w ->
            when (w) {
                0 -> connectTo(addr)
                1 -> copy(addr)
                2 -> {
                    val e = edit(tr("ключ от владельца сервера"), prefs.keyFor(addr) ?: "")
                    AlertDialog.Builder(this).setTitle(tr("Ключ доступа")).setView(e)
                        .setPositiveButton(tr("Сохранить")) { _, _ ->
                            prefs.setKey(addr, Ygg.normKey(e.text.toString()))
                            serversSig = null; refreshNow(); toast(tr("Ключ сохранён"))
                        }
                        .setNegativeButton(tr("Отмена"), null).show()
                }
                4 -> {
                    val e = edit(tr("сообщение владельцу: кто вы и зачем"), "", lines = 2)
                    e.filters = arrayOf(android.text.InputFilter.LengthFilter(200))
                    AlertDialog.Builder(this).setTitle(tr("Запрос доступа")).setView(e)
                        .setMessage(tr("Владелец увидит имя устройства (%1\$s) и сообщение.", Ygg.deviceName()))
                        .setPositiveButton(tr("Отправить")) { _, _ ->
                            val msg = e.text.toString()
                            bg.execute {
                                val (code, exp) = Ygg.probeAccess(addr, null, ask = true, message = msg)
                                prefs.noteAccess(addr, code, exp)
                                State.requests[addr] = code
                                if (code == "pending") prefs.addPending(addr)
                                ui.post { toast(if (code == "pending") tr("Запрос отправлен — придёт уведомление, когда владелец ответит") else Ygg.accessText(code)) }
                            }
                        }
                        .setNegativeButton(tr("Отмена"), null).show()
                }
                3 -> {
                    if (prefs.vpnEnabled && prefs.server == addr) { toast(tr("Сначала отключите VPN от этого сервера")); return@setItems }
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
        val byAddr = Ygg.rttByAddr(rtt)
        val list = prefs.serverList()
        val fromTrackers = State.trackerExits.filter { it.first !in list }
        val sig = list.joinToString() + "|" + fromTrackers.joinToString { it.first } + "|" + prefs.serverKeys + State.trackerPublic + "|" + prefs.server + "|" + prefs.vpnEnabled + State.vpnActive + "|" +
            list.joinToString { byAddr[it]?.let { o -> o.opt("rtt_ms").toString() + o.opt("avg_ms") } ?: "-" }
        if (sig != serversSig) {
            serversSig = sig
            serversBox.removeAllViews()
            if (list.isEmpty()) serversBox.addView(note(tr("Список пуст: отсканируйте QR сервера или введите адрес.")))
            for (a in list) {
                val o = byAddr[a]
                val ping = when {
                    !State.nodeRunning -> tr("узел не запущен")
                    o == null || o.isNull("rtt_ms") -> tr("нет ответа")
                    else -> tr("%1\$s мс (сред. %2\$s мс)", o.optLong("rtt_ms"), o.optLong("avg_ms"))
                }
                val sel = a == prefs.server
                val mark = when { sel && prefs.vpnEnabled && State.vpnActive -> tr("● подключено"); sel && prefs.vpnEnabled -> tr("◐ подключается"); sel -> tr("○ выбран"); else -> "" }
                val kind = (if (prefs.isOwn(a)) "🔑 " else if (a in State.trackerPublic) "🌐 " else "") + (if (a in State.ckrServers) "CKR " else "")
                serversBox.addView(mono(tr("%1\$s%2\$s\n    пинг: %3\$s  %4\$s", kind, prefs.serverNameOf(a)?.let { "$it\n    $a" } ?: a, ping, mark)).apply {
                    setTextIsSelectable(false)
                    if (sel) setTypeface(Typeface.MONOSPACE, Typeface.BOLD)
                    setBackgroundResource(android.R.drawable.list_selector_background)
                    setPadding(dp(4), dp(8), dp(4), dp(8))
                    setOnClickListener { connectTo(a) }
                    setOnLongClickListener { serverMenu(a); true }
                })
            }
            if (fromTrackers.isNotEmpty()) {
                serversBox.addView(h2(tr("От трекеров")))
                for ((a, trackerRtt) in fromTrackers) {
                    val o = byAddr[a]
                    val ping = when {
                        o != null && !o.isNull("rtt_ms") -> tr("%1\$s мс", o.optLong("rtt_ms"))
                        trackerRtt >= 0 -> tr("нет ответа (у трекера %1\$s мс)", trackerRtt)
                        else -> tr("нет ответа")
                    }
                    val kind = if (a in State.trackerPublic) "🌐 " else "🔒 "
                    serversBox.addView(mono(tr("%1\$s%2\$s\n    пинг: %3\$s", kind, a, ping)).apply {
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
        if (::tvTrackers.isInitialized) tvTrackers.text = tr("Трекеры: ") + (if (prefs.useTrackers) State.trackerStatus else tr("выключены"))
        val root = rtt?.optJSONObject("root")
        tvRoot.text = tr("Корень сети: ") + when {
            !State.nodeRunning -> tr("узел не запущен")
            root == null || root.isNull("rtt_ms") -> tr("нет ответа") + (root?.optString("key")?.take(16)?.let { " ($it…)" } ?: "")
            else -> tr("%1\$s мс (сред. %2\$s мс)", root.optLong("rtt_ms"), root.optLong("avg_ms"))
        }
        btnApps.text = tr("Приложения без VPN (%1\$s)", prefs.excludedAppList().size)
    }

    /** Multi-choice list of installed apps whose traffic bypasses the VPN. */
    private fun pickApps() {
        toast(tr("Загружаю список приложений…"))
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
                    .setTitle(tr("Не пускать в VPN"))
                    .setMultiChoiceItems(apps.map { "${it.second}\n${it.first}" }.toTypedArray(), checked) { _, w, on -> checked[w] = on }
                    .setPositiveButton(tr("Сохранить")) { _, _ ->
                        prefs.excludedApps = apps.filterIndexed { i, _ -> checked[i] }.joinToString(",") { it.first }
                        if (prefs.vpnEnabled) YggService.send(this, YggService.ACTION_RECONCILE)
                        serversSig = null; refreshNow()
                        toast(tr("Сохранено: %1\$s", prefs.excludedAppList().size))
                    }
                    .setNegativeButton(tr("Отмена"), null)
                    .show()
            }
        }
    }

    private fun exportLogs() {
        val stamp = java.text.SimpleDateFormat("yyyyMMdd-HHmm", java.util.Locale.US).format(java.util.Date())
        val i = Intent(Intent.ACTION_CREATE_DOCUMENT).addCategory(Intent.CATEGORY_OPENABLE).setType("text/plain")
            .putExtra(Intent.EXTRA_TITLE, "ratatoskr-log-$stamp.txt")
        @Suppress("DEPRECATION")
        try { startActivityForResult(i, REQ_EXPORT) } catch (e: Exception) { toast(tr("Нет приложения для сохранения файлов")) }
    }

    private fun shareKey(k: JSONObject) {
        val addr = State.address ?: run { toast(tr("Адрес появится после запуска узла")); return }
        val text = tr("Сервер Ratatoskr (ключ для одного устройства, %1\$s дн.)\n", k.optInt("days")) +
            tr("%1\$s\nАдрес: %2\$s\nКлюч: %3\$s", Ygg.connectLink(addr, k.optString("key"), Ygg.peerUris(prefs)), addr, k.optString("key"))
        startActivity(Intent.createChooser(Intent(Intent.ACTION_SEND).setType("text/plain").putExtra(Intent.EXTRA_TEXT, text), tr("Ключ доступа")))
    }

    // --------------------------------------------------------------------------- access (server)

    /** Run `access …` off the UI thread, then show the new state ([msg] when it worked). */
    private fun accessOp(msg: String, vararg args: String) {
        bg.execute {
            val r = Ygg.accessCmd(this, *args)
            ui.post {
                r.onSuccess { lastAccess = it; accessSig = null; renderAccess(); toast(msg) }
                    .onFailure { toast(tr("Ошибка: %1\$s", it.message)) }
            }
        }
    }

    private fun newKey() {
        val days = etKeyDays.text.toString().toIntOrNull()?.takeIf { it in 1..365 } ?: run { toast(tr("Срок: от 1 до 365 дней")); return }
        qrKeyId = ""
        accessOp(tr("Ключ создан — покажите QR-код устройству"), "new", etKeyName.text.toString().trim(), days.toString())
        etKeyName.setText("")
    }

    private fun keyList(): List<JSONObject> = lastAccess?.optJSONArray("keys")?.let { a -> (0 until a.length()).map { a.getJSONObject(it) } } ?: emptyList()

    /** The key shown as a QR code: the chosen one, or the newest unused after «Создать». */
    private fun qrKey(): JSONObject? {
        val id = qrKeyId ?: return null
        val keys = keyList()
        return if (id.isEmpty()) keys.filter { it.isNull("addr") }.maxByOrNull { it.optLong("created") }
        else keys.firstOrNull { it.optString("id") == id }
    }

    private fun keyMenu(k: JSONObject) {
        val id = k.optString("id")
        val bound = !k.isNull("addr")
        val items = mutableListOf<Pair<String, () -> Unit>>()
        if (!bound) items += tr("Показать QR-код") to { qrKeyId = id; renderButtons() }
        if (!bound) items += tr("Поделиться ссылкой") to { shareKey(k) }
        items += tr("Продлить на %1\$s дн. от сегодня", k.optInt("days")) to { accessOp(tr("Ключ продлён"), "renew", id) }
        if (bound) items += tr("Написать") to { val a = k.optString("addr"); openOwnerChat(a, clientName(a) ?: a) }
        if (bound) items += tr("Отвязать от устройства") to { accessOp(tr("Ключ отвязан: им сможет войти новое устройство"), "unbind", id) }
        items += tr("Удалить (доступ сразу закрыт)") to { accessOp(tr("Ключ удалён"), "revoke", id) }
        AlertDialog.Builder(this).setTitle(tr("Ключ %1\$s", k.optString("name").ifEmpty { id }))
            .setItems(items.map { it.first }.toTypedArray()) { _, w -> items[w].second() }.show()
    }

    private fun requestMenu(r: JSONObject) {
        val addr = r.optString("addr")
        val days = etKeyDays.text.toString().toIntOrNull()?.takeIf { it in 1..365 } ?: 30
        AlertDialog.Builder(this).setTitle(tr("Запрос доступа"))
            .setMessage(tr("%1\$s\n%2\$s\nпопыток: %3\$s", r.optString("name"), addr, r.optInt("count")) +
                r.optString("message").takeIf { it.isNotEmpty() }?.let { "\n\n«$it»" }.orEmpty())
            .setPositiveButton(tr("Одобрить на %1\$s дн.", days)) { _, _ -> accessOp(tr("Доступ одобрен"), "approve", addr, days.toString()) }
            .setNegativeButton(tr("Отклонить")) { _, _ -> accessOp(tr("Запрос отклонён — устройство получит уведомление"), "deny", addr) }
            .setNeutralButton(tr("Написать")) { _, _ -> openOwnerChat(addr, r.optString("name").ifEmpty { addr }) }.show()
    }

    private fun publicDialog() {
        val cb = check(tr("Я понимаю последствия и принимаю риск"), false)
        val box = page().apply { addView(note(Ygg.PUBLIC_RISK)); addView(cb) }
        val d = AlertDialog.Builder(this).setTitle(tr("Сделать сервер публичным?")).setView(scrollPage(box))
            .setPositiveButton(tr("Сделать публичным")) { _, _ -> accessOp(tr("Сервер стал публичным"), "public", "on", "--accept-risk") }
            .setNegativeButton(tr("Отмена"), null).show()
        val ok = d.getButton(AlertDialog.BUTTON_POSITIVE)
        ok.isEnabled = false
        cb.setOnCheckedChangeListener { _, v -> ok.isEnabled = v }
    }

    private fun renderAccess() {
        val a = lastAccess ?: return
        val now = System.currentTimeMillis() / 1000
        val sig = a.toString() + (now / 60)
        if (sig == accessSig) return
        accessSig = sig
        renderButtons()
        val public = a.optBoolean("public")
        tvAccess.text = if (public) tr("ПУБЛИЧНЫЙ: пользоваться может любой, кто найдёт сервер.") else tr("Закрыт — только по ключам.")
        tvAccess.setTextColor(if (public) Color.parseColor("#B04010") else Color.BLACK)
        accessBox.removeAllViews()
        accessBox.addView(if (public) smallButton(tr("Закрыть сервер (только по ключам)")) { accessOp(tr("Сервер закрыт"), "public", "off") }
            else smallButton(tr("Сделать публичным…")) { publicDialog() })
        val keys = keyList()
        accessBox.addView(h2(tr("Ключи (%1\$s)", keys.size)))
        if (keys.isEmpty()) accessBox.addView(note(tr("Ключей пока нет.")))
        for (k in keys) {
            val exp = k.optLong("expires")
            val left = if (exp > now) tr("ещё %1\$s", Ygg.dur(exp - now)) else tr("ИСТЁК")
            val dev = if (k.isNull("addr")) tr("ещё не использован") else "${k.optString("device")} ${k.optString("addr")}"
            accessBox.addView(mono(tr("%1\$s · %2\$s дн., %3\$s\n    %4\$s", k.optString("name").ifEmpty { k.optString("id") }, k.optInt("days"), left, dev)).apply {
                setTextIsSelectable(false)
                if (exp <= now) setTextColor(Color.GRAY)
                setBackgroundResource(android.R.drawable.list_selector_background)
                setPadding(dp(4), dp(8), dp(4), dp(8))
                setOnClickListener { keyMenu(k) }
            })
        }
        val den = a.optJSONArray("denied")
        if (den != null && den.length() > 0) {
            accessBox.addView(h2(tr("Отклонённые (%1\$s)", den.length())))
            accessBox.addView(note(tr("Повторно просить 30 дней не могут. Нажмите — одобрить или разрешить запросить снова.")))
            for (i in 0 until den.length()) {
                val d = den.getJSONObject(i)
                val addr = d.optString("addr")
                accessBox.addView(mono(tr("%1\$s\n    %2\$s · %3\$s назад", d.optString("name"), addr, Ygg.dur(now - d.optLong("time")))).apply {
                    setTextIsSelectable(false)
                    setTextColor(Color.GRAY)
                    setBackgroundResource(android.R.drawable.list_selector_background)
                    setPadding(dp(4), dp(8), dp(4), dp(8))
                    setOnClickListener {
                        val days = etKeyDays.text.toString().toIntOrNull()?.takeIf { it in 1..365 } ?: 30
                        AlertDialog.Builder(this@MainActivity).setTitle(d.optString("name").ifEmpty { addr })
                            .setPositiveButton(tr("Одобрить на %1\$s дн.", days)) { _, _ -> accessOp(tr("Доступ одобрен"), "approve", addr, days.toString()) }
                            .setNegativeButton(tr("Разрешить запросить снова")) { _, _ -> accessOp(tr("Устройство снова может запросить доступ"), "forget", addr) }
                            .setNeutralButton(tr("Отмена"), null).show()
                    }
                })
            }
        }
        val reqs = a.optJSONArray("requests")
        if (reqs != null && reqs.length() > 0) {
            accessBox.addView(h2(tr("Запросы доступа (%1\$s)", reqs.length())))
            for (i in 0 until reqs.length()) {
                val r = reqs.getJSONObject(i)
                val msg = r.optString("message").takeIf { it.isNotEmpty() }?.let { "\n    «$it»" }.orEmpty()
                accessBox.addView(mono(tr("%1\$s\n    %2\$s · %3\$s назад%4\$s", r.optString("name"), r.optString("addr"), Ygg.dur(now - r.optLong("last")), msg)).apply {
                    setTextIsSelectable(false)
                    setBackgroundResource(android.R.drawable.list_selector_background)
                    setPadding(dp(4), dp(8), dp(4), dp(8))
                    setOnClickListener { requestMenu(r) }
                })
            }
        }
    }

    private fun kbit(e: EditText) = ((e.text.toString().replace(',', '.').toDoubleOrNull() ?: 0.0) * 1000).toInt().coerceAtLeast(0)
    private fun mbitText(kbit: Int) = if (kbit % 1000 == 0) (kbit / 1000).toString() else "%.1f".format(kbit / 1000.0)

    /** Save the limits; a running exit re-reads its limits file within 2 s (no reconnects). */
    private fun applyLimits() {
        saveAll()
        try { java.io.File(filesDir, "limits.txt").writeText(prefs.limitsText()) } catch (_: Exception) {}
        toast(if (prefs.serverEnabled) tr("Ограничения применены") else tr("Сохранено"))
    }

    private fun toggleExit() {
        saveAll()
        prefs.serverEnabled = !prefs.serverEnabled
        State.log("ui", if (prefs.serverEnabled) tr("кнопка «Запустить сервер»") else tr("кнопка «Остановить сервер»"))
        YggService.send(this, YggService.ACTION_RECONCILE)
        refreshNow()
        // A server must keep running with the screen off: offer the battery exemption once.
        val pm = getSystemService(POWER_SERVICE) as PowerManager
        if (prefs.serverEnabled && !pm.isIgnoringBatteryOptimizations(packageName)) {
            AlertDialog.Builder(this)
                .setTitle(tr("Работа с выключенным экраном"))
                .setMessage(tr("Чтобы сервер не засыпал, разрешите приложению работать в фоне без ограничений батареи."))
                .setPositiveButton(tr("Разрешить")) { _, _ -> askBattery() }
                .setNegativeButton(tr("Не сейчас"), null)
                .show()
        }
    }

    private fun askBattery() {
        val pm = getSystemService(POWER_SERVICE) as PowerManager
        if (pm.isIgnoringBatteryOptimizations(packageName)) { toast(tr("Уже без ограничений")); return }
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
            ui.postDelayed(this, 1000)
        }
    }

    private var probeCounter = 0

    private fun refreshNow() {
        if (!::tvStatus.isInitialized) return
        renderButtons()
        refreshTrackers()
        if (current == 1 && accessCounter++ % 3 == 0) bg.execute { Ygg.accessCmd(this, "list").onSuccess { lastAccess = it; ui.post { renderAccess() } } }
        if (current <= 2) {
            // Exit probe every ~6 s on its own thread: it may wait on the network.
            if (current == 2 && prefs.vpnEnabled && (probeCounter++ % 6 == 0)) {
                bg.execute { lastProbe = Ygg.probeExit(); probeDone = true }
            }
            if (statsBusy.compareAndSet(false, true)) {
                val withNode = current != 0
                statsBg.execute {
                    try {
                        lastRtt = Ygg.fetchRtt()
                        if (withNode) {
                            lastStats = Ygg.fetchStats()
                            lastPeers = Ygg.fetchPeers()
                        }
                    } finally {
                        statsBusy.set(false)
                    }
                    ui.post { renderServers(); renderStatus() }
                }
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
        btnVpn.text = if (prefs.vpnEnabled) tr("Отключить VPN") else tr("Подключить VPN")
        btnExit.text = if (prefs.serverEnabled) tr("Остановить сервер") else tr("Запустить сервер")
        tvClientState.text = when {
            !prefs.vpnEnabled -> tr("VPN выключен")
            prefs.yggOnly -> if (State.vpnActive) tr("VPN работает: только сайты Yggdrasil и ALFIS") else tr("VPN запускается…")
            State.vpnActive -> tr("VPN работает → %1\$s · %2\$s\n↓ %3\$s  ↑ %4\$s", State.vpnTarget, Ygg.modeText(), Ygg.human(State.tunRx), Ygg.human(State.tunTx))
            else -> tr("VPN запускается…") + (State.lastError?.let { "\n$it" } ?: "")
        } + (if (prefs.vpnEnabled && !prefs.yggOnly) State.access?.takeIf { it.first == Ygg.serverFromText(prefs.server) }
            .let { tr("\nдоступ: ") + Ygg.accessText(it?.second) + (it?.let { a -> Ygg.keyLeft(a.third) } ?: "") } else "") +
            State.requests.entries.joinToString("") { tr("\nзапрос к %1\$s: %2\$s", it.key, Ygg.accessText(it.value).let { t -> if (it.value == "pending") tr("ждёт ответа владельца") else t }) }
        val addr = State.address
        val link = addr?.let { a -> qrKey()?.let { Ygg.connectLink(a, it.optString("key"), Ygg.peerUris(prefs)) } }
        if (link != qrText) {
            qrText = link
            ivQr.setImageBitmap(link?.let { Qr.bitmap(it, dp(240)) })
            qrBox.visibility = if (link == null) View.GONE else View.VISIBLE
        }
        tvIdentity.text = if (addr == null) tr("Адрес появится после запуска узла.")
        else tr("Адрес сервера: %1\$s", addr) +
            if (prefs.serverEnabled) tr("\n%1\$s · соединений %2\$s (всего %3\$s, отказов %4\$s)\n", if (State.exitActive) tr("Работает") else tr("Запускается…"), State.srvConns, State.srvTotal, State.srvDenied) +
                tr("отдано %1\$s · принято %2\$s", Ygg.human(State.srvTx), Ygg.human(State.srvRx)) else ""
    }

    /** Exit clients seen in the last 10 s, with their current rate. */
    private fun renderClients() {
        val now = System.currentTimeMillis()
        val rows = if (prefs.serverEnabled) State.clients.entries.filter { now - it.value[3] < 10_000 }.sortedByDescending { it.value[1] + it.value[2] } else emptyList()
        tvClients.text = if (!prefs.serverEnabled) tr("сервер выключен") else tr("нет подключённых клиентов")
        tvClients.visibility = if (rows.isEmpty()) View.VISIBLE else View.GONE
        // Rows kept by address (rebuilt only when the set changes), so a tap is not lost to a refresh.
        val addrs = rows.map { it.key }
        if (addrs.toSet() != clientRows.keys) {
            clientsBox.removeAllViews(); clientRows.clear()
            for (addr in addrs) clientRows[addr] = mono("").apply {
                setTextIsSelectable(false)
                setBackgroundResource(android.R.drawable.list_selector_background)
                setPadding(dp(4), dp(8), dp(4), dp(8))
                setOnClickListener { clientMenu(addr) }
            }.also { clientsBox.addView(it) }
        }
        for ((addr, v) in rows) {
            val prev = clientPrev[addr]
            val dt = if (prev != null) (v[3] - prev[2]).coerceAtLeast(1) else 0
            val down = if (prev != null && dt > 0) (v[1] - prev[0]) * 1000 / dt else 0
            val up = if (prev != null && dt > 0) (v[2] - prev[1]) * 1000 / dt else 0
            if (prev == null || prev[2] != v[3]) clientPrev[addr] = longArrayOf(v[1], v[2], v[3])
            val name = clientName(addr)?.let { "$it\n" }.orEmpty()
            clientRows[addr]?.text = name + tr("%1\$s\n  соединений %2\$s · ↓ %3\$s ↑ %4\$s · всего ↓ %5\$s ↑ %6\$s", addr, v[0], Ygg.mbit(down.coerceAtLeast(0)), Ygg.mbit(up.coerceAtLeast(0)), Ygg.human(v[1]), Ygg.human(v[2]))
        }
    }

    /** A client's name from its key (name — device) or its access request; null if unknown. */
    private fun clientName(addr: String): String? {
        keyList().firstOrNull { it.optString("addr") == addr }?.let { k ->
            return listOf(k.optString("name"), k.optString("device")).filter { it.isNotEmpty() }.distinct().joinToString(" — ").ifEmpty { null }
        }
        val reqs = lastAccess?.optJSONArray("requests") ?: return null
        for (i in 0 until reqs.length()) reqs.getJSONObject(i).let { if (it.optString("addr") == addr) return it.optString("name").ifEmpty { null } }
        return null
    }

    /** A connected client was tapped. */
    private fun clientMenu(addr: String) {
        val name = clientName(addr)
        AlertDialog.Builder(this).setTitle(name ?: tr("Клиент"))
            .setMessage(addr)
            .setPositiveButton(tr("Написать")) { _, _ -> openOwnerChat(addr, name ?: addr) }
            .setNegativeButton(tr("Закрыть"), null).show()
    }

    private fun renderStatus() {
        val st = lastStats
        val sb = StringBuilder()
        sb.append(tr("Сервис: ")).append(if (State.serviceRunning) tr("работает") else tr("остановлен")).append('\n')
        sb.append(tr("Узел Yggdrasil: ")).append(
            when {
                st != null -> tr("на связи (аптайм %1\$s)", Ygg.dur(st.optLong("uptime_s")))
                State.nodeRunning -> tr("запущен, API не отвечает")
                else -> tr("не запущен")
            }).append('\n')
        if (st != null) {
            val n = st.getJSONObject("node")
            val key = n.optString("public_key")
            val root = n.optString("root")
            val coords = n.optJSONArray("coords")
            val c = (0 until (coords?.length() ?: 0)).joinToString(" ") { coords!!.get(it).toString() }
            // /api/stats "peers_up" has only 3 slots (the ESP32 format): count the /api/peers list.
            val peersUp = lastPeers?.optJSONArray("peers")?.length() ?: n.optInt("peers_up")
            sb.append(tr("Сеть: ")).append(
                when {
                    peersUp == 0 -> tr("нет пиров ✖")
                    root == key -> tr("только свой корень (сеть не видна) ⚠")
                    else -> tr("подключено ✔")
                }).append('\n')
            sb.append(tr("Адрес:  ")).append(n.optString("address")).append('\n')
            sb.append(tr("Ключ:   ")).append(key).append('\n')
            sb.append(tr("Корень: ")).append(root.take(16)).append("…\n")
            sb.append(tr("Коорд.: [")).append(c).append("]\n")
            lastRtt?.optJSONObject("root")?.let { r ->
                sb.append(tr("Пинг до корня: ")).append(if (r.isNull("rtt_ms")) tr("нет ответа") else tr("%1\$s мс (сред. %2\$s мс)", r.optLong("rtt_ms"), r.optLong("avg_ms"))).append('\n')
            }
            sb.append(tr("Поиск в LAN: ")).append(
                when {
                    !prefs.lanDiscovery -> tr("выключен")
                    State.lanStatus == null -> tr("запускается…")
                    State.lanStatus!!.startsWith("joined") -> "✔ " + State.lanStatus
                    else -> "✖ " + State.lanStatus
                }).append('\n')
            sb.append(tr("Пиров: ")).append(peersUp).append(tr(" · сессий ")).append(n.optInt("sessions")).append(tr(" · путей ")).append(n.optInt("paths")).append('\n')
            if (prefs.useTrackers) {
                // The node pings every tracker every 10 s (that also keeps the route warm).
                val byAddr = Ygg.rttByAddr(lastRtt)
                sb.append(tr("\nТрекеры (пинг узла):\n"))
                for (t in prefs.trackerList().mapNotNull { Ygg.serverFromText(it) }.distinct()) {
                    val o = byAddr[t]
                    sb.append("  ").append(t).append("  ").append(
                        if (o == null || o.isNull("rtt_ms")) tr("нет ответа") else tr("%1\$s мс (сред. %2\$s мс)", o.optLong("rtt_ms"), o.optLong("avg_ms"))
                    ).append('\n')
                }
            }
        } else if (State.address != null) {
            sb.append(tr("Адрес:  ")).append(State.address).append('\n')
        }
        sb.append('\n')
        if (prefs.vpnEnabled) {
            sb.append("VPN: ").append(if (State.vpnActive) tr("работает") else tr("запускается")).append(" → ").append(State.vpnTarget ?: "?").append(" · ").append(Ygg.modeText()).append('\n')
            sb.append(tr("Пинг до сервера: ")).append(State.serverRtt ?: tr("измеряю…")).append('\n')
            State.lastSwap?.let { sb.append(tr("Замена пира: ")).append(it).append('\n') }
            sb.append(tr("Связь с сервером выхода: ")).append(
                when {
                    !probeDone -> tr("проверяю…")
                    lastProbe != null -> tr("✔ %1\$s мс", lastProbe)
                    else -> tr("✖ нет ответа")
                }).append('\n')
            sb.append(tr("Трафик: ↓ ")).append(Ygg.human(State.tunRx)).append("  ↑ ").append(Ygg.human(State.tunTx)).append('\n')
        } else sb.append(tr("VPN: выключен\n"))
        if (prefs.serverEnabled) {
            sb.append(tr("Сервер выхода: ")).append(if (State.exitActive) tr("работает") else tr("запускается"))
                .append(tr(" · открыт всем узлам Yggdrasil")).append('\n')
            sb.append(tr("  соединений ")).append(State.srvConns).append(tr(" (всего ")).append(State.srvTotal).append(tr(", отказов ")).append(State.srvDenied).append(")\n")
            sb.append(tr("  отдано ")).append(Ygg.human(State.srvTx)).append(tr(" · принято ")).append(Ygg.human(State.srvRx)).append('\n')
        } else sb.append(tr("Сервер выхода: выключен\n"))
        State.lastError?.let { sb.append(tr("\nПоследняя ошибка: ")).append(it) }
        tvStatus.text = sb.toString()

        renderPeers()
    }

    /** Connected peers with live traffic; the one carrying most of it now is starred. */
    /** Peer rows are updated in place (no re-creation every second: no flicker, scroll stays). */
    private fun renderPeers() {
        val arr = lastPeers?.optJSONArray("peers")
        val rows = ArrayList<Pair<String, JSONObject?>>()
        if (arr == null || arr.length() == 0) {
            rows.add(tr("нет данных") to null)
        } else {
            val list = (0 until arr.length()).map { arr.getJSONObject(it) }
            val busiest = list.maxByOrNull { it.optLong("rx_bps") + it.optLong("tx_bps") }
                ?.takeIf { it.optLong("rx_bps") + it.optLong("tx_bps") > 500 }
            lastPeers?.let { t -> rows.add(tr("Всего сейчас: ↓ %1\$s  ↑ %2\$s", Ygg.mbit(t.optLong("rx_bps")), Ygg.mbit(t.optLong("tx_bps"))) to null) }
            for (p in list) {
                val host = p.optString("uri").substringAfter("://").substringBefore('?').ifEmpty { tr("входящий ") + p.optString("address") }
                val rtt = if (p.isNull("rtt_ms")) "" else tr("  %1\$s мс", p.optLong("rtt_ms"))
                val star = if (p === busiest) "★ " else "  "
                rows.add("$star${host}$rtt\n    ↓ ${Ygg.mbit(p.optLong("rx_bps"))}  ↑ ${Ygg.mbit(p.optLong("tx_bps"))}  · ${p.optInt("share")} %" to p)
            }
        }
        while (peersBox.childCount > rows.size) peersBox.removeViewAt(peersBox.childCount - 1)
        while (peersBox.childCount < rows.size) peersBox.addView(mono("").apply {
            setTextIsSelectable(false)
            setPadding(dp(4), dp(8), dp(4), dp(8))
        })
        for ((i, row) in rows.withIndex()) {
            val tv = peersBox.getChildAt(i) as TextView
            if (tv.text.toString() != row.first) tv.text = row.first
            val p = row.second
            if (p != null) {
                tv.setBackgroundResource(android.R.drawable.list_selector_background)
                tv.setOnClickListener { showPeer(p) }
            } else {
                tv.background = null
                tv.setOnClickListener(null)
                tv.isClickable = false
            }
        }
    }

    private fun showPeer(p: JSONObject) {
        val text = buildString {
            append(tr("Адрес пира: ")).append(p.optString("uri").ifEmpty { tr("входящее соединение") }).append("\n\n")
            append(tr("Направление: ")).append(if (p.optString("dir") == "out") tr("исходящий") else tr("входящий"))
            if (p.optBoolean("lan")) append(tr(" · найден в LAN"))
            append(tr("\nПинг: ")).append(if (p.isNull("rtt_ms")) "—" else tr("%1\$s мс", p.optLong("rtt_ms")))
            append(tr("\nСессия: ")).append(Ygg.dur(p.optLong("up_s")))
            append(tr("\n\nСейчас: ↓ ")).append(Ygg.mbit(p.optLong("rx_bps"))).append("  ↑ ").append(Ygg.mbit(p.optLong("tx_bps")))
            append(tr("\nДоля трафика узла: ")).append(p.optInt("share")).append(" %")
            append(tr("\nЗа сессию: ↓ ")).append(Ygg.human(p.optLong("rx"))).append("  ↑ ").append(Ygg.human(p.optLong("tx")))
            append(tr("\n\nYggdrasil-адрес пира:\n")).append(p.optString("address"))
            append(tr("\n\nКлюч:\n")).append(p.optString("key"))
        }
        AlertDialog.Builder(this)
            .setTitle(tr("Пир"))
            .setMessage(text)
            .setPositiveButton(tr("Закрыть"), null)
            .setNeutralButton(tr("Копировать")) { _, _ -> copy(p.optString("uri").ifEmpty { p.optString("address") }) }
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
        if (s.isNullOrEmpty()) { toast(tr("Пока нет данных")); return }
        (getSystemService(CLIPBOARD_SERVICE) as ClipboardManager).setPrimaryClip(ClipData.newPlainText("ygg", s))
        toast(tr("Скопировано"))
    }

    private fun toast(s: String) = Toast.makeText(this, s, Toast.LENGTH_SHORT).show()

    companion object {
        private val ACCENT = Color.parseColor("#1F6F43")
    }
}
