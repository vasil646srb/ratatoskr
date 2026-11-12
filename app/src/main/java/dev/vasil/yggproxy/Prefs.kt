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

    // Client
    var vpnEnabled: Boolean get() = b("vpn", false); set(v) = put { putBoolean("vpn", v) }
    /** Server: Yggdrasil address (or 64-hex public key) of the exit node. */
    var server: String get() = s("server", ""); set(v) = put { putString("server", v.trim()) }
    var udp: Boolean get() = b("udp", true); set(v) = put { putBoolean("udp", v) }
    var ipv6: Boolean get() = b("ipv6", true); set(v) = put { putBoolean("ipv6", v) }
    var excludedApps: String get() = s("excluded", ""); set(v) = put { putString("excluded", v) }
    /** Saved exit servers (Yggdrasil addresses, one per line); [server] is the selected one. */
    var servers: String get() = s("servers", server); set(v) = put { putString("servers", v) }

    /** Trackers to ask for exit servers (one per line); new ones are learned from their lists. */
    var trackers: String get() = s("trackers", Ygg.DEFAULT_TRACKERS.joinToString("\n")); set(v) = put { putString("trackers", v) }
    /** Keep the Yggdrasil node running in the background (pings servers and root) without VPN/server. */
    var keepNode: Boolean get() = b("keep_node", true); set(v) = put { putBoolean("keep_node", v) }
    var useTrackers: Boolean get() = b("use_trackers", true); set(v) = put { putBoolean("use_trackers", v) }
    fun trackerList(): List<String> = trackers.lines().map { it.trim() }.filter { it.isNotEmpty() && !it.startsWith("#") }.distinct()

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
    var target: Int get() = i("target", 3); set(v) = put { putInt("target", v.coerceIn(1, 32)) }
    /** tcp_node --lan-upstream: find yggdrasil-go peers on the LAN by their beacons, dial over TLS. */
    var lanDiscovery: Boolean get() = b("lan", true); set(v) = put { putBoolean("lan", v) }
    /** --lan-target: outbound slots for LAN peers; 0 = same as target. */
    var lanTarget: Int get() = i("lan_target", 0); set(v) = put { putInt("lan_target", v.coerceIn(0, 32)) }
    /** Client: tcp_node --rtt-limit, replace peers while the ping to the server is above this. */
    var rttLimit: Int get() = i("rtt_limit", 100); set(v) = put { putInt("rtt_limit", v.coerceIn(10, 5000)) }
    /** tcp_node --swap-interval: seconds between two peer replacements. */
    var swapInterval: Int get() = i("swap_interval", 30); set(v) = put { putInt("swap_interval", v.coerceIn(5, 3600)) }
    /** tcp_node --swap-settle: seconds a new peer is measured before the replacement is judged. */
    var swapSettle: Int get() = i("swap_settle", 60); set(v) = put { putInt("swap_settle", v.coerceIn(10, 3600)) }
    var extraArgs: String get() = s("extra", ""); set(v) = put { putString("extra", v.trim()) }

    fun customPeerList(): List<String> = customPeers.lines().map { it.trim() }.filter { it.isNotEmpty() && !it.startsWith("#") }
    fun excludedAppList(): List<String> = excludedApps.split(',', '\n', ' ').map { it.trim() }.filter { it.isNotEmpty() }
}
