package dev.vasil.yggproxy

import org.json.JSONObject
import java.io.File
import java.io.InputStream
import java.net.Inet6Address
import java.net.InetAddress
import java.net.InetSocketAddress
import java.net.Socket
import java.net.URL
import javax.net.ssl.HttpsURLConnection

object Ygg {
    // Local ports, all on 127.0.0.1.
    const val CLIENT_SOCKS = 10808     // tcp_node --connect 10808:[server]:PORT
    const val EXIT_SOCKS = 10800       // ygghelper server, exposed as --forward PORT:10800
    const val UDPGW = 7300             // udpgw on the exit node (as seen by its SOCKS server)
    const val SPEEDTEST = 7301         // speed test on the exit node (ygghelper server --speedtest)
    const val NODE_SOCKS = 10810       // tcp_node --socks: SOCKS5 into Yggdrasil (tracker requests)
    const val TRACKER_PORT = 27081     // tracker API on the overlay
    /** Built-in tracker (the project's server); more are learned from its /trackers. */
    val DEFAULT_TRACKERS = listOf("200:379c:ac8b:ad9f:2fbd:c38c:4be:51a0")
    const val API = 19080              // tcp_node --api
    const val HTML_PORT = 1081         // tcp_node overlay status page (kept off the proxy port)

    const val PUBLIC_PEERS_URL = "https://publicpeers.neilalexander.dev/publicnodes.json"

    private val HEX64 = Regex("^[0-9a-fA-F]{64}$")
    fun isKey(s: String) = HEX64.matches(s)

    /** Yggdrasil address of a public key (yggdrasil-go address.AddrForKey). */
    fun addrForKey(hex: String): Inet6Address {
        val key = ByteArray(32) { hex.substring(it * 2, it * 2 + 2).toInt(16).toByte() }
        val buf = ByteArray(32) { (key[it].toInt() xor 0xff).toByte() }
        val temp = ArrayList<Byte>()
        var done = false
        var ones = 0
        var bits = 0
        var nBits = 0
        for (idx in 0 until 256) {
            val bit = (buf[idx / 8].toInt() shr (7 - idx % 8)) and 1
            if (!done && bit != 0) { ones++; continue }
            if (!done && bit == 0) { done = true; continue }
            bits = ((bits shl 1) or bit) and 0xff
            if (++nBits == 8) { nBits = 0; temp.add(bits.toByte()) }
        }
        val a = ByteArray(16)
        a[0] = 0x02
        a[1] = ones.toByte()
        for (i in 0 until minOf(14, temp.size)) a[2 + i] = temp[i]
        return InetAddress.getByAddress(a) as Inet6Address
    }

    fun isYgg(a: Inet6Address) = (a.address[0].toInt() and 0xfe) == 0x02

    /** `addr`, `[addr]`, `[addr]:port` or a 64-hex key → (address, port or null). */
    fun parseServer(input: String): Pair<Inet6Address, Int?>? {
        val s = input.trim()
        if (isKey(s)) return addrForKey(s) to null
        var host = s
        var port: Int? = null
        if (s.startsWith("[")) {
            val end = s.indexOf(']')
            if (end < 0) return null
            host = s.substring(1, end)
            val rest = s.substring(end + 1)
            if (rest.startsWith(":")) port = rest.substring(1).toIntOrNull() ?: return null
        }
        if (!host.contains(':') || !host.all { it.isLetterOrDigit() || it == ':' }) return null
        val a = try { InetAddress.getByName(host) } catch (e: Exception) { return null }
        if (a !is Inet6Address || !isYgg(a)) return null
        return a to port
    }

    /** Overlay port of the proxy, the same on every server (not the common SOCKS 1080). */
    const val PORT = 27080

    /** Quick-connect link shown as a QR code: yggproxy://c?a=ADDR&k=KEY (the port is always [PORT]). */
    fun connectLink(addr: String, key: String? = null) = "yggproxy://c?a=$addr" + (key?.let { "&k=$it" } ?: "")

    /** Keys are typed by hand: case, spaces and dashes don't matter. */
    fun normKey(s: String) = s.filter { it.isLetterOrDigit() && it.code < 128 }.lowercase()

