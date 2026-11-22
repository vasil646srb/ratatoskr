package dev.vasil.yggproxy

import android.content.Context
import android.content.SharedPreferences

/** All settings. Read by the activity and the service (same process). */
class Prefs(ctx: Context) {
    private val p: SharedPreferences = ctx.applicationContext.getSharedPreferences("ygg", Context.MODE_PRIVATE)

    private fun b(k: String, d: Boolean) = p.getBoolean(k, d)
    private fun s(k: String, d: String) = p.getString(k, d) ?: d
    private fun i(k: String, d: Int) = p.getInt(k, d)
    private fun put(f: SharedPreferences.Editor.() -> Unit) = p.edit().apply(f).apply()

    /** Update check (GitHub releases): when it last ran, and a version the user chose to skip. */
    var updateCheckedAt: Long get() = p.getLong("update_at", 0); set(v) = put { putLong("update_at", v) }
    var updateSkipped: String get() = s("update_skip", ""); set(v) = put { putString("update_skip", v) }

    // Client
    var vpnEnabled: Boolean get() = b("vpn", false); set(v) = put { putBoolean("vpn", v) }
    /** Server: Yggdrasil address (or 64-hex public key) of the exit node. */
    var server: String get() = s("server", ""); set(v) = put { putString("server", v.trim()) }
    var udp: Boolean get() = b("udp", true); set(v) = put { putBoolean("udp", v) }
    var ipv6: Boolean get() = b("ipv6", true); set(v) = put { putBoolean("ipv6", v) }
    /** Yggdrasil sites (200::/7) and ALFIS names (.ygg, .anon…) through our own node, next to the internet. */
    var yggSites: Boolean get() = b("ygg_sites", true); set(v) = put { putBoolean("ygg_sites", v) }
    /** VPN only for Yggdrasil sites, without an exit server. */
    var yggOnly: Boolean get() = b("ygg_only", false); set(v) = put { putBoolean("ygg_only", v) }
    /** VPN as a crypto-key routing tunnel (every IP packet, UDP included), Yggdrasil-ng compatible. */
    var ckr: Boolean get() = b("ckr", false); set(v) = put { putBoolean("ckr", v) }
    /** CKR by itself whenever the exit offers it (else the proxy); such exits are picked first. */
    var ckrAuto: Boolean get() = b("ckr_auto", true); set(v) = put { putBoolean("ckr_auto", v) }
    /** The VPN through [server] goes through CKR. */
    fun ckrFor(server: String?): Boolean = server != null && (ckr || (ckrAuto && server in State.ckrServers))
    /** Prefer exits with CKR when picking one. */
    val ckrWanted: Boolean get() = ckr || ckrAuto
    /** Our IPv4 address in the CKR tunnel. */
    var ckrAddress: String get() = s("ckr_address", "10.99.0.2"); set(v) = put { putString("ckr_address", v.trim()) }
    /** DNS server used through the CKR tunnel. */
    var ckrDns: String get() = s("ckr_dns", "1.1.1.1"); set(v) = put { putString("ckr_dns", v.trim()) }
    var excludedApps: String get() = s("excluded", ""); set(v) = put { putString("excluded", v) }
    /** Saved exit servers (Yggdrasil addresses, one per line); [server] is the selected one. */
    var servers: String get() = s("servers", server); set(v) = put { putString("servers", v) }

    /** Trackers to ask for exit servers (one per line); new ones are learned from their lists. */
    var trackers: String get() = s("trackers", Ygg.DEFAULT_TRACKERS.joinToString("\n")); set(v) = put { putString("trackers", v) }
    /** Keep the Yggdrasil node running in the background (pings servers and root) without VPN/server. */
    var keepNode: Boolean get() = b("keep_node", true); set(v) = put { putBoolean("keep_node", v) }
    /** The full app instead of the one-button simple screen. */
    var advancedMode: Boolean get() = b("advanced", false); set(v) = put { putBoolean("advanced", v) }
    var useTrackers: Boolean get() = b("use_trackers", true); set(v) = put { putBoolean("use_trackers", v) }
    /** Saved trackers plus the built-in ones (always asked, also on lists saved by older versions). */
    fun trackerList(): List<String> =
        (Ygg.DEFAULT_TRACKERS + trackers.lines().map { it.trim() }.filter { it.isNotEmpty() && !it.startsWith("#") }).distinct()

    /** The node's ping list: servers, the trackers' exits and the trackers (pings keep their routes warm). */
    fun monitorList(): List<String> =
        (serverList() + State.trackerExits.map { it.first } + (if (useTrackers) trackerList() else emptyList())).distinct()

    /** Access keys of closed servers, one `address key` per line. */
    var serverKeys: String get() = s("server_keys", ""); set(v) = put { putString("server_keys", v) }
    fun keyFor(addr: String): String? =
        serverKeys.lines().map { it.trim().split(' ') }.firstOrNull { it.size == 2 && it[0] == addr }?.get(1)
    fun setKey(addr: String, key: String?) {
        val rest = serverKeys.lines().filter { it.isNotBlank() && it.trim().split(' ')[0] != addr }
        serverKeys = (rest + listOfNotNull(key?.takeIf { it.isNotEmpty() }?.let { "$addr $it" })).joinToString("\n")
    }

