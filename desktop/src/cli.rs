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
  yggproxy qr                  адрес сервера (сервер закрыт: клиенту нужен ключ)
  yggproxy access              ключи доступа и запросы устройств
  yggproxy access new ИМЯ ДНЕЙ новый ключ (1–365 дней) — QR-код и ссылка для одного устройства
  yggproxy access qr ID        снова показать QR-код ключа
  yggproxy access approve АДРЕС ДНЕЙ [ИМЯ]   одобрить запрос устройства
  yggproxy access deny АДРЕС   отклонить запрос
  yggproxy access renew ID ДНЕЙ  продлить ключ
  yggproxy access unbind ID    отвязать ключ от устройства (для переустановки)
  yggproxy access revoke ID    удалить ключ (доступ сразу закрыт)
  yggproxy access public on|off  сделать сервер публичным (после предупреждения) или закрыть
  yggproxy clients             подключённые клиенты и их трафик
  yggproxy limit               показать ограничения скорости
  yggproxy limit total|client DOWN UP
                               ограничить, Мбит/с (0 — без ограничения): весь сервер или
                               каждого клиента; применяется сразу, без обрыва соединений
  yggproxy lan on|off          пускать клиентов в локальную сеть сервера

Клиент (VPN):
  yggproxy servers             серверы выхода: сохранённые и от трекеров, с пингом
  yggproxy connect [АДРЕС [КЛЮЧ]]  подключиться к серверу (200:… или ссылка yggproxy://c?a=…&k=…);
                               без ключа к закрытому серверу уходит запрос владельцу;
                               без адреса — к серверу с наименьшим пингом
  yggproxy request АДРЕС [СООБЩЕНИЕ]  запросить доступ к закрытому серверу (с сообщением владельцу)
  yggproxy forget АДРЕС        убрать сервер из сохранённых
  yggproxy ygg                 VPN только для сайтов Yggdrasil и ALFIS (.ygg, .anon…), без сервера
  yggproxy disconnect          отключиться
  yggproxy speedtest           тест скорости до сервера (10 с приём, 10 с отдача)

Трекеры (поиск выходных серверов в сети Yggdrasil):
  yggproxy exits [ТРЕКЕР]      список выходных серверов от трекера (напрямую)
  yggproxy refresh-trackers    спросить трекеры сейчас (служба делает это каждые 2 мин)
  yggproxy trackers [ТРЕКЕР]   известные трекеры
  sudo yggproxy install-tracker  запустить трекер на этой машине (нужен yggdrasil-go)

Общее:
  yggproxy status              состояние узла, сервера и VPN
  yggproxy peers               пиры Yggdrasil и трафик через них
  yggproxy logs [-f]           журнал (с -f — следить)
  yggproxy set КЛЮЧ ЗНАЧЕНИЕ   настройка сети: rtt_limit, swap_interval, swap_settle, target,
                               lan_discovery, public_peers, peers (через запятую), udp, ipv6,
                               trackers (через запятую), use_trackers, keep_node (узел без
                               клиента и сервера: пинги серверов и трекеры), ygg_sites (сайты
                               Yggdrasil и ALFIS рядом с интернетом), alfis (DNS ALFIS, через запятую)
  yggproxy refresh-peers       скачать публичный список пиров заново
  yggproxy update              проверить новую версию на GitHub (с sudo на Linux — установить)
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
    if let Some(v) = s["update"]["version"].as_str() {
        println!("Доступна новая версия {v}: yggproxy update");
    }
    println!("Узел: {}", if n["running"].as_bool() == Some(true) { "работает" } else { "не запущен" });
    if let Some(a) = n["address"].as_str() {
        println!("  адрес: {a}");
    }
    if let Some(st) = n["stats"].as_object() {
        let node = &st["node"];
        println!(
            "  пиров {} · сессий {} · аптайм {}",
            api::peer_count(&s),
            node["sessions"],
            dur(st.get("uptime_s").and_then(|v| v.as_u64()).unwrap_or(0))
        );
    }
    if let Some(l) = n["lan"].as_str() {
        println!("  поиск в LAN: {l}");
    }
    let tp = api::tracker_pings(&s);
    if !tp.is_empty() {
        println!("Трекеры (пинг узла):");
        for (a, l, avg) in tp {
            println!("  {a}  {}", api::ping_text(l, avg));
        }
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
        let a = &sv["access"];
        if a["public"].as_bool() == Some(true) {
            println!("  ПУБЛИЧНЫЙ: пользоваться может любой");
        } else {
            println!("  закрытый: ключей {} (действуют {}), запросов {} — yggproxy access", a["keys"], a["active"], a["requests"]);
        }
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
    if c["enabled"].as_bool() == Some(true) && c["ygg_only"].as_bool() == Some(true) {
        println!(
            "VPN: {} — только сайты Yggdrasil и ALFIS · ↓ {} ↑ {}",
            if c["active"].as_bool() == Some(true) { "работает" } else { "запускается" },
            human(c["rx"].as_u64().unwrap_or(0)),
            human(c["tx"].as_u64().unwrap_or(0))
        );
    } else if c["enabled"].as_bool() == Some(true) {
        println!(
            "VPN: {} → {} · ↓ {} ↑ {}",
            if c["active"].as_bool() == Some(true) { "работает" } else { "запускается" },
            c["server"].as_str().unwrap_or("?"),
            human(c["rx"].as_u64().unwrap_or(0)),
            human(c["tx"].as_u64().unwrap_or(0))
        );
        println!("  пинг до сервера: {}", c["server_rtt"].as_str().unwrap_or("измеряю…"));
        match c["access"].as_str() {
            Some(code) if code.starts_with("error: ") => println!("  доступ: не проверен ({})", &code[7..]),
            Some(code) => println!("  доступ: {}{}", crate::daemon::access_text(code), api::key_left(c["access_expires"].as_u64().unwrap_or(0))),
            None => println!("  доступ: проверяю…"),
        }
        if let Some(sw) = c["last_swap"].as_str() {
            println!("  замена пира: {sw}");
        }
        if c["ygg_sites"].as_bool() == Some(true) {
            println!("  сайты Yggdrasil и ALFIS (.ygg, .anon…): через свой узел");
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

/// Saved servers and the trackers' exits with the node's ping: (address, saved, ping ms).
fn server_rows(s: &Value) -> Vec<(String, bool, Option<u64>)> {
    let ping = api::ping_map(s);
    let mut rows: Vec<(String, bool, Option<u64>)> = Vec::new();
    for a in s["client"]["servers"].as_array().into_iter().flatten().filter_map(|v| v.as_str()) {
        rows.push((a.to_string(), true, ping.get(a).copied()));
    }
    for e in s["trackers"]["exits"].as_array().into_iter().flatten() {
        if let Some(a) = e["address"].as_str()
            && !rows.iter().any(|r| r.0 == a) {
                rows.push((a.to_string(), false, ping.get(a).copied()));
            }
    }
    rows
}

fn servers() -> Result<(), String> {
    let s = api::status()?;
    let cur = s["client"]["server"].as_str().unwrap_or("");
    let rows = server_rows(&s);
    if rows.is_empty() {
        println!("серверов нет: yggproxy connect АДРЕС, или дождитесь списка от трекеров");
    }
    for (a, saved, ping) in &rows {
        let mark = if a == cur && s["client"]["enabled"].as_bool() == Some(true) { "●" } else if a == cur { "○" } else { " " };
        println!("{mark} {a}  {}  {}", ping.map(|p| format!("{p} мс")).unwrap_or("нет ответа".into()), if *saved { "сохранён" } else { "от трекера" });
    }
    if let Some(r) = s["rtt"]["root"]["rtt_ms"].as_u64() {
        println!("корень сети: {r} мс");
    }
    println!("трекеры: {}", s["trackers"]["status"].as_str().unwrap_or(if s["trackers"]["enabled"].as_bool() == Some(true) { "ещё не опрашивались" } else { "выключены" }));
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

fn access(args: &[String]) -> Result<(), String> {
    let a = |i: usize| args.get(i).map(|s| s.as_str());
    let days = |s: Option<&str>| -> Result<u64, String> {
        s.and_then(|d| d.parse::<u64>().ok()).filter(|d| (1..=365).contains(d)).ok_or_else(|| "срок: от 1 до 365 дней".to_string())
    };
    let req = match a(1) {
        None | Some("list") | Some("qr") => json!({"cmd": "access", "op": "list"}),
        Some("new") => json!({"cmd": "access", "op": "new", "name": a(2).unwrap_or(""), "days": days(a(3).or(Some("30")))?}),
        Some("approve") => json!({"cmd": "access", "op": "approve", "addr": a(2).ok_or("АДРЕС")?, "days": days(a(3))?, "name": a(4).unwrap_or("")}),
        Some("deny") => json!({"cmd": "access", "op": "deny", "addr": a(2).ok_or("АДРЕС")?}),
        Some("renew") => json!({"cmd": "access", "op": "renew", "id": a(2).ok_or("ID")?, "days": days(a(3))?}),
        Some("unbind") => json!({"cmd": "access", "op": "unbind", "id": a(2).ok_or("ID")?}),
        Some("revoke") => json!({"cmd": "access", "op": "revoke", "id": a(2).ok_or("ID")?}),
        Some("public") => match a(2) {
            Some("off") => json!({"cmd": "access", "op": "public", "on": false}),
            Some("on") => {
                let r: Value = api::request(json!({"cmd": "access", "op": "list"}))?;
                println!("ВНИМАНИЕ!\n{}\n", r["risk"].as_str().unwrap_or(""));
                let accepted = args.iter().any(|x| x == "--accept-risk") || {
                    print!("Напишите «принимаю», чтобы сделать сервер публичным: ");
                    let _ = std::io::Write::flush(&mut std::io::stdout());
                    let mut l = String::new();
                    let _ = std::io::stdin().read_line(&mut l);
                    l.trim().to_lowercase() == "принимаю"
                };
                if !accepted {
                    return Err("отменено: сервер остаётся закрытым".into());
                }
                json!({"cmd": "access", "op": "public", "on": true, "accept_risk": true})
            }
            _ => return Err("yggproxy access public on|off".into()),
        },
        Some(other) => return Err(format!("неизвестная команда access {other}")),
    };
    let r = api::request(req)?;
    let now = r["now"].as_u64().unwrap_or(0);
    let keys = r["keys"].as_array().cloned().unwrap_or_default();
    let show_qr = |k: &Value| {
        if let Some(l) = k["link"].as_str() {
            print_qr(l);
            println!("Ссылка: {l}");
        }
        println!("Адрес: {}\nКлюч:  {}", r["server"].as_str().unwrap_or("? (узел не запущен)"), k["key"].as_str().unwrap_or(""));
        println!("Ключ подходит одному устройству и действует {} дн.", k["days"]);
    };
    match a(1) {
        Some("new") => {
            if let Some(k) = keys.last() {
                show_qr(k);
            }
            return Ok(());
        }
        Some("qr") => {
            let id = a(2).ok_or("yggproxy access qr ID")?;
            let k = keys.iter().find(|k| k["id"].as_str() == Some(id)).ok_or("нет такого ключа")?;
            show_qr(k);
            return Ok(());
        }
        _ => {}
    }
    println!("Сервер: {}", if r["public"].as_bool() == Some(true) { "ПУБЛИЧНЫЙ — пользоваться может любой" } else { "закрытый — только по ключам" });
    println!("Ключи ({}):", keys.len());
    for k in &keys {
        let exp = k["expires"].as_u64().unwrap_or(0);
        let left = if exp > now { format!("ещё {}", dur(exp - now)) } else { "ИСТЁК".into() };
        let dev = match k["addr"].as_str() {
            Some(ad) => format!("{} {ad}", k["device"].as_str().unwrap_or("")),
            None => "не использован".into(),
        };
        println!("  {}  {:<16} {:<12} {}", k["id"].as_str().unwrap_or(""), k["name"].as_str().unwrap_or(""), left, dev);
    }
    let reqs = r["requests"].as_array().cloned().unwrap_or_default();
    if !reqs.is_empty() {
        println!("Запросы доступа ({}) — yggproxy access approve АДРЕС ДНЕЙ:", reqs.len());
        for q in reqs {
            println!("  {}  {}  {} назад, попыток {}", q["addr"].as_str().unwrap_or(""), q["name"].as_str().unwrap_or(""),
                dur(now.saturating_sub(q["last"].as_u64().unwrap_or(0))), q["count"]);
            if let Some(m) = q["message"].as_str().filter(|m| !m.is_empty()) {
                println!("      «{m}»");
            }
        }
    }
    Ok(())
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
            "trackers" => net.trackers = val.split(',').map(|s| s.trim().to_string()).filter(|s| config::parse_server(s).is_some()).collect(),
            "use_trackers" => net.use_trackers = b.clone().unwrap_or(net.use_trackers),
            "keep_node" => net.keep_node = b.clone().unwrap_or(net.keep_node),
            "peers" => net.peers = val.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
            "extra_args" => net.extra_args = val.to_string(),
            "udp" => c.client.udp = b.clone().unwrap_or(c.client.udp),
            "ipv6" => c.client.ipv6 = b.clone().unwrap_or(c.client.ipv6),
            "ygg_sites" => c.client.ygg_sites = b.clone().unwrap_or(c.client.ygg_sites),
            "alfis" => c.client.alfis = val.split(',').map(|s| s.trim().to_string()).filter(|s| config::parse_server(s).is_some()).collect(),
            _ => {}
        }
    })?;
    let known = ["keep_node", "trackers", "use_trackers", "rtt_limit", "swap_interval", "swap_settle", "target", "lan_target", "lan_discovery", "public_peers", "peers", "extra_args", "udp", "ipv6", "ygg_sites", "alfis"];
    if !known.contains(&key) {
        return Err(format!("неизвестная настройка {key}"));
    }
    println!("{key} = {val}");
    Ok(())
}

struct SystemTimeNow;
impl SystemTimeNow {
    fn secs() -> u64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs()
    }
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
            println!("Сервер закрыт: по этой ссылке клиент только отправит запрос доступа.\nКлюч для устройства: yggproxy access new ИМЯ ДНЕЙ");
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
        "servers" => servers(),
        "forget" => {
            let Some(addr) = a(1).and_then(|s| config::parse_server(s)) else { return Err("yggproxy forget АДРЕС".into()) };
            api::update(|c| c.client.servers.retain(|x| config::parse_server(x) != Some(addr)))?;
            println!("{addr} убран из сохранённых");
            Ok(())
        }
        "access" => access(args),
        "request" => {
            let addr = a(1).and_then(|s| config::parse_server(s)).ok_or("yggproxy request АДРЕС [СООБЩЕНИЕ]")?;
            let msg = args.get(2..).map(|m| m.join(" ")).unwrap_or_default();
            let r = api::request(json!({"cmd": "request_access", "server": addr.to_string(), "message": msg}))?;
            if let Some(e) = r["error"].as_str() {
                return Err(e.to_string());
            }
            println!("{addr}: {}{}", r["text"].as_str().unwrap_or(""), api::key_left(r["expires"].as_u64().unwrap_or(0)));
            Ok(())
        }
        "connect" => {
            let key = a(2).map(|k| crate::access::norm_secret(k)).or_else(|| a(1).and_then(|l| config::link_key(l)));
            let addr = match a(1) {
                Some(s) => config::parse_server(s).ok_or("нужен адрес сервера 200:… или ссылка yggproxy://c?a=…")?,
                None => {
                    // The lowest ping among saved and tracker servers; else the tracker's first.
                    let s = api::status()?;
                    let rows = server_rows(&s);
                    let best = rows.iter().filter(|r| r.2.is_some()).min_by_key(|r| r.2).or(rows.first())
                        .ok_or("серверов пока нет: укажите адрес или дождитесь списка от трекеров (yggproxy servers)")?;
                    println!("лучший сервер: {}{}", best.0, best.2.map(|p| format!(" ({p} мс)")).unwrap_or_default());
                    best.0.parse().map_err(|_| "неверный адрес".to_string())?
                }
            };
            api::update(|c| {
                c.client.enabled = true;
                c.client.ygg_only = false;
                c.client.server = addr.to_string();
                if !c.client.servers.iter().any(|x| config::parse_server(x) == Some(addr)) {
                    c.client.servers.push(addr.to_string());
                }
                if let Some(k) = key.clone().filter(|k| !k.is_empty()) {
                    c.client.keys.insert(addr.to_string(), k);
                }
            })?;
            println!("подключаюсь к {addr} (порт {}) — состояние: yggproxy status", config::PORT);
            Ok(())
        }
        "ygg" => {
            api::update(|c| {
                c.client.enabled = true;
                c.client.ygg_only = true;
            })?;
            println!("VPN только для Yggdrasil: адреса 200::/7 и имена ALFIS (.ygg, .anon…) через свой узел, интернет как обычно");
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
        "exits" | "trackers" => {
            let what = if a(0).map(|s| s.as_str()) == Some("exits") { "exits" } else { "trackers" };
            // A given tracker, else the configured ones (with the built-in) until one answers.
            let list: Vec<String> = match a(1) {
                Some(x) => vec![config::parse_server(x).ok_or("адрес трекера: 200:…")?.to_string()],
                None => crate::tracker_client::with_defaults(&api::config().map(|c| c.network.trackers).unwrap_or_default()),
            };
            let (t, v) = crate::tracker_client::fetch_any(&list, &format!("/{what}"))?;
            let list = v[what].as_array().cloned().unwrap_or_default();
            println!("трекер {t}: {} {}", list.len(), if what == "exits" { "выходных серверов" } else { "трекеров" });
            for e in list {
                let ago = e["last_seen"].as_u64().map(|t| SystemTimeNow::secs().saturating_sub(t)).unwrap_or(0);
                println!("  {}  {}  проверен {} назад", e["address"].as_str().unwrap_or("?"),
                    e["rtt_ms"].as_u64().map(|r| format!("{r} мс")).unwrap_or_default(), dur(ago));
            }
            Ok(())
        }
        "refresh-trackers" => api::request(json!({"cmd": "refresh_trackers"})).map(|_: Value| println!("спрашиваю трекеры — список: yggproxy servers")),
        "refresh-peers" => api::request(json!({"cmd": "refresh_peers"})).map(|_: Value| println!("список пиров обновляется, узел перезапустится")),
        "help" | "-h" | "--help" => {
            println!("{USAGE}");
            Ok(())
        }
        other => Err(format!("неизвестная команда {other}\n\n{USAGE}")),
    }
}