    /** The access key in a `yggproxy://c?a=…&k=KEY` link, if any. */
    fun keyFromText(text: String): String? {
        val t = text.trim()
        if (!t.startsWith("yggproxy:", ignoreCase = true)) return null
        return android.net.Uri.parse(t).getQueryParameter("k")?.let { normKey(it) }?.takeIf { it.isNotEmpty() }
    }

    /** This device's name, sent with a key or an access request. */
    fun deviceName(): String = "${android.os.Build.MANUFACTURER} ${android.os.Build.MODEL}".trim().take(40)

    /**
     * Access to a closed exit (see ygghelper access.rs), through the node's SOCKS (no VPN needed):
     * code `ok`, `pending` (a request is with the owner), `bad_key`, `expired`, `other_device`,
     * `closed` (no key and [ask] off: nothing was sent), or `error: …`; and when the access ends
     * (unix seconds, 0: public or unknown). With [ask] a missing key becomes a request to the
     * owner, with [message] for them.
     */
    fun probeAccess(server: String, key: String?, ask: Boolean, message: String = ""): Pair<String, Long> = try {
        val a = (InetAddress.getByName(server) as Inet6Address).address
        Socket().use { s ->
            s.connect(InetSocketAddress("127.0.0.1", NODE_SOCKS), 1000)
            s.soTimeout = 40000
            val o = s.getOutputStream()
            val i = s.getInputStream()
            o.write(byteArrayOf(5, 1, 0)); readN(i, 2)
            o.write(byteArrayOf(5, 1, 0, 4) + a + byteArrayOf((PORT shr 8).toByte(), (PORT and 0xff).toByte()))
            if (readN(i, 22)[1].toInt() != 0) return "error: сервер недоступен" to 0L
            val user = (if (message.isBlank()) deviceName() else deviceName() + "\u001f" + message.trim().replace('\n', ' '))
                .toByteArray().let { it.copyOf(minOf(it.size, 255)) }
            val k = (key ?: "").toByteArray()
            // 0x80: our own method (status and the key's end); 2 for older servers.
            o.write(byteArrayOf(5, 3, 0x80.toByte(), 2, 0))
            when (readN(i, 2)[1].toInt() and 0xff) {
                0 -> "ok" to 0L
                0x80 -> {
                    o.write(byteArrayOf(1, if (ask) 1 else 0, user.size.toByte()) + user + byteArrayOf(k.size.toByte()) + k)
                    val r = readN(i, 10)
                    val exp = java.nio.ByteBuffer.wrap(r, 2, 8).long
                    codeName(r[1].toInt()) to exp
                }
                2 -> {
                    if (k.isEmpty() && !ask) return "closed" to 0L
                    o.write(byteArrayOf(1, user.size.toByte()) + user + byteArrayOf(k.size.toByte()) + k)
                    codeName(readN(i, 2)[1].toInt()) to 0L
                }
                else -> "closed" to 0L
            }
        }
    } catch (e: Exception) { "error: ${e.message ?: e.javaClass.simpleName}" to 0L }

    private fun codeName(c: Int) = when (c) {
        0 -> "ok"; 1 -> "pending"; 2 -> "bad_key"; 3 -> "expired"; 4 -> "other_device"; 5 -> "closed"
        else -> "error: неизвестный ответ $c"
    }

    /** " · ключ ещё 12д 3ч" for an access that ends (0: no end date). */
    fun keyLeft(expires: Long): String {
        if (expires <= 0) return ""
        val left = expires - System.currentTimeMillis() / 1000
        return if (left > 0) " · ключ ещё ${dur(left)}" else " · ключ истёк"
    }

    fun accessText(code: String?): String = when {
        code == null -> "проверяю…"
        code == "ok" -> "доступ есть"
        code == "pending" -> "нет доступа — запрос отправлен владельцу сервера, ждите одобрения"
        code == "bad_key" -> "неверный ключ доступа"
        code == "expired" -> "срок ключа истёк — попросите владельца продлить"
        code == "other_device" -> "ключ уже привязан к другому устройству"
        code == "closed" -> "сервер закрыт: нужен ключ или запрос доступа"
        else -> "не проверен (${code.removePrefix("error: ")})"
    }