    /** Our access requests the owners have not answered yet (addresses, one per line). */
    var pendingRequests: String get() = s("pending_req", ""); set(v) = put { putString("pending_req", v) }
    fun pendingList(): List<String> = pendingRequests.lines().map { it.trim() }.filter { it.isNotEmpty() }.distinct()
    fun addPending(addr: String) { if (addr !in pendingList()) pendingRequests = (pendingList() + addr).joinToString("\n") }
    fun removePending(addr: String) { pendingRequests = pendingList().filter { it != addr }.joinToString("\n") }

    /**
     * Servers that let this device in personally without a key we hold: the owner approved our
     * request (the access is bound to our address). They count as ours like the keyed ones: used
     * first, with their own peer.
     */
    private var grantedText: String get() = s("granted_servers", ""); set(v) = put { putString("granted_servers", v) }
    fun grantedList(): List<String> = grantedText.lines().map { it.trim() }.filter { it.isNotEmpty() }.distinct()
    fun isGranted(addr: String): Boolean = addr in grantedList()
    /** Ours: a key, or access the owner gave us. */
    fun isOwn(addr: String): Boolean = keyFor(addr) != null || isGranted(addr)

    /**
     * What a server answered about our access (see [Ygg.probeAccess]). Access with an end date is
     * personal (a key or an approved request; a public server has none): the server becomes ours
     * and the peer it announced is kept for it. A refusal takes the mark away.
     */
    fun noteAccess(addr: String, code: String, expires: Long) {
        val granted = grantedList()
        when {
            code == "ok" && expires > 0 -> {
                if (addr !in granted) grantedText = (granted + addr).joinToString("\n")
                State.serverPeers[addr]?.let { if (it != serverPeers(addr)) setServerPeers(addr, it) }
            }
            code in NO_ACCESS && addr in granted -> grantedText = granted.filter { it != addr }.joinToString("\n")
        }
        // The name it told us, kept for the lists (also when it has none any more); and whether
        // its owner takes chat messages.
        if (!code.startsWith("error")) {
            val chat = addr in State.chatServers
            if (chat != isChatServer(addr)) chatServersText = (chatServerList().filter { it != addr } + listOfNotNull(addr.takeIf { chat })).joinToString("\n")
            val n = State.serverNames[addr] ?: ""
            val m = mapOf(serverNamesText)
            if ((m[addr] ?: "") != n) serverNamesText = mapText(if (n.isEmpty()) m - addr else m + (addr to n))
        }
    }

    /** Servers whose owner takes chat messages (from their access answers). */
    private var chatServersText: String get() = s("chat_servers", ""); set(v) = put { putString("chat_servers", v) }
    private fun chatServerList() = chatServersText.lines().map { it.trim() }.filter { it.isNotEmpty() }
    fun isChatServer(addr: String) = addr in chatServerList()

    /** Exit mode: what clients see instead of our address. */
    var serverName: String get() = s("server_name", ""); set(v) = put { putString("server_name", v.trim().take(40)) }

    /** Names the servers told us (one `address name` per line). */
    private var serverNamesText: String get() = s("server_names", ""); set(v) = put { putString("server_names", v) }
    private fun mapOf(text: String): Map<String, String> =
        text.lines().mapNotNull { l -> l.indexOf(' ').takeIf { it > 0 }?.let { l.substring(0, it) to l.substring(it + 1) } }.toMap()
    private fun mapText(m: Map<String, String>) = m.entries.joinToString("\n") { "${it.key} ${it.value}" }
    fun serverNameOf(addr: String): String? = State.serverNames[addr] ?: mapOf(serverNamesText)[addr]?.takeIf { it.isNotEmpty() }
    /** The server as people know it: its name, else its address. */
    fun label(addr: String): String = serverNameOf(addr) ?: addr

    /** Chat: the newest message time seen (opened) and notified about, per server. */
    private var chatSeenText: String get() = s("chat_seen", ""); set(v) = put { putString("chat_seen", v) }
    private var chatNotifiedText: String get() = s("chat_notified", ""); set(v) = put { putString("chat_notified", v) }
    fun chatSeen(addr: String): Long = mapOf(chatSeenText)[addr]?.toLongOrNull() ?: 0L
    fun setChatSeen(addr: String, t: Long) { if (t > chatSeen(addr)) chatSeenText = mapText(mapOf(chatSeenText) + (addr to t.toString())) }
    fun chatNotified(addr: String): Long = maxOf(chatSeen(addr), mapOf(chatNotifiedText)[addr]?.toLongOrNull() ?: 0L)
    fun setChatNotified(addr: String, t: Long) { chatNotifiedText = mapText(mapOf(chatNotifiedText) + (addr to t.toString())) }
    /** Exit mode: client messages seen / notified (newest time). */
    var ownerChatSeen: Long get() = s("owner_chat_seen", "0").toLongOrNull() ?: 0L; set(v) = put { putString("owner_chat_seen", v.toString()) }

