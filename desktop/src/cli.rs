//! Console commands (servers without a desktop, or a terminal on the desktop).

use crate::api::{self, dur, human, limit_text, mbit};
use crate::config;
use serde_json::{json, Value};

pub const USAGE: &str = "yggproxy — интернет через Yggdrasil (сервер выхода и VPN-клиент)

Служба:
  sudo yggproxy install        установить службу systemd (и группу yggproxy для управления)
  sudo yggproxy uninstall      удалить службу
  yggproxy daemon              запустить службу вручную (от root)

Сервер выхода:
  yggproxy server on|off       включить/выключить раздачу интернета
  yggproxy qr                  адрес сервера и QR-код для клиента
  yggproxy clients             подключённые клиенты и их трафик
  yggproxy limit               показать ограничения скорости
  yggproxy limit total|client DOWN UP
                               ограничить, Мбит/с (0 — без ограничения): весь сервер или
                               каждого клиента; применяется сразу, без обрыва соединений
  yggproxy lan on|off          пускать клиентов в локальную сеть сервера

Клиент (VPN):
  yggproxy connect АДРЕС       подключиться к серверу (200:…, или ссылка yggproxy://c?a=…)
  yggproxy disconnect          отключиться
  yggproxy speedtest           тест скорости до сервера (10 с приём, 10 с отдача)

Общее:
  yggproxy status              состояние узла, сервера и VPN
  yggproxy peers               пиры Yggdrasil и трафик через них
  yggproxy logs [-f]           журнал (с -f — следить)
  yggproxy set КЛЮЧ ЗНАЧЕНИЕ   настройка сети: rtt_limit, swap_interval, swap_settle, target,
                               lan_discovery, public_peers, peers (через запятую), udp, ipv6
  yggproxy refresh-peers       скачать публичный список пиров заново
  yggproxy gui                 окно (если собрано с GUI)";

fn print_qr(text: &str) {
    let Some((m, w)) = api::qr(text) else { return };
    let dark = |x: isize, y: isize| x >= 0 && y >= 0 && (x as usize) < w && (y as usize) < w && m[y as usize * w + x as usize];
    // Two rows per line with half blocks; light on dark terminals reads fine either way with a quiet zone.
    for y in (-2..w as isize + 2).step_by(2) {
        let mut line = String::new();
        for x in -2..w as isize + 2 {
            line.push(match (dark(x, y), dark(x, y + 1)) {
                (false, false) => '█',
                (true, false) => '▄',
                (false, true) => '▀',
                (true, true) => ' ',
            });
        }
        println!("{line}");
    }
}

fn status() -> Result<(), String> {
    let s = api::status()?;
    let n = &s["node"];
    println!("yggproxy {} · порт прокси в Yggdrasil: {}", s["version"].as_str().unwrap_or("?"), s["port"]);
    println!("Узел: {}", if n["running"].as_bool() == Some(true) { "работает" } else { "не запущен" });
    if let Some(a) = n["address"].as_str() {
        println!("  адрес: {a}");
    }
    if let Some(st) = n["stats"].as_object() {
        let node = &st["node"];
        println!(
            "  пиров {} · сессий {} · аптайм {}",
            node["peers_up"],
            node["sessions"],
            dur(st.get("uptime_s").and_then(|v| v.as_u64()).unwrap_or(0))
        );
    }
    if let Some(l) = n["lan"].as_str() {
        println!("  поиск в LAN: {l}");
    }
    let sv = &s["server"];
    if sv["enabled"].as_bool() == Some(true) {
        println!(
            "Сервер выхода: {} · соединений {} (всего {}, отказов {}) · отдано {} · принято {}",
            if sv["active"].as_bool() == Some(true) { "работает" } else { "запускается" },
            sv["active_conns"],
            sv["total"],
            sv["denied"],
            human(sv["tx"].as_u64().unwrap_or(0)),
            human(sv["rx"].as_u64().unwrap_or(0))
        );
        let l = &sv["limits"];
        println!(
            "  ограничения: сервер ↓ {} ↑ {} · клиент ↓ {} ↑ {}",
            limit_text(l["total_down"].as_u64().unwrap_or(0)),
            limit_text(l["total_up"].as_u64().unwrap_or(0)),
            limit_text(l["client_down"].as_u64().unwrap_or(0)),
            limit_text(l["client_up"].as_u64().unwrap_or(0))
        );
    } else {
        println!("Сервер выхода: выключен");
    }
    let c = &s["client"];
    if c["enabled"].as_bool() == Some(true) {
        println!(
            "VPN: {} → {} · ↓ {} ↑ {}",
            if c["active"].as_bool() == Some(true) { "работает" } else { "запускается" },
            c["server"].as_str().unwrap_or("?"),
            human(c["rx"].as_u64().unwrap_or(0)),
            human(c["tx"].as_u64().unwrap_or(0))
        );
        println!("  пинг до сервера: {}", c["server_rtt"].as_str().unwrap_or("измеряю…"));
        if let Some(sw) = c["last_swap"].as_str() {
            println!("  замена пира: {sw}");
        }
    } else {
        println!("VPN: выключен");
    }
    if let Some(e) = s["last_error"].as_str() {
        println!("Последняя ошибка: {e}");
    }
    Ok(())
}

fn peers() -> Result<(), String> {
    let s = api::status()?;
    let Some(list) = s["node"]["peers"]["peers"].as_array() else {
        println!("нет данных (узел не запущен?)");
        return Ok(());
    };
    let busiest = list.iter().map(|p| p["rx_bps"].as_u64().unwrap_or(0) + p["tx_bps"].as_u64().unwrap_or(0)).max().unwrap_or(0);
    println!(
        "Всего сейчас: ↓ {} ↑ {}",
        mbit(s["node"]["peers"]["rx_bps"].as_u64().unwrap_or(0)),
        mbit(s["node"]["peers"]["tx_bps"].as_u64().unwrap_or(0))
    );
    for p in list {
        let rate = p["rx_bps"].as_u64().unwrap_or(0) + p["tx_bps"].as_u64().unwrap_or(0);
        let uri = p["uri"].as_str().filter(|u| !u.is_empty()).map(|u| u.split('?').next().unwrap_or(u).to_string())
            .unwrap_or_else(|| format!("входящий {}", p["address"].as_str().unwrap_or("")));
        println!(
            "{} {uri}  {}  сессия {}\n    ↓ {} ↑ {} · {} % · за сессию ↓ {} ↑ {}\n    {}  {}",
            if rate == busiest && rate > 500 { "★" } else { " " },
            p["rtt_ms"].as_u64().map(|r| format!("{r} мс")).unwrap_or("—".into()),
            dur(p["up_s"].as_u64().unwrap_or(0)),
            mbit(p["rx_bps"].as_u64().unwrap_or(0)),
            mbit(p["tx_bps"].as_u64().unwrap_or(0)),
            p["share"],
            human(p["rx"].as_u64().unwrap_or(0)),
            human(p["tx"].as_u64().unwrap_or(0)),
            p["address"].as_str().unwrap_or(""),
            p["key"].as_str().unwrap_or("")
        );
    }
    Ok(())
}

fn clients() -> Result<(), String> {
    let s = api::status()?;
    let list = s["server"]["clients"].as_array().cloned().unwrap_or_default();
    if s["server"]["enabled"].as_bool() != Some(true) {
        println!("сервер выключен");
    } else if list.is_empty() {
        println!("нет подключённых клиентов");
    }
    for c in list {
        println!(
            "{}  соединений {} · отдано ↓ {} · принято ↑ {}",
            c["address"].as_str().unwrap_or("?"),
            c["active"],
            human(c["tx"].as_u64().unwrap_or(0)),
            human(c["rx"].as_u64().unwrap_or(0))
        );
    }
    Ok(())
}

fn logs(follow: bool) -> Result<(), String> {
    let mut since = 0u64;
    loop {
        let r = api::request(json!({"cmd": "logs", "since": since}))?;
        for l in r["lines"].as_array().into_iter().flatten() {
            println!("{}", l.as_str().unwrap_or(""));
        }
        since = r["seq"].as_u64().unwrap_or(since);
        if !follow {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

fn mbit_arg(s: &str) -> Result<u64, String> {
    let v: f64 = s.replace(',', ".").parse().map_err(|_| format!("не число: {s}"))?;
    if v < 0.0 {
        return Err("скорость не может быть отрицательной".into());
    }
    Ok((v * 1000.0).round() as u64)
}

fn on_off(s: Option<&String>) -> Result<bool, String> {
    match s.map(|s| s.as_str()) {
        Some("on" | "1" | "yes" | "вкл") => Ok(true),
        Some("off" | "0" | "no" | "выкл") => Ok(false),
        _ => Err("нужно on или off".into()),
    }
}

fn set(key: &str, val: &str) -> Result<(), String> {
    let num = || val.parse::<u32>().map_err(|_| format!("{key}: нужно число"));
    let flag = || on_off(Some(&val.to_string()));
    let (n, b) = (num(), flag());
    api::update(|c| {
        let net = &mut c.network;
        match key {
            "rtt_limit" => net.rtt_limit = n.clone().unwrap_or(net.rtt_limit),
            "swap_interval" => net.swap_interval = n.clone().unwrap_or(net.swap_interval),
            "swap_settle" => net.swap_settle = n.clone().unwrap_or(net.swap_settle),
            "target" => net.target = n.clone().unwrap_or(net.target),
            "lan_target" => net.lan_target = n.clone().unwrap_or(net.lan_target),
            "lan_discovery" => net.lan_discovery = b.clone().unwrap_or(net.lan_discovery),
            "public_peers" => net.public_peers = b.clone().unwrap_or(net.public_peers),
            "peers" => net.peers = val.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
            "extra_args" => net.extra_args = val.to_string(),
            "udp" => c.client.udp = b.clone().unwrap_or(c.client.udp),
            "ipv6" => c.client.ipv6 = b.clone().unwrap_or(c.client.ipv6),
            _ => {}
        }
    })?;
    let known = ["rtt_limit", "swap_interval", "swap_settle", "target", "lan_target", "lan_discovery", "public_peers", "peers", "extra_args", "udp", "ipv6"];
    if !known.contains(&key) {
        return Err(format!("неизвестная настройка {key}"));
    }
    println!("{key} = {val}");
    Ok(())
}

pub fn run(args: &[String]) -> Result<(), String> {
    let a = |i: usize| args.get(i);
    match a(0).map(|s| s.as_str()).unwrap_or("status") {
        "status" => status(),
        "peers" => peers(),
        "clients" => clients(),
        "logs" => logs(a(1).map(|s| s == "-f").unwrap_or(false)),
        "server" => {
            let on = on_off(a(1))?;
            api::update(|c| c.server.enabled = on)?;
            println!("сервер выхода {}", if on { "включён — адрес: yggproxy qr" } else { "выключен" });
            Ok(())
        }
        "lan" => {
            let on = on_off(a(1))?;
            api::update(|c| c.server.allow_lan = on)?;
            println!("доступ клиентов в LAN {}", if on { "разрешён" } else { "закрыт" });
            Ok(())
        }
        "qr" => {
            let s = api::status()?;
            let Some(addr) = s["node"]["address"].as_str() else {
                return Err("адреса ещё нет: узел не запущен (yggproxy server on)".into());
            };
            if s["server"]["enabled"].as_bool() != Some(true) {
                println!("(сервер выхода выключен: yggproxy server on)");
            }
            print_qr(&config::connect_link(addr));
            println!("Адрес сервера: {addr}\nСсылка: {}", config::connect_link(addr));
            Ok(())
        }
        "limit" => match (a(1).map(|s| s.as_str()), a(2), a(3)) {
            (None | Some("show"), _, _) => {
                let l = api::config()?.server.limits;
                println!("весь сервер: ↓ {} ↑ {}", limit_text(l.total_down), limit_text(l.total_up));
                println!("каждый клиент: ↓ {} ↑ {}", limit_text(l.client_down), limit_text(l.client_up));
                Ok(())
            }
            (Some(which @ ("total" | "client")), Some(d), Some(u)) => {
                let (d, u) = (mbit_arg(d)?, mbit_arg(u)?);
                api::update(|c| {
                    let l = &mut c.server.limits;
                    if which == "total" { (l.total_down, l.total_up) = (d, u) } else { (l.client_down, l.client_up) = (d, u) }
                })?;
                println!("{}: ↓ {} ↑ {}", if which == "total" { "весь сервер" } else { "каждый клиент" }, limit_text(d), limit_text(u));
                Ok(())
            }
            _ => Err("yggproxy limit total|client DOWN UP (Мбит/с, 0 — без ограничения)".into()),
        },
        "connect" => {
            let Some(addr) = a(1).and_then(|s| config::parse_server(s)) else {
                return Err("нужен адрес сервера 200:… или ссылка yggproxy://c?a=…".into());
            };
            api::update(|c| {
                c.client.enabled = true;
                c.client.server = addr.to_string();
            })?;
            println!("подключаюсь к {addr} (порт {}) — состояние: yggproxy status", config::PORT);
            Ok(())
        }
        "disconnect" => {
            api::update(|c| c.client.enabled = false)?;
            println!("VPN выключен");
            Ok(())
        }
        "speedtest" => {
            for down in [true, false] {
                let dir = if down { "↓ приём" } else { "↑ отдача" };
                let mut prog = |v: f64| {
                    print!("\r{dir}: {v:.1} Мбит/с …   ");
                    let _ = std::io::Write::flush(&mut std::io::stdout());
                };
                let r = api::speed_test(down, 10, &mut prog);
                println!("\r{dir}: {}                ", r.unwrap_or_else(|e| format!("ошибка: {e}")));
            }
            Ok(())
        }
        "set" => match (a(1), a(2)) {
            (Some(k), Some(v)) => set(k, v),
            _ => Err("yggproxy set КЛЮЧ ЗНАЧЕНИЕ".into()),
        },
        "refresh-peers" => api::request(json!({"cmd": "refresh_peers"})).map(|_: Value| println!("список пиров обновляется, узел перезапустится")),
        "help" | "-h" | "--help" => {
            println!("{USAGE}");
            Ok(())
        }
        other => Err(format!("неизвестная команда {other}\n\n{USAGE}")),
    }
}