    /**
     * Keys and requests of our own exit: `libygghelper.so access FILE ARGS…` (one implementation
     * with the exit itself, which writes the same file). The whole file as JSON, or an error.
     */
    fun accessCmd(ctx: android.content.Context, vararg args: String): Result<JSONObject> = try {
        val p = ProcessBuilder(listOf("${ctx.applicationInfo.nativeLibraryDir}/libygghelper.so", "access", File(ctx.filesDir, "access.json").path) + args)
            .redirectErrorStream(false).start()
        val out = p.inputStream.bufferedReader().readText().trim()
        p.waitFor()
        val line = out.lines().lastOrNull { it.isNotBlank() } ?: ""
        if (line.startsWith("ERROR ")) Result.failure(Exception(line.removePrefix("ERROR "))) else Result.success(JSONObject(line))
    } catch (e: Exception) { Result.failure(e) }

    /** Shown before the owner makes the exit public (same text as the desktop). */
    const val PUBLIC_RISK = "Публичный сервер может использовать любой, кто найдёт его через трекер.\n" +
        "• Весь их трафик выходит в интернет с вашего IP-адреса: за нарушения закона, спам, взломы, пиратство и блокировки отвечать придётся вам — жалобы и запросы придут вашему провайдеру.\n" +
        "• Чужие люди займут ваш канал и могут исчерпать лимит трафика.\n" +
        "• Сайты и сервисы могут заблокировать ваш IP или показывать капчу.\n" +
        "• Вы не узнаете, кто именно пользуется сервером.\n" +
        "Включайте, только если понимаете и принимаете эти последствия."

    /** Server from a scanned QR / opened link: the yggproxy:// link, `[addr]:port`, `addr` or a key. */
    fun serverFromText(text: String): String? {
        val t = text.trim()
        if (t.startsWith("yggproxy:", ignoreCase = true)) {
            val u = android.net.Uri.parse(t)
            val a = u.getQueryParameter("a") ?: return null
            return parseServer(a)?.let { fmtAddr(it.first) }
        }
        return parseServer(t)?.let { fmtAddr(it.first) }
    }

    fun fmtAddr(a: Inet6Address): String = a.hostAddress!!.substringBefore('%').replace(Regex("(^|:)0+([0-9a-f])"), "$1$2")

    fun human(b: Long): String = when {
        b < 1024 -> "$b B"
        b < 1024 * 1024 -> "%.1f KB".format(b / 1024.0)
        b < 1024L * 1024 * 1024 -> "%.1f MB".format(b / 1048576.0)
        else -> "%.2f GB".format(b / 1073741824.0)
    }

    fun dur(s: Long): String = when {
        s < 60 -> "${s}с"
        s < 3600 -> "${s / 60}м ${s % 60}с"
        s < 86400 -> "${s / 3600}ч ${s % 3600 / 60}м"
        else -> "${s / 86400}д ${s % 86400 / 3600}ч"
    }

    /** GET /api/stats from tcp_node (raw HTTP: no cleartext policy involved). */
    fun fetchStats(): JSONObject? = fetchJson("/api/stats")

    /**
     * One HTTP request to [tracker]:27081 through tcp_node --socks (the app itself is outside the
     * VPN). Returns the whole response text, or null when the tracker cannot be reached.
     */
    private fun trackerRequest(tracker: String, request: ByteArray): String? = try {
        val a = (InetAddress.getByName(tracker) as Inet6Address).address
        Socket().use { s ->
            s.connect(InetSocketAddress("127.0.0.1", NODE_SOCKS), 1000)
            // The first connection to a new address waits for path discovery: allow 40 s.
            s.soTimeout = 40000
            val o = s.getOutputStream()
            val i = s.getInputStream()
            o.write(byteArrayOf(5, 1, 0)); readN(i, 2)
            o.write(byteArrayOf(5, 1, 0, 4) + a + byteArrayOf((TRACKER_PORT shr 8).toByte(), (TRACKER_PORT and 0xff).toByte()))
            if (readN(i, 22)[1].toInt() != 0) return null
            o.write(request)
            i.readBytes().toString(Charsets.UTF_8)
        }
    } catch (e: Exception) { null }

