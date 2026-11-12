package dev.vasil.yggproxy

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.content.pm.ServiceInfo
import android.net.ConnectivityManager
import android.net.LocalSocket
import android.net.Network
import android.net.NetworkCapabilities
import android.net.NetworkRequest
import android.net.wifi.WifiManager
import android.net.LocalSocketAddress
import android.net.VpnService
import android.os.Build
import android.os.ParcelFileDescriptor
import android.os.PowerManager
import java.io.File
import java.util.concurrent.Executors

/**
 * One service for both roles:
 *  - always: tcp_node (Yggdrasil leaf node) with arguments built from the enabled roles;
 *  - client: VPN interface → ygghelper tun (tun2proxy) → 127.0.0.1:10808 → tcp_node --connect → exit;
 *  - server: ygghelper server (SOCKS5 + udpgw) exposed by tcp_node --forward PORT:10800.
 * Every start command just re-applies the settings ([reconcile]).
 */
class YggService : VpnService() {

    companion object {
        const val ACTION_RECONCILE = "dev.vasil.yggproxy.RECONCILE"
        const val ACTION_STOP = "dev.vasil.yggproxy.STOP"
        const val ACTION_REFRESH_PEERS = "dev.vasil.yggproxy.REFRESH_PEERS"
        private const val NOTIF_ID = 1
        private const val CHANNEL = "ygg"
        private const val PEERS_MAX_AGE = 6 * 3600 * 1000L

        /** The running service (same process), for calls that must not start it. */
        @Volatile var instance: YggService? = null

        fun send(ctx: Context, action: String) {
            val i = Intent(ctx, YggService::class.java).setAction(action)
            if (Build.VERSION.SDK_INT >= 26) ctx.startForegroundService(i) else ctx.startService(i)
        }
    }

    private lateinit var prefs: Prefs
    private val exec = Executors.newSingleThreadExecutor()

    private var node: ManagedProcess? = null
    private var exitProc: ManagedProcess? = null
    private var tunProc: ManagedProcess? = null
    private var vpnPfd: ParcelFileDescriptor? = null
    private var vpnSig: String? = null
    @Volatile private var ctl: LocalSocket? = null
    private var wakeLock: PowerManager.WakeLock? = null
    private var wifiLock: WifiManager.WifiLock? = null
    private var mcastLock: WifiManager.MulticastLock? = null
    private var wifiCb: ConnectivityManager.NetworkCallback? = null
    @Volatile private var wifiNet: Network? = null
    @Volatile private var registeredAt = 0L

    private val bin: String get() = applicationInfo.nativeLibraryDir
    private val keyFile get() = File(filesDir, "ygg.key")
    private val publicPeers get() = File(filesDir, "public-peers.json")
    private val peerFile get() = File(filesDir, "peers.json")
    private val ctlPath get() = File(filesDir, "tun.sock").path
    private val limitsFile get() = File(filesDir, "limits.txt")
    private val monitorsFile get() = File(filesDir, "monitors.txt")
    /** The exit server for the node (`--connect-file`): re-read live, so no restart on a switch. */
    private val connectFile get() = File(filesDir, "connect.txt")
    private var lastTarget: String? = null

    override fun onCreate() {
        super.onCreate()
        prefs = Prefs(this)
        State.serviceRunning = true
        instance = this
        journalTimer.scheduleWithFixedDelay({ journalTick() }, 10, 10, java.util.concurrent.TimeUnit.SECONDS)
    }

    // ------------------------------------------------------------------ peer journal

    private val journalTimer = Executors.newSingleThreadScheduledExecutor()
    private val journal by lazy { PeerJournal(File(filesDir, "peer-journal.json")) }
    private val preferFile get() = File(filesDir, "prefer.txt")
    private val searchFile get() = File(filesDir, "search.txt")

    /** The trackers, for the node's peer search while no server is used (re-read live). */
    private fun writeSearchFile() {
        try {
            val text = if (prefs.useTrackers) prefs.trackerList().joinToString("\n") else ""
            if (!searchFile.exists() || searchFile.readText() != text) searchFile.writeText(text)
        } catch (_: Exception) {}
    }
    private var journalTicks = 0L

