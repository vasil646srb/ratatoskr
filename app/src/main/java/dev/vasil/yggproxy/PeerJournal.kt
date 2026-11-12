package dev.vasil.yggproxy

import org.json.JSONArray
import org.json.JSONObject
import java.io.File

/**
 * How well each peer actually carried our traffic (same rules as desktop/src/peerstats.rs):
 * every 10 s the node's peers are recorded — link ping, ping to the exit server while the peer
 * carried most of the traffic, speed from real traffic, uptime, drops. The ranking becomes the
 * node's `--prefer-file`; an anonymous summary (no address, no exit server) goes to a tracker.
 */
class PeerJournal(private val file: File) {

    class Rec {
        var uri = ""
        var samples = 0L
        var upS = 0L
        var sessions = 0L
        var drops = 0L
        var rttSum = 0.0
        var rttN = 0L
        var srvSum = 0.0
        var srvN = 0L
        var speedBps = 0.0
        var lastSeen = 0L
        var sessionStart = 0L // not saved

        fun pingMs(): Double? = when {
            srvN >= 3 -> srvSum / srvN
            rttN > 0 -> rttSum / rttN + 60.0
            else -> null
        }

        fun failRate() = if (sessions == 0L) 0.0 else drops.toDouble() / sessions

        fun score(): Double? {
            if (samples < MIN_SAMPLES) return null
            return score(pingMs() ?: return null, failRate(), speedBps * 8 / 1000)
        }
    }

    val peers = HashMap<String, Rec>()
    var lastReport = 0L

    init { load() }

    private fun load() = try {
        val o = JSONObject(file.readText())
        lastReport = o.optLong("last_report")
        val p = o.optJSONObject("peers") ?: JSONObject()
        for (k in p.keys()) {
            val j = p.getJSONObject(k)
            peers[k] = Rec().apply {
                uri = j.optString("uri"); samples = j.optLong("samples"); upS = j.optLong("up_s")
                sessions = j.optLong("sessions"); drops = j.optLong("drops")
                rttSum = j.optDouble("rtt_sum", 0.0); rttN = j.optLong("rtt_n")
                srvSum = j.optDouble("srv_sum", 0.0); srvN = j.optLong("srv_n")
                speedBps = j.optDouble("speed_bps", 0.0); lastSeen = j.optLong("last_seen")
            }
        }
    } catch (_: Exception) {}

    fun save() = try {
        val p = JSONObject()
        for ((k, r) in peers) p.put(k, JSONObject()
            .put("uri", r.uri).put("samples", r.samples).put("up_s", r.upS).put("sessions", r.sessions)
            .put("drops", r.drops).put("rtt_sum", r.rttSum).put("rtt_n", r.rttN).put("srv_sum", r.srvSum)
            .put("srv_n", r.srvN).put("speed_bps", r.speedBps).put("last_seen", r.lastSeen))
        val tmp = File(file.path + ".tmp")
        tmp.writeText(JSONObject().put("peers", p).put("last_report", lastReport).toString())
        tmp.renameTo(file)
    } catch (_: Exception) {}

    /** One tick: `/api/peers` of the node and the current ping to the server (ms, if connected). */
    fun tick(api: JSONObject, serverRtt: Double?) {
        val t = System.currentTimeMillis() / 1000
        val seen = HashSet<String>()
        val list = api.optJSONArray("peers") ?: JSONArray()
        for (i in 0 until list.length()) {
            val p = list.getJSONObject(i)
            if (p.optString("dir") != "out" || p.optBoolean("lan")) continue
            val uri = p.optString("uri")
            if (uri.isEmpty()) continue
            val id = peerId(uri)
            seen += id
            val r = peers.getOrPut(id) { Rec() }
            r.uri = uri
            r.samples++
            r.lastSeen = t
            if (r.sessionStart == 0L) { r.sessionStart = t; r.sessions++ }
            r.upS += 10
            if (!p.isNull("rtt_ms")) { r.rttSum += p.optDouble("rtt_ms"); r.rttN++ }
            val bps = p.optLong("rx_bps") + p.optLong("tx_bps")
            if (p.optDouble("share", 0.0) >= 50.0) {
                if (serverRtt != null) { r.srvSum += serverRtt; r.srvN++ }
                if (bps >= SPEED_MIN_BPS) r.speedBps = if (r.speedBps == 0.0) bps.toDouble() else r.speedBps * 0.8 + bps * 0.2
            }
        }
        for ((id, r) in peers) {
            if (id !in seen && r.sessionStart != 0L) {
                if (t - r.sessionStart < 120) r.drops++
                r.sessionStart = 0L
            }
        }
        peers.values.removeAll { t - it.lastSeen > 30 * 86400 }
    }

    fun ranked(): List<Pair<String, Double>> =
        peers.values.mapNotNull { r -> r.score()?.let { r.uri to it } }.sortedBy { it.second }

    fun report(): JSONObject {
        val a = JSONArray()
        for (r in peers.values.filter { it.samples >= MIN_SAMPLES }) {
            a.put(JSONObject().put("uri", r.uri).put("ping_ms", r.pingMs()?.let { Math.round(it) })
                .put("speed_kbps", Math.round(r.speedBps * 8 / 1000)).put("up_s", r.upS)
                .put("sessions", r.sessions).put("drops", r.drops))
        }
        return JSONObject().put("peers", a)
    }

    /** Priority list for the node: our own best first, then the trackers' ranking. */
    fun preferList(trackerPeers: JSONObject?, max: Int = 12): List<String> {
        val out = ArrayList<String>()
        val ids = HashSet<String>()
        fun push(u: String) { if (u.isNotEmpty() && ids.add(peerId(u))) out += u }
        ranked().take(max).forEach { push(it.first) }
        val a = trackerPeers?.optJSONArray("peers")
        if (a != null) for (i in 0 until minOf(a.length(), max)) {
            if (out.size >= max * 2) break
            push(a.getJSONObject(i).optString("uri"))
        }
        return out
    }

    companion object {
        const val MIN_SAMPLES = 6L
        const val SPEED_MIN_BPS = 20_000L

        fun peerId(uri: String) = uri.substringBefore('?').trim()

        /** Lower is better: the ping that matters, worse with drops, better with speed (×2 at 20 Mbit/s). */
        fun score(pingMs: Double, failRate: Double, speedKbps: Double): Double {
            val mbps = minOf(speedKbps / 1000, 20.0)
            return pingMs * (1 + minOf(failRate, 1.0)) / (1 + mbps / 20)
        }
    }
}