    private fun jsonBody(response: String?): JSONObject? = try {
        response?.substringAfter("\r\n\r\n", "")?.takeIf { it.isNotEmpty() }?.let { JSONObject(it) }
    } catch (e: Exception) { null }

    /** GET [tracker]:27081/PATH. */
    fun fetchTracker(tracker: String, path: String): JSONObject? =
        jsonBody(trackerRequest(tracker, "GET $path HTTP/1.0\r\nHost: [$tracker]\r\n\r\n".toByteArray()))

    /** POST [tracker]:27081/PATH with a JSON body. The reply, or null unless the tracker said 200. */
    fun postTracker(tracker: String, path: String, body: JSONObject): JSONObject? {
        val b = body.toString().toByteArray()
        val head = "POST $path HTTP/1.0\r\nHost: [$tracker]\r\nContent-Type: application/json\r\nContent-Length: ${b.size}\r\n\r\n"
        val text = trackerRequest(tracker, head.toByteArray() + b) ?: return null
        return if (text.startsWith("HTTP/1.0 200")) jsonBody(text) ?: JSONObject() else null
    }

    /** A tracker's `/exits` reply into [into]: address -> ping the tracker measured (-1 unknown). */
    fun parseExits(reply: JSONObject, into: MutableMap<String, Long>, public: MutableSet<String>? = null) {
        val a = reply.optJSONArray("exits") ?: return
        for (k in 0 until a.length()) {
            val o = a.getJSONObject(k)
            serverFromText(o.optString("address"))?.let {
                into.putIfAbsent(it, if (o.isNull("rtt_ms")) -1 else o.optLong("rtt_ms"))
                if (o.optBoolean("public")) public?.add(it)
            }
        }
    }

    /** `/api/rtt` of the node: its ping entry for every monitored address (servers, trackers). */
    fun rttByAddr(rtt: JSONObject?): Map<String, JSONObject> {
        val a = rtt?.optJSONArray("servers") ?: return emptyMap()
        val out = HashMap<String, JSONObject>()
        for (k in 0 until a.length()) a.getJSONObject(k).let { o -> serverFromText(o.optString("address"))?.let { out[it] = o } }
        return out
    }

    /** The node's ping list (re-read live): written only when it changed. */
    fun writeMonitors(dir: File, prefs: Prefs) = writeIfChanged(File(dir, "monitors.txt"), prefs.monitorList().joinToString("\n"))

    /** Files the node re-reads are compared by content: no write when nothing changed. */
    fun writeIfChanged(f: File, text: String): Boolean = try {
        if (f.exists() && f.readText() == text) false else { f.writeText(text); true }
    } catch (e: Exception) { false }

    /** GET /api/rtt: ping to the saved servers (monitor list) and to the root. */
    fun fetchRtt(): JSONObject? = fetchJson("/api/rtt")

    /** GET /api/peers: connected peers with session traffic, rates and share of traffic now. */
    fun fetchPeers(): JSONObject? = fetchJson("/api/peers")

    fun mbit(bytesPerSec: Long): String = when {
        bytesPerSec < 125 -> "${bytesPerSec * 8} бит/с"
        bytesPerSec < 125_000 -> "%.1f кбит/с".format(bytesPerSec * 8 / 1000.0)
        else -> "%.1f Мбит/с".format(bytesPerSec * 8 / 1_000_000.0)
    }

    private fun fetchJson(path: String): JSONObject? = try {
        Socket().use { s ->
            s.connect(InetSocketAddress("127.0.0.1", API), 1000)
            s.soTimeout = 2000
            s.getOutputStream().write("GET $path HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n".toByteArray())
            val body = s.getInputStream().readBytes().toString(Charsets.UTF_8).substringAfter("\r\n\r\n", "")
            if (body.isEmpty()) null else JSONObject(body)
        }
    } catch (e: Exception) { null }

    private fun readN(i: InputStream, n: Int): ByteArray {
        val b = ByteArray(n)
        var off = 0
        while (off < n) {
            val r = i.read(b, off, n - off)
            if (r < 0) throw java.io.EOFException()
            off += r
        }
        return b
    }