    /**
     * Every 10 s: what each peer did (see PeerJournal). Every minute: the node's priority list
     * (our own best peers, then the trackers' ranking). Every 30 min: an anonymous summary to a
     * tracker (no address, no exit server).
     */
    private fun journalTick() { try {
        if (State.nodeRunning) Ygg.fetchPeers()?.let { peers ->
            val server = Ygg.fetchRtt()?.optJSONArray("servers")?.let { a ->
                (0 until a.length()).map { a.getJSONObject(it) }.firstOrNull { it.optBoolean("connected") && !it.isNull("avg_ms") }?.optDouble("avg_ms")
            }
            synchronized(journal) {
                journal.tick(peers, server)
                journalTicks++
                if (journalTicks % 6 == 0L) {
                    journal.save()
                    writeSearchFile() // trackers learned meanwhile
                    val tp = try { org.json.JSONObject(File(filesDir, "tracker-peers.json").readText()) } catch (_: Exception) { null }
                    val list = journal.preferList(if (prefs.useTrackers) tp else null).joinToString("\n")
                    if (!preferFile.exists() || preferFile.readText() != list) {
                        preferFile.writeText(list)
                        if (journalTicks == 6L) State.log("peers", "приоритет пиров: ${list.lines().count { it.isNotBlank() }} (свои лучшие и рейтинг трекеров)")
                    }
                }
                val now = System.currentTimeMillis() / 1000
                if (prefs.useTrackers && now - journal.lastReport >= 1800) {
                    val rep = journal.report()
                    val n = rep.optJSONArray("peers")?.length() ?: 0
                    if (n > 0) {
                        journal.lastReport = now
                        for (t in prefs.trackerList()) {
                            if (Ygg.postTracker(t, "/report", rep) != null) { State.log("peers", "статистика $n пиров отправлена трекеру $t"); break }
                        }
                    }
                }
            }
        }
    } catch (e: Exception) { State.log("peers", "журнал пиров: ${e.message}") } }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        foreground("Запуск…")
        State.log("service", "команда: ${intent?.action?.substringAfterLast('.') ?: "перезапуск системой (START_STICKY)"}")
        when (intent?.action) {
            SERVICE_INTERFACE -> if (VpnService.prepare(this) == null) prefs.vpnEnabled = true // always-on VPN
            ACTION_STOP -> {
                State.log("service", "«Остановить» в уведомлении")
                prefs.vpnEnabled = false; prefs.serverEnabled = false
                State.stoppedByUser = true
            }
            ACTION_REFRESH_PEERS -> exec.execute { refreshPeers(force = true); restartNode() }
        }
        exec.execute { reconcile() }
        return START_STICKY
    }

    override fun onTaskRemoved(rootIntent: Intent?) {
        State.log("service", "приложение убрано из недавних (сервис продолжает работать)")
        super.onTaskRemoved(rootIntent)
    }

    override fun onTrimMemory(level: Int) {
        if (level >= TRIM_MEMORY_RUNNING_LOW) State.log("service", "мало памяти (уровень $level)")
        super.onTrimMemory(level)
    }

    override fun onRevoke() {
        State.log("vpn", "VPN отозван системой или другим VPN")
        prefs.vpnEnabled = false
        exec.execute { reconcile() }
    }

    override fun onDestroy() {
        State.log("service", "onDestroy: сервис уничтожен системой или остановлен")
        stopAll("сервис завершён (onDestroy)")
        exec.shutdownNow()
        journalTimer.shutdownNow()
        try { synchronized(journal) { journal.save() } } catch (_: Exception) {}
        State.serviceRunning = false
        instance = null
        super.onDestroy()
    }

    // ---------------------------------------------------------------------------------------

    /** Re-apply the settings from the UI thread without (re)starting the service. */
    fun requestReconcile() {
        try { exec.execute { reconcile() } } catch (_: Exception) {}
    }

    private fun reconcile() {
        try {
            reconcileInner()
        } catch (e: Exception) {
            State.error("service", e.message ?: e.toString())
        }
        updateNotification()
    }

    private fun clientTarget(): Pair<String, Int>? {
        val (addr, port) = Ygg.parseServer(prefs.server) ?: return null
        return Ygg.fmtAddr(addr) to Ygg.PORT
    }

    private fun reconcileInner() {
        val target = clientTarget()
        if (prefs.vpnEnabled && target == null) State.error("vpn", "не задан адрес сервера выхода")
        if (prefs.vpnEnabled && VpnService.prepare(this) != null) {
            State.error("vpn", "нет разрешения на VPN: откройте приложение и нажмите «Подключить»")
            prefs.vpnEnabled = false
        }
        val wantVpn = prefs.vpnEnabled && target != null
        val wantExit = prefs.serverEnabled

        // With the app on screen the node runs anyway: it pings the saved servers and the root.
        val wantNode = State.uiVisible || (prefs.keepNode && !State.stoppedByUser)
        if (!wantVpn && !wantExit && !wantNode) {
            State.log("service", "нет включённых ролей (VPN и сервер выключены) — сервис завершается")
            stopAll("VPN и сервер выключены, приложение закрыто")
            stopForeground(STOP_FOREGROUND_REMOVE)
            stopSelf()
            return
        }

        refreshPeers(force = false)

        // 1. Yggdrasil node.
        // The server goes to the node through a file: switching servers or turning the VPN on and
        // off keeps the node, its peers and its paths.
        val connectLine = if (wantVpn) "[${target!!.first}]:${target.second}" else ""
        if (connectLine != lastTarget) {
            try { connectFile.writeText(connectLine) } catch (e: Exception) { State.error("vpn", "connect.txt: ${e.message}") }
            if (lastTarget != null) State.serverRtt = null
            lastTarget = connectLine
            if (wantVpn) State.log("vpn", "сервер выхода: ${target!!.first} (узел не перезапускается)")
        }
        val nodeCmd = nodeArgs(wantExit)
        if (node?.cmd != nodeCmd) {
            node?.stop("новые параметры узла")
            node = ManagedProcess("node", nodeCmd, filesDir,
                onStart = { State.nodeRunning = true; State.nodeStartedAt = System.currentTimeMillis() },
                onExit = { State.nodeRunning = false },
                onLine = ::nodeLine).start()
        }
        if (prefs.lanDiscovery) lanOn() else lanOff()

        // 2. Exit server.
        if (wantExit) {
            limitsFile.writeText(prefs.limitsText()) // the exit re-reads it live
            val cmd = buildList {
                add("$bin/libygghelper.so"); add("server")
                add("--socks"); add("127.0.0.1:${Ygg.EXIT_SOCKS}")
                add("--udpgw"); add("127.0.0.1:${Ygg.UDPGW}")
                add("--speedtest"); add("127.0.0.1:${Ygg.SPEEDTEST}")
                add("--proxy-protocol")
                add("--limits"); add(limitsFile.path)
                if (prefs.allowLan) add("--allow-lan")
                add("-v"); add("warn")
            }
            if (exitProc?.cmd != cmd) {
                exitProc?.stop("новые параметры прокси")
                exitProc = ManagedProcess("exit", cmd, filesDir,
                    onExit = { State.exitActive = false },
                    onLine = { l -> helperLine("exit", l) { if (it.startsWith("READY")) State.exitActive = true } }).start()
            }
        } else {
            exitProc?.stop("сервер выключен"); exitProc = null
            State.exitActive = false
        }
        if (wantExit) acquireWakeLock() else releaseWakeLock()

        // 3. VPN.
        if (wantVpn) {
            // The server is not part of it: a switch needs no new VPN interface.
            val sig = "${prefs.udp}|${prefs.ipv6}|${prefs.excludedApps}"
            State.vpnTarget = target!!.first
            if (vpnPfd == null || sig != vpnSig) {
                stopVpn()
                startVpn(target)
                vpnSig = sig
            }
        } else {
            stopVpn()
        }
    }

    private fun nodeArgs(exit: Boolean): List<String> = buildList {
        add("$bin/libtcp_node.so")
        add("--key"); add(keyFile.path)
        add("--peers"); add(peerFile.path)
        add("--no-public")
        add("--target"); add(prefs.target.toString())
        add("--swap-interval"); add(prefs.swapInterval.toString())
        add("--swap-settle"); add(prefs.swapSettle.toString())
        add("--api"); add("127.0.0.1:${Ygg.API}")
        add("--html-port"); add(Ygg.HTML_PORT.toString())
        // Ping list for the server list's ping column (re-read live, no restart on changes).
        monitorsFile.writeText(prefs.monitorList().joinToString("\n"))
        add("--monitor-file"); add(monitorsFile.path)
        add("--socks"); add("127.0.0.1:${Ygg.NODE_SOCKS}")
        if (prefs.lanDiscovery) {
            add("--lan-upstream")
            if (prefs.lanTarget > 0) { add("--lan-target"); add(prefs.lanTarget.toString()) }
        }
        // Client SOCKS -> the server in connect.txt (empty: no VPN, connections refused).
        add("--connect-file"); add("${Ygg.CLIENT_SOCKS}:${connectFile.path}")
        // Peers ranked by how well they carried our traffic and by the trackers (re-read live).
        add("--prefer-file"); add(preferFile.path)
        // Without a server, peers are picked by the ping to the trackers (not the root).
        writeSearchFile()
        add("--search-file"); add(searchFile.path)
        // Peer search keeps replacing peers while the ping to the server is above this.
        add("--rtt-limit"); add(prefs.rttLimit.toString())
        if (exit) {
            val p = Ygg.PORT
            // Public exit: any Yggdrasil node may use the port, clients need only the address.
            // PROXY v1 tells the exit who the client is (per-client limits and stats).
            add("--forward"); add("$p:${Ygg.EXIT_SOCKS}")
            add("--open"); add(p.toString())
            add("--proxy-protocol"); add(p.toString())
        }
        if (prefs.extraArgs.isNotBlank()) addAll(prefs.extraArgs.split(Regex("\\s+")))
    }

    private fun restartNode() {
        node?.stop("перезапуск узла"); node = null
        State.lanStatus = null
        State.serverRtt = null
        reconcile()
    }

    private fun nodeLine(l: String) {
        when {
            l.startsWith("device address:") -> State.address = l.substringAfter(':').trim()
            l.startsWith("public key:") -> State.publicKey = l.substringAfter(':').trim()
            l.startsWith("server rtt:") -> State.serverRtt = l.substringAfter(':').trim().substringAfter("] ")
            l.contains("заменён пир") || l.startsWith("замена ") ->
                State.lastSwap = java.text.SimpleDateFormat("HH:mm", java.util.Locale.US).format(java.util.Date()) + " " + l
            // Success prints "joined …" then "[group]:port (listen only, TLS dial)"; keep the first.
            l.startsWith("lan upstream:") -> {
                val v = l.substringAfter(':').trim()
                if (v.startsWith("joined") || v.startsWith("disabled")) State.lanStatus = v
            }
        }
        State.log("node", l)
    }

    private inline fun helperLine(tag: String, l: String, extra: (String) -> Unit) {
        when {
            l.startsWith("STAT ") -> State.parseStat(l)
            l.startsWith("ERROR ") -> State.error(tag, l.removePrefix("ERROR "))
            else -> State.log(tag, l)
        }
        extra(l)
    }

    // ---------------------------------------------------------------------------------------
    // Peers

    private fun refreshPeers(force: Boolean) {
        if (prefs.usePublicPeers) {
            val stale = !publicPeers.isFile || System.currentTimeMillis() - publicPeers.lastModified() > PEERS_MAX_AGE
            if (force || stale) {
                try {
                    Ygg.downloadPeers(publicPeers)
                    State.log("peers", "публичный список загружен (${publicPeers.length() / 1024} КБ)")
                } catch (e: Exception) {
                    State.error("peers", "не удалось скачать публичный список: ${e.message}" +
                        if (publicPeers.isFile) " — используется сохранённый" else "")
                }
            }
        }
        val custom = prefs.customPeerList()
        val pub = if (prefs.usePublicPeers) publicPeers else null
        val fromTrackers = File(filesDir, "tracker-peers.json").takeIf { prefs.useTrackers && it.isFile }
        Ygg.writePeerFile(peerFile, custom, pub, fromTrackers)
        if (custom.isEmpty() && (pub == null || !pub.isFile) && fromTrackers == null) State.error("peers", "нет ни одного пира: добавьте свои или включите публичный список")
    }

    // ---------------------------------------------------------------------------------------
    // VPN

    private fun startVpn(target: Pair<String, Int>) {
        val b = Builder()
            .setSession("Yggdrasil → [${target.first}]")
            .setMtu(1500)
            .addAddress("10.111.222.2", 24)
            .addRoute("0.0.0.0", 0)
            .addDnsServer("10.111.222.53")
            .addAddress("fd59:7967:6772::2", 64)
            .addRoute("::", 0)
        // Our own UID carries the Yggdrasil peer links: it must bypass the VPN.
        b.addDisallowedApplication(packageName)
        for (app in prefs.excludedAppList()) {
            try { b.addDisallowedApplication(app) } catch (e: PackageManager.NameNotFoundException) {
                State.log("vpn", "исключение: приложение $app не найдено")
            }
        }
        if (Build.VERSION.SDK_INT >= 29) b.setMetered(false)
        b.setConfigureIntent(PendingIntent.getActivity(this, 0, Intent(this, MainActivity::class.java), PendingIntent.FLAG_IMMUTABLE))
        val pfd = b.establish() ?: throw IllegalStateException("VPN не установлен (нет разрешения)")
        vpnPfd = pfd
        State.resetTraffic()
        State.vpnTarget = target.first

        File(ctlPath).delete()
        val cmd = buildList {
            add("$bin/libygghelper.so"); add("tun")
            add("--ctl"); add(ctlPath)
            add("--socks"); add("127.0.0.1:${Ygg.CLIENT_SOCKS}")
            if (prefs.udp) { add("--udpgw"); add("127.0.0.1:${Ygg.UDPGW}") }
            if (prefs.ipv6) add("--ipv6")
            add("--mtu"); add("1500")
            add("-v"); add("warn")
        }
        tunProc = ManagedProcess("tun", cmd, filesDir,
            onExit = { State.vpnActive = false; closeCtl() },
            onLine = { l ->
                helperLine("tun", l) {
                    when {
                        it.startsWith("READY tun") -> Thread { sendTunFd() }.start()
                        it == "STATE tun running" -> State.vpnActive = true
                    }
                }
            }).start()
        State.log("vpn", "интерфейс поднят, выход через [${target.first}]:${target.second}")
    }

    /** Hand the VPN fd to the helper over its control socket (SCM_RIGHTS). */
    private fun sendTunFd() {
        val pfd = vpnPfd ?: return
        try {
            closeCtl()
            val s = LocalSocket()
            s.connect(LocalSocketAddress(ctlPath, LocalSocketAddress.Namespace.FILESYSTEM))
            s.setFileDescriptorsForSend(arrayOf(pfd.fileDescriptor))
            s.outputStream.write(1)
            s.outputStream.flush()
            ctl = s // kept open: closing it stops the helper
        } catch (e: Exception) {
            State.error("tun", "не удалось передать VPN-интерфейс: ${e.message}")
        }
    }

    private fun closeCtl() {
        try { ctl?.close() } catch (_: Exception) {}
        ctl = null
    }

    private fun stopVpn() {
        closeCtl()
        tunProc?.stop("VPN выключен"); tunProc = null
        try { vpnPfd?.close() } catch (_: Exception) {}
        vpnPfd = null
        vpnSig = null
        State.vpnActive = false
        State.vpnTarget = null
    }

    private fun stopAll(why: String) {
        stopVpn()
        exitProc?.stop(why); exitProc = null
        node?.stop(why); node = null
        State.exitActive = false
        State.nodeRunning = false
        releaseWakeLock()
        lanOff()
    }

    // ---------------------------------------------------------------------------------------
    // Foreground notification, wake lock

    // LAN discovery: multicast lock + node restart on a new Wi-Fi network
    // (tcp_node joins ff02::114 on the interfaces present at its start).

    private fun lanOn() {
        if (mcastLock?.isHeld != true) {
            val wm = applicationContext.getSystemService(WIFI_SERVICE) as WifiManager
            mcastLock = wm.createMulticastLock("yggproxy:lan").apply { setReferenceCounted(false); acquire() }
        }
        if (wifiCb == null) {
            val cm = getSystemService(CONNECTIVITY_SERVICE) as ConnectivityManager
            val cb = object : ConnectivityManager.NetworkCallback() {
                override fun onAvailable(network: Network) {
                    val prev = wifiNet
                    wifiNet = network
                    // The first callbacks right after registering report networks the node already saw.
                    if (android.os.SystemClock.elapsedRealtime() - registeredAt < 5000 || prev == network) return
                    State.log("lan", "новая сеть Wi-Fi/Ethernet — перезапуск узла для поиска пиров в LAN")
                    try { exec.execute { restartNode() } } catch (_: Exception) {}
                }
                override fun onLost(network: Network) { if (wifiNet == network) wifiNet = null }
            }
            registeredAt = android.os.SystemClock.elapsedRealtime()
            val req = NetworkRequest.Builder().addTransportType(NetworkCapabilities.TRANSPORT_WIFI)
                .addTransportType(NetworkCapabilities.TRANSPORT_ETHERNET).build()
            try { cm.registerNetworkCallback(req, cb); wifiCb = cb } catch (e: Exception) { State.log("lan", "callback: ${e.message}") }
        }
    }

    private fun lanOff() {
        try { if (mcastLock?.isHeld == true) mcastLock?.release() } catch (_: Exception) {}
        mcastLock = null
        wifiCb?.let { cb ->
            try { (getSystemService(CONNECTIVITY_SERVICE) as ConnectivityManager).unregisterNetworkCallback(cb) } catch (_: Exception) {}
        }
        wifiCb = null
        wifiNet = null
    }

    /** Server mode: keep the CPU awake and Wi-Fi out of power save while the screen is off. */
    private fun acquireWakeLock() {
        if (wakeLock?.isHeld != true) {
            val pm = getSystemService(POWER_SERVICE) as PowerManager
            wakeLock = pm.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "yggproxy:exit").apply {
                setReferenceCounted(false); acquire()
            }
        }
        if (wifiLock?.isHeld != true) {
            val wm = applicationContext.getSystemService(WIFI_SERVICE) as WifiManager
            // HIGH_PERF is deprecated on 34+ but still disables Wi-Fi power save; LOW_LATENCY
            // would only apply while the app is in the foreground with the screen on.
            @Suppress("DEPRECATION")
            wifiLock = wm.createWifiLock(WifiManager.WIFI_MODE_FULL_HIGH_PERF, "yggproxy:exit").apply {
                setReferenceCounted(false); acquire()
            }
        }
    }

    private fun releaseWakeLock() {
        try { if (wakeLock?.isHeld == true) wakeLock?.release() } catch (_: Exception) {}
        wakeLock = null
        try { if (wifiLock?.isHeld == true) wifiLock?.release() } catch (_: Exception) {}
        wifiLock = null
    }

    private fun notification(text: String): Notification {
        val nm = getSystemService(NOTIFICATION_SERVICE) as NotificationManager
        if (Build.VERSION.SDK_INT >= 26 && nm.getNotificationChannel(CHANNEL) == null) {
            nm.createNotificationChannel(NotificationChannel(CHANNEL, "Yggdrasil", NotificationManager.IMPORTANCE_LOW))
        }
        val open = PendingIntent.getActivity(this, 0, Intent(this, MainActivity::class.java), PendingIntent.FLAG_IMMUTABLE)
        val stopIntent = Intent(this, YggService::class.java).setAction(ACTION_STOP)
        val stop = if (Build.VERSION.SDK_INT >= 26)
            PendingIntent.getForegroundService(this, 1, stopIntent, PendingIntent.FLAG_IMMUTABLE)
        else PendingIntent.getService(this, 1, stopIntent, PendingIntent.FLAG_IMMUTABLE)
        val b = if (Build.VERSION.SDK_INT >= 26) Notification.Builder(this, CHANNEL) else @Suppress("DEPRECATION") Notification.Builder(this)
        return b.setSmallIcon(R.drawable.ic_notif)
            .setContentTitle("Ratatoskr")
            .setContentText(text)
            .setContentIntent(open)
            .setOngoing(true)
            .addAction(Notification.Action.Builder(null, "Остановить", stop).build())
            .build()
    }

    private fun foreground(text: String) {
        val n = notification(text)
        if (Build.VERSION.SDK_INT >= 34) startForeground(NOTIF_ID, n, ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE)
        else startForeground(NOTIF_ID, n)
    }

    private fun updateNotification() {
        val parts = mutableListOf<String>()
        if (vpnPfd != null) parts += "VPN → ${State.vpnTarget}"
        if (exitProc != null) parts += "сервер выхода"
        if (parts.isEmpty() && node != null) parts += "проверка серверов"
        if (parts.isEmpty()) return
        val nm = getSystemService(NOTIFICATION_SERVICE) as NotificationManager
        nm.notify(NOTIF_ID, notification(parts.joinToString(" · ")))
    }
}
