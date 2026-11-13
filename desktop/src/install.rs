//! `sudo yggproxy install` / `uninstall`: binary, systemd unit, group for the control socket,
//! and a menu entry for the GUI.

use std::process::Command;

const BIN: &str = "/usr/local/bin/yggproxy";
const UNIT: &str = "/etc/systemd/system/yggproxy.service";
const DESKTOP: &str = "/usr/share/applications/yggproxy.desktop";

const UNIT_TEXT: &str = "[Unit]
Description=Ratatoskr: internet through Yggdrasil (exit server and VPN client)
After=network-online.target
Wants=network-online.target

[Service]
ExecStart=/usr/local/bin/yggproxy daemon
Restart=on-failure
RestartSec=3
# SIGTERM to the daemon only: it stops tcp_node/ygghelper and undoes routing itself.
KillMode=mixed
TimeoutStopSec=15

[Install]
WantedBy=multi-user.target
";

const DESKTOP_TEXT: &str = "[Desktop Entry]
Type=Application
Name=Ratatoskr
Comment=Интернет через Yggdrasil
Exec=/usr/local/bin/yggproxy gui
Icon=network-vpn
Terminal=false
Categories=Network;
";

fn sh(args: &[&str]) -> Result<(), String> {
    let st = Command::new(args[0]).args(&args[1..]).status().map_err(|e| format!("{}: {e}", args[0]))?;
    if st.success() { Ok(()) } else { Err(format!("{} завершилась с ошибкой ({st})", args.join(" "))) }
}

fn need_root() -> Result<(), String> {
    if unsafe { libc::geteuid() } != 0 {
        return Err("нужны права root: sudo yggproxy install".into());
    }
    Ok(())
}

/// The user who ran sudo/pkexec, to add to the yggproxy group.
fn invoking_user() -> Option<String> {
    if let Ok(u) = std::env::var("SUDO_USER") {
        return Some(u).filter(|u| u != "root");
    }
    let uid: u32 = std::env::var("PKEXEC_UID").ok()?.parse().ok()?;
    let pw = unsafe { libc::getpwuid(uid) };
    if pw.is_null() {
        return None;
    }
    Some(unsafe { std::ffi::CStr::from_ptr((*pw).pw_name) }.to_string_lossy().into_owned())
}

/// Put this binary at [`BIN`] (through a temporary file: the running one may be in use).
fn copy_self() -> Result<(), String> {
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    if me != std::path::Path::new(BIN) {
        let tmp = format!("{BIN}.new");
        std::fs::copy(&me, &tmp).map_err(|e| format!("{tmp}: {e}"))?;
        std::fs::rename(&tmp, BIN).map_err(|e| format!("{BIN}: {e}"))?;
    }
    Ok(())
}

pub fn install() -> Result<(), String> {
    need_root()?;
    copy_self()?;
    std::fs::write(UNIT, UNIT_TEXT).map_err(|e| format!("{UNIT}: {e}"))?;
    if std::path::Path::new("/usr/share/applications").is_dir() {
        let _ = std::fs::write(DESKTOP, DESKTOP_TEXT);
    }
    sh(&["groupadd", "-f", "yggproxy"])?;
    let user = invoking_user();
    if let Some(u) = &user {
        sh(&["usermod", "-aG", "yggproxy", u])?;
    }
    sh(&["systemctl", "daemon-reload"])?;
    sh(&["systemctl", "enable", "yggproxy"])?;
    sh(&["systemctl", "restart", "yggproxy"])?;
    println!("Установлено: {BIN}, служба yggproxy включена и запущена.");
    if let Some(u) = user {
        println!("Пользователь {u} добавлен в группу yggproxy: управление без sudo после повторного входа в систему.");
    }
    println!("Дальше: yggproxy status · yggproxy server on · yggproxy connect АДРЕС · yggproxy gui");
    Ok(())
}

const TRACKER_UNIT: &str = "/etc/systemd/system/yggproxy-tracker.service";
const TRACKER_UNIT_TEXT: &str = "[Unit]
Description=Ratatoskr tracker: finds Yggdrasil exit nodes and serves the list (port 27081)
After=network-online.target
Wants=network-online.target

[Service]
ExecStart=/usr/local/bin/yggproxy tracker
Restart=on-failure
RestartSec=10

[Install]
WantedBy=multi-user.target
";

/// `sudo yggproxy install-tracker`: the tracker service (it runs its own Yggdrasil node).
pub fn install_tracker() -> Result<(), String> {
    need_root()?;
    copy_self()?;
    std::fs::write(TRACKER_UNIT, TRACKER_UNIT_TEXT).map_err(|e| format!("{TRACKER_UNIT}: {e}"))?;
    sh(&["systemctl", "daemon-reload"])?;
    sh(&["systemctl", "enable", "yggproxy-tracker"])?;
    sh(&["systemctl", "restart", "yggproxy-tracker"])?;
    println!("Трекер установлен и запущен: journalctl -u yggproxy-tracker -f");
    println!("У трекера свой узел Yggdrasil (yggdrasil-ng): yggdrasil-go и открытые порты не нужны.");
    println!("Его адрес — в журнале (строка «трекер … на [адрес]:27081») и в /var/lib/yggproxy/tracker-node.key (ключ).");
    Ok(())
}

pub fn uninstall() -> Result<(), String> {
    need_root()?;
    let _ = sh(&["systemctl", "disable", "--now", "yggproxy"]);
    let _ = sh(&["systemctl", "disable", "--now", "yggproxy-tracker"]);
    for p in [UNIT, TRACKER_UNIT, DESKTOP, BIN] {
        let _ = std::fs::remove_file(p);
    }
    let _ = sh(&["systemctl", "daemon-reload"]);
    println!("Служба удалена. Настройки и ключ узла остались в /etc/yggproxy и /var/lib/yggproxy.");
    Ok(())
}