    /** SOCKS5 greeting through the client tunnel: proves the overlay path to the exit. ms or null. */
    fun probeExit(): Long? = try {
        Socket().use { s ->
            s.connect(InetSocketAddress("127.0.0.1", CLIENT_SOCKS), 1000)
            s.soTimeout = 8000
            val t0 = System.nanoTime()
            s.getOutputStream().write(byteArrayOf(5, 1, 0))
            val r = readN(s.getInputStream(), 2)
            if (r[0].toInt() == 5 && r[1].toInt() == 0) (System.nanoTime() - t0) / 1_000_000 else null
        }
    } catch (e: Exception) { null }

    /** Full check: CONNECT host:80 via the exit and fetch /generate_204. */
    fun checkInternet(host: String = "cp.cloudflare.com"): String = try {
        Socket().use { s ->
            s.connect(InetSocketAddress("127.0.0.1", CLIENT_SOCKS), 1000)
            s.soTimeout = 15000
            val o = s.getOutputStream()
            val i = s.getInputStream()
            val t0 = System.nanoTime()
            o.write(byteArrayOf(5, 1, 0))
            val g = readN(i, 2)
            if (g[1].toInt() != 0) return "сервер отверг приветствие SOCKS"
            val tGreet = (System.nanoTime() - t0) / 1_000_000
            val h = host.toByteArray()
            o.write(byteArrayOf(5, 1, 0, 3, h.size.toByte()) + h + byteArrayOf(0, 80))
            val rep = readN(i, 4)
            if (rep[1].toInt() != 0) return "сервер: CONNECT отклонён (код ${rep[1]})"
            when (rep[3].toInt()) { 1 -> readN(i, 6); 4 -> readN(i, 18); 3 -> readN(i, (i.read() and 0xff) + 2) }
            o.write("GET /generate_204 HTTP/1.1\r\nHost: $host\r\nConnection: close\r\n\r\n".toByteArray())
            val status = i.bufferedReader().readLine() ?: return "нет ответа HTTP"
            val tAll = (System.nanoTime() - t0) / 1_000_000
            "OK: $status · рукопожатие $tGreet мс · всего $tAll мс"
        }
    } catch (e: Exception) { "нет связи: ${e.javaClass.simpleName} ${e.message ?: ""}" }

    fun mbps(bytes: Long, ms: Long) = if (ms <= 0) 0.0 else bytes * 8.0 / ms / 1000.0

    /**
     * iperf-like test to the exit node through the client tunnel (SOCKS → Yggdrasil → server).
     * `download`: the server sends for `secs`; else we send and the server reports what arrived.
     * [progress] gets intermediate Mbit/s every ~0.5 s. Returns a one-line result.
     */
    fun speedTest(download: Boolean, secs: Int, progress: (Double) -> Unit): String {
        val dir = if (download) "↓ приём" else "↑ отдача"
        return try {
            Socket().use { s ->
                s.tcpNoDelay = true
                s.connect(InetSocketAddress("127.0.0.1", CLIENT_SOCKS), 1000)
                s.soTimeout = 15000
                val o = s.getOutputStream()
                val i = s.getInputStream()
                o.write(byteArrayOf(5, 1, 0))
                if (readN(i, 2)[1].toInt() != 0) return "$dir: сервер отверг SOCKS"
                o.write(byteArrayOf(5, 1, 0, 1, 127, 0, 0, 1, (SPEEDTEST shr 8).toByte(), (SPEEDTEST and 0xff).toByte()))
                val rep = readN(i, 10)
                if (rep[1].toInt() != 0) return "$dir: сервер не поддерживает тест (код ${rep[1]}) — обновите приложение на сервере"
                o.write(byteArrayOf((if (download) 'D' else 'U').code.toByte(), secs.toByte()))
                o.flush()
                if (download) download(i, secs, progress) else upload(s, secs, progress)
            }.let { "$dir: $it" }
        } catch (e: Exception) { "$dir: ошибка: ${e.javaClass.simpleName} ${e.message ?: ""}" }
    }

