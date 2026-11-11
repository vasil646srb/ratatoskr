package dev.vasil.yggproxy

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent

/** Autostart after boot / app update, if enabled. VPN on boot is better done with system always-on VPN. */
class BootReceiver : BroadcastReceiver() {
    override fun onReceive(ctx: Context, intent: Intent) {
        val p = Prefs(ctx)
        if (!p.autostart || !(p.serverEnabled || p.vpnEnabled)) return
        try {
            YggService.send(ctx, YggService.ACTION_RECONCILE)
        } catch (e: Exception) {
            State.log("boot", "автозапуск не удался: ${e.message}")
        }
    }
}