    fun serverList(): List<String> = servers.lines().map { it.trim() }.filter { it.isNotEmpty() }.distinct()
    fun addServer(addr: String) { if (addr !in serverList()) servers = (serverList() + addr).joinToString("\n") }
    fun removeServer(addr: String) { servers = serverList().filter { it != addr }.joinToString("\n") }

    // Exit server
    var serverEnabled: Boolean get() = b("srv", false); set(v) = put { putBoolean("srv", v) }
    /** Speed limits of the exit, kbit/s (0 = none): whole server and each client, down/up. */
    var limTotalDown: Int get() = i("lim_total_down", 0); set(v) = put { putInt("lim_total_down", v.coerceAtLeast(0)) }
    var limTotalUp: Int get() = i("lim_total_up", 0); set(v) = put { putInt("lim_total_up", v.coerceAtLeast(0)) }
    var limClientDown: Int get() = i("lim_client_down", 0); set(v) = put { putInt("lim_client_down", v.coerceAtLeast(0)) }
    var limClientUp: Int get() = i("lim_client_up", 0); set(v) = put { putInt("lim_client_up", v.coerceAtLeast(0)) }

    /** ygghelper --limits file contents (re-read live by the exit). */
    fun limitsText() = "total_down=$limTotalDown\ntotal_up=$limTotalUp\nclient_down=$limClientDown\nclient_up=$limClientUp\n"
    var allowLan: Boolean get() = b("allow_lan", false); set(v) = put { putBoolean("allow_lan", v) }
    var autostart: Boolean get() = b("autostart", false); set(v) = put { putBoolean("autostart", v) }

    // Yggdrasil network
    var usePublicPeers: Boolean get() = b("public", true); set(v) = put { putBoolean("public", v) }
    var customPeers: String get() = s("peers", ""); set(v) = put { putString("peers", v) }
    /** Accept Yggdrasil peers on this TCP port (0: off): clients can peer with us directly. */
    var listenPort: Int get() = p.getInt("listen_port", 0); set(v) = put { putInt("listen_port", v) }
    /** Our address(es) for clients (public IP or name), comma separated; empty: the device's own. */
    var peerHost: String get() = s("peer_host", ""); set(v) = put { putString("peer_host", v.trim()) }
    /**
     * The exits' own Yggdrasil peers (from their links or their access answer), one
     * `address uri uri…` per line. While the chosen exit's peer is reachable it is our only peer.
     */
    private var serverPeerText: String get() = s("server_peers", ""); set(v) = put { putString("server_peers", v) }
    fun serverPeers(addr: String): List<String> =
        serverPeerText.lines().map { it.trim().split(' ') }.firstOrNull { it.size >= 2 && it[0] == addr }?.drop(1) ?: emptyList()
    fun setServerPeers(addr: String, peers: List<String>) {
        if (peers.isEmpty()) return
        val rest = serverPeerText.lines().filter { it.isNotBlank() && it.trim().split(' ')[0] != addr }
        serverPeerText = (rest + "$addr ${peers.joinToString(" ")}").joinToString("\n")
    }
    var target: Int get() = i("target", 3); set(v) = put { putInt("target", v.coerceIn(1, 32)) }
    /** tcp_node --lan-upstream: find yggdrasil-go peers on the LAN by their beacons, dial over TLS. */
    var lanDiscovery: Boolean get() = b("lan", true); set(v) = put { putBoolean("lan", v) }
    /** --lan-target: outbound slots for LAN peers; 0 = same as target. */
    var lanTarget: Int get() = i("lan_target", 0); set(v) = put { putInt("lan_target", v.coerceIn(0, 32)) }
    /** Client: tcp_node --rtt-limit, replace peers while the ping to the server is above this. */
    /** Peer search threshold: 250 ms (100 ms, the old default, swapped peers too eagerly). */
    var rttLimit: Int get() = i("rtt_limit", 250).let { if (it == 100) 250 else it }; set(v) = put { putInt("rtt_limit", v.coerceIn(10, 5000)) }
    /** tcp_node --swap-interval: seconds between two peer replacements. */
    var swapInterval: Int get() = i("swap_interval", 30); set(v) = put { putInt("swap_interval", v.coerceIn(5, 3600)) }
    /** tcp_node --swap-settle: seconds a new peer is measured before the replacement is judged. */
    var swapSettle: Int get() = i("swap_settle", 60); set(v) = put { putInt("swap_settle", v.coerceIn(10, 3600)) }
    var extraArgs: String get() = s("extra", ""); set(v) = put { putString("extra", v.trim()) }

    fun customPeerList(): List<String> = customPeers.lines().map { it.trim() }.filter { it.isNotEmpty() && !it.startsWith("#") }
    fun excludedAppList(): List<String> = excludedApps.split(',', '\n', ' ').map { it.trim() }.filter { it.isNotEmpty() }
}

/** Access answers that mean this device is not let in (any more). */
private val NO_ACCESS = setOf("bad_key", "expired", "denied", "closed", "other_device", "pending")