    private fun download(i: InputStream, secs: Int, progress: (Double) -> Unit): String {
        val buf = ByteArray(64 * 1024)
        var total = 0L
        var t0 = 0L
        var lastTick = 0L
        var lastBytes = 0L
        while (true) {
            val n = i.read(buf)
            if (n < 0) break
            val now = System.nanoTime()
            if (t0 == 0L) { t0 = now; lastTick = now }
            total += n
            if (now - lastTick >= 500_000_000L) {
                progress(mbps(total - lastBytes, (now - lastTick) / 1_000_000))
                lastTick = now; lastBytes = total
            }
        }
        if (total == 0L) return "нет данных"
        val ms = (System.nanoTime() - t0) / 1_000_000
        return "%.1f Мбит/с · %s за %.1f с".format(mbps(total, ms), human(total), ms / 1000.0)
    }

    private fun upload(s: Socket, secs: Int, progress: (Double) -> Unit): String {
        val o = s.getOutputStream()
        val i = s.getInputStream()
        // The server answers after `secs` from our first byte: read that on a side thread
        // and keep sending until it arrives.
        val report = java.util.concurrent.atomic.AtomicReference<ByteArray?>(null)
        val failure = java.util.concurrent.atomic.AtomicReference<Exception?>(null)
        s.soTimeout = (secs + 20) * 1000
        val reader = Thread {
            try { report.set(readN(i, 16)) } catch (e: Exception) { failure.set(e) }
        }.apply { isDaemon = true; start() }
        val buf = ByteArray(64 * 1024)
        val t0 = System.nanoTime()
        var sent = 0L
        var lastTick = t0
        var lastSent = 0L
        val deadline = t0 + (secs + 20) * 1_000_000_000L
        try {
            while (report.get() == null && failure.get() == null && System.nanoTime() < deadline) {
                o.write(buf)
                sent += buf.size
                val now = System.nanoTime()
                if (now - lastTick >= 500_000_000L) {
                    progress(mbps(sent - lastSent, (now - lastTick) / 1_000_000))
                    lastTick = now; lastSent = sent
                }
            }
        } catch (e: Exception) {
            // The server closing after its report can break our last write.
        }
        reader.join(5000)
        val r = report.get() ?: return "нет отчёта сервера" + (failure.get()?.let { ": ${it.message}" } ?: "")
        val bb = java.nio.ByteBuffer.wrap(r)
        val bytes = bb.long
        val ms = bb.long
        return "%.1f Мбит/с · %s за %.1f с".format(mbps(bytes, ms), human(bytes), ms / 1000.0)
    }

    /** Download the public peer list into `dst` (atomic). */
    fun downloadPeers(dst: File) {
        val c = URL(PUBLIC_PEERS_URL).openConnection() as HttpsURLConnection
        c.connectTimeout = 15000
        c.readTimeout = 30000
        try {
            if (c.responseCode != 200) throw IllegalStateException("HTTP ${c.responseCode}")
            val body = c.inputStream.readBytes()
            val text = String(body, Charsets.UTF_8)
            if (!text.trimStart().startsWith("{") || !(text.contains("tls://") || text.contains("tcp://")))
                throw IllegalStateException("неожиданный формат списка")
            val tmp = File(dst.path + ".tmp")
            tmp.writeBytes(body)
            if (!tmp.renameTo(dst)) throw IllegalStateException("rename")
        } finally { c.disconnect() }
    }

    private fun jsonStr(s: String) = JSONObject.quote(s)

    /** tcp_node --peers file: custom peers plus the cached public list (scanner ignores structure). */
    fun writePeerFile(dst: File, custom: List<String>, publicList: File?, trackerList: File? = null) {
        val sb = StringBuilder("{\"custom\":{")
        custom.forEachIndexed { i, u ->
            if (i > 0) sb.append(',')
            sb.append(jsonStr(u)).append(":{\"up\":true}")
        }
        sb.append('}')
        if (publicList != null && publicList.isFile && publicList.length() > 2) {
            sb.append(",\"public\":").append(publicList.readText())
        }
        // Peers checked by the trackers (works where the public list's site is blocked).
        if (trackerList != null && trackerList.isFile && trackerList.length() > 2) {
            sb.append(",\"tracker\":").append(trackerList.readText())
        }
        sb.append('}')
        dst.writeText(sb.toString())
    }
}
