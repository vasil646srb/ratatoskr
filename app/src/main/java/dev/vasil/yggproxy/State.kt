package dev.vasil.yggproxy

import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale

/** In-process shared state: the service writes it, the activity polls it. */
object State {
    private const val MAX_LOG = 2000
    private val logs = ArrayDeque<String>()
    private val fmt = SimpleDateFormat("HH:mm:ss", Locale.US)

    @Volatile var serviceRunning = false
    /** The app's window is on screen: the node runs to ping the saved servers and the root. */
    @Volatile var uiVisible = false
    /** Screens of the app currently started (the simple and the full one hand over to each other). */
    private val uiCount = java.util.concurrent.atomic.AtomicInteger(0)
    fun uiShown() { uiCount.incrementAndGet(); uiVisible = true }
    fun uiHidden() { uiVisible = uiCount.decrementAndGet().coerceAtLeast(0) > 0 }
    /** «Остановить» in the notification: no background node until the app is opened again. */
    @Volatile var stoppedByUser = false
    /** Exit servers from the trackers: address -> rtt as the tracker measured it (ms, -1 unknown). */
    @Volatile var trackerExits: List<Pair<String, Long>> = emptyList()
    /** Exits the trackers list as public (anyone may use them); the rest need a key. */
    @Volatile var trackerPublic: Set<String> = emptySet()
    /** Client: access to the selected server — (server, `ok`/`pending`/… or `error: …`). */
    @Volatile var access: Triple<String, String, Long>? = null
    @Volatile var trackerStatus: String = "ещё не запрашивались"
    @Volatile var trackerAt = 0L
    /** Last time public peers were taken from a tracker (hourly). */
    @Volatile var trackerPeersAt = 0L
    @Volatile var nodeRunning = false
    @Volatile var nodeRestarts = 0
    @Volatile var nodeStartedAt = 0L
    @Volatile var address: String? = null
    @Volatile var publicKey: String? = null
    /** tcp_node `lan upstream:` line: joined interfaces or why discovery is off. */
    @Volatile var lanStatus: String? = null
    /** tcp_node `server rtt:` line (client): echo latency to the exit node. */
    @Volatile var serverRtt: String? = null
    /** Last peer replacement by the peer search. */
    @Volatile var lastSwap: String? = null

    @Volatile var vpnActive = false
    @Volatile var vpnTarget: String? = null
    @Volatile var tunTx = 0L
    @Volatile var tunRx = 0L

    @Volatile var exitActive = false
    @Volatile var srvConns = 0L
    @Volatile var srvTotal = 0L
    @Volatile var srvDenied = 0L
    @Volatile var srvTx = 0L
    @Volatile var srvRx = 0L

    /** Exit clients from `STAT client ADDR active=N tx=N rx=N`: address -> (active, tx, rx, seen ms). */
    val clients = java.util.concurrent.ConcurrentHashMap<String, LongArray>()

    @Volatile var lastError: String? = null

    fun log(tag: String, msg: String) {
        val line = "${fmt.format(Date())} [$tag] $msg"
        synchronized(logs) {
            logs.addLast(line)
            while (logs.size > MAX_LOG) logs.removeFirst()
        }
    }

    fun error(tag: String, msg: String) {
        lastError = "[$tag] $msg"
        log(tag, "ОШИБКА: $msg")
    }

    fun logsText(): String = synchronized(logs) { logs.joinToString("\n") }
    fun clearLogs() = synchronized(logs) { logs.clear() }

    /** `STAT tun tx=N rx=N` / `STAT server active=N total=N denied=N tx=N rx=N`. */
    fun parseStat(line: String) {
        val kv = line.split(' ').mapNotNull { t -> t.split('=', limit = 2).takeIf { it.size == 2 }?.let { it[0] to (it[1].toLongOrNull() ?: 0L) } }.toMap()
        when {
            line.startsWith("STAT tun") -> { tunTx = kv["tx"] ?: tunTx; tunRx = kv["rx"] ?: tunRx }
            line.startsWith("STAT client ") -> {
                val addr = line.split(' ').getOrNull(2) ?: return
                clients[addr] = longArrayOf(kv["active"] ?: 0, kv["tx"] ?: 0, kv["rx"] ?: 0, System.currentTimeMillis())
            }
            line.startsWith("STAT server") -> {
                srvConns = kv["active"] ?: 0; srvTotal = kv["total"] ?: 0; srvDenied = kv["denied"] ?: 0
                srvTx = kv["tx"] ?: 0; srvRx = kv["rx"] ?: 0
            }
        }
    }

    fun resetTraffic() { tunTx = 0; tunRx = 0 }
}
