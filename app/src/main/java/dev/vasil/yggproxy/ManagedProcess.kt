package dev.vasil.yggproxy

import android.os.SystemClock
import java.io.File

/**
 * A native binary from nativeLibraryDir, kept running: output lines go to [onLine],
 * a crash is restarted with backoff (1 s .. 30 s) until [stop].
 */
class ManagedProcess(
    val tag: String,
    val cmd: List<String>,
    private val dir: File,
    private val onStart: () -> Unit = {},
    private val onExit: (Int) -> Unit = {},
    private val onLine: (String) -> Unit,
) {
    @Volatile private var stopping = false
    @Volatile private var proc: Process? = null
    @Volatile var startedAt = 0L; private set
    @Volatile var restarts = 0; private set
    private var thread: Thread? = null

    val alive: Boolean get() = proc != null

    fun start(): ManagedProcess {
        thread = Thread({ loop() }, "proc-$tag").apply { isDaemon = true; start() }
        return this
    }

    private fun loop() {
        var backoff = 1000L
        while (!stopping) {
            try {
                val pb = ProcessBuilder(cmd).directory(dir).redirectErrorStream(true)
                pb.environment()["HOME"] = dir.path
                pb.environment()["RUST_BACKTRACE"] = "1"
                val p = pb.start()
                proc = p
                startedAt = SystemClock.elapsedRealtime()
                State.log(tag, "запущен: " + cmd.joinToString(" ") { it.substringAfterLast('/') })
                onStart()
                try {
                    p.inputStream.bufferedReader().forEachLine(onLine)
                } catch (_: Exception) {
                }
                val code = p.waitFor()
                proc = null
                onExit(code)
                if (stopping) break
                State.error(tag, "процесс завершился (код $code), перезапуск через ${backoff / 1000} с")
                if (SystemClock.elapsedRealtime() - startedAt > 60_000) backoff = 1000
            } catch (e: InterruptedException) {
                break
            } catch (e: Exception) {
                proc = null
                if (stopping) break
                State.error(tag, "не запускается: ${e.message}")
            }
            restarts++
            try { Thread.sleep(backoff) } catch (_: InterruptedException) { break }
            backoff = (backoff * 2).coerceAtMost(30_000)
        }
        proc = null
    }

    /** Stop and do not restart. Waits up to 2 s for the process to exit. */
    /** [why] goes to the log, so an unexpected stop names its cause. */
    fun stop(why: String = "") {
        stopping = true
        val p = proc
        p?.destroy()
        thread?.interrupt()
        if (p != null) {
            val deadline = SystemClock.elapsedRealtime() + 2000
            while (p.isAlive && SystemClock.elapsedRealtime() < deadline) SystemClock.sleep(50)
            if (p.isAlive) p.destroyForcibly()
        }
        State.log(tag, if (why.isEmpty()) "остановлен" else "остановлен: $why")
    }
}
